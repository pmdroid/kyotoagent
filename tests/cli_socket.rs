//! The `kyotoagent` binary talking to the server's socket.
//!
//! A server is started on a socket in a temporary home, and the real binary is
//! run as a subprocess with `HOME` pointed there: `kyotoagent new` creates a session
//! for the current directory, `kyotoagent sessions` lists it, `kyotoagent log` prints
//! its log, and `kyotoagent cancel` cancels its turn. The socket is the only way
//! in, so the binary and the server never touch a TCP port.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use kyotoagent::config::Config;
use kyotoagent::server::{Server, SOCKET_FILE};

/// Run the `kyotoagent` binary with `HOME` in the test home, and return its output.
///
/// The subprocess runs on a blocking thread, so the server this test shares a
/// runtime with is not starved: a current-thread runtime would block the one
/// thread on `output()` and the server could never answer the subprocess.
async fn kyotoagent(home: &Path, args: &[&str], cwd: Option<&Path>) -> std::process::Output {
    let home = home.to_path_buf();
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let cwd = cwd.map(|dir| dir.to_path_buf());
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kyotoagent"));
        command.args(&args).env("HOME", &home);
        if let Some(dir) = &cwd {
            command.current_dir(dir);
        }
        command.output().expect("the kyotoagent binary runs")
    })
    .await
    .expect("the subprocess thread joins")
}

/// Wait until the server is answering on the socket.
async fn wait_for_socket(socket: &PathBuf) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::net::UnixStream::connect(socket).await.is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the server did not create the socket"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The subcommands create, list, read, and cancel over the socket.
#[tokio::test]
async fn the_cli_talks_to_the_socket() {
    let home = std::env::temp_dir().join(format!("kyotoagent-cli-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let root = home.join(".kyotoagent");
    fs::create_dir_all(&root).expect("the root exists");

    // A server on the socket. No turn is started here, so the model URL is a
    // placeholder: the server answers, and a turn would end with a result.
    let config =
        Config::from_toml("base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"test/model\"\n")
            .expect("the config parses");
    let server = Server::new(&root, &config).expect("the server is built");
    let handle = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    let socket = root.join(SOCKET_FILE);
    wait_for_socket(&socket).await;

    // A workspace for the session, and the current directory of `kyotoagent new`.
    let workspace = home.join("work");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    let workspace = fs::canonicalize(&workspace).unwrap_or(workspace);

    // `kyotoagent new` creates a session for the current directory.
    let output = kyotoagent(&home, &["new"], Some(&workspace)).await;
    assert!(
        output.status.success(),
        "kyotoagent new succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let created = String::from_utf8(output.stdout).expect("the output is text");
    let id = created
        .split_whitespace()
        .nth(2)
        .expect("the id is the third word")
        .to_string();
    assert!(
        created.contains(&format!("for {}", workspace.display())),
        "kyotoagent new stays in the current directory: {created}"
    );
    assert!(
        !created.contains("Where should this session work?"),
        "a new session without a terminal asks nothing: {created}"
    );
    assert!(
        !root.join("worktrees").exists(),
        "kyotoagent new without a terminal does not add a worktree"
    );

    // `kyotoagent sessions` lists the session on one plain-text row.
    let output = kyotoagent(&home, &["sessions"], None).await;
    assert!(
        output.status.success(),
        "kyotoagent sessions succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = String::from_utf8(output.stdout).expect("the output is text");
    assert!(rows.contains(&id), "the row names the session: {rows}");
    assert!(
        rows.contains(
            workspace
                .file_name()
                .expect("a name")
                .to_str()
                .expect("text")
        ),
        "the row names the workspace: {rows}"
    );

    // `kyotoagent log` prints the event log of the newest session in this directory.
    let output = kyotoagent(&home, &["log"], Some(&workspace)).await;
    assert!(
        output.status.success(),
        "kyotoagent log succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // `kyotoagent log <id>` prints the log of the session named.
    let output = kyotoagent(&home, &["log", &id], None).await;
    assert!(
        output.status.success(),
        "kyotoagent log <id> succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // `kyotoagent cancel` cancels the turn of the newest session in this directory.
    let output = kyotoagent(&home, &["cancel"], Some(&workspace)).await;
    assert!(
        output.status.success(),
        "kyotoagent cancel succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cancelled = String::from_utf8(output.stdout).expect("the output is text");
    assert!(
        cancelled.contains(&id),
        "the output names the session: {cancelled}"
    );

    handle.abort();
    let _ = fs::remove_dir_all(&home);
}

/// Without a server, the subcommands fail with the socket in the message.
#[tokio::test]
async fn without_a_server_the_cli_says_so() {
    let home = std::env::temp_dir().join(format!("kyotoagent-cli-down-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).expect("the home exists");

    let output = kyotoagent(&home, &["sessions"], None).await;
    assert!(
        !output.status.success(),
        "kyotoagent sessions fails without a server"
    );
    let stderr = String::from_utf8(output.stderr).expect("the error is text");
    assert!(
        stderr.contains("kyotoagent serve"),
        "the error says what to do: {stderr}"
    );

    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn repos_lists_the_servers_registered_repositories() {
    let home = std::env::temp_dir().join(format!("kyotoagent-cli-repos-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let root = home.join(".kyotoagent");
    fs::create_dir_all(&root).expect("root");
    let config = Config::from_toml(
        r#"base_url = "http://127.0.0.1:1/v1"
model = "test/model"
"#,
    )
    .expect("config");
    let server = Server::new(&root, &config).expect("server");
    let handle = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_socket(&root.join(SOCKET_FILE)).await;
    let output = kyotoagent(&home, &["repos"], None).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    fs::write(
        root.join("config.toml"),
        r#"base_url = "http://127.0.0.1:1/v1"
model = "test/model"
[projects.alpha]
name = "Alpha repository"
path = "/remote/alpha repo"
[projects.beta]
path = "/remote/beta"
"#,
    )
    .expect("config writes");
    for command in ["repos", "repo"] {
        let output = kyotoagent(&home, &[command], None).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).expect("text"),
            "alpha\tAlpha repository\t/remote/alpha repo\nbeta\tbeta\t/remote/beta\n"
        );
    }
    handle.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn new_uses_a_registered_repository_by_id() {
    let home = std::env::temp_dir().join(format!("kyotoagent-cli-new-repo-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let root = home.join(".kyotoagent");
    let workspace = home.join("registered repo");
    fs::create_dir_all(&root).expect("root");
    fs::create_dir_all(&workspace).expect("workspace");
    let text = format!("base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"test/model\"\n[projects.chosen]\npath = {}\n", serde_json::to_string(&workspace.display().to_string()).expect("path"));
    fs::write(root.join("config.toml"), &text).expect("config writes");
    let config = Config::from_toml(&text).expect("config");
    let server = Server::new(&root, &config).expect("server");
    let handle = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_socket(&root.join(SOCKET_FILE)).await;
    let output = kyotoagent(&home, &["new", "chosen"], Some(&home)).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(&format!("for {}", workspace.display()))
    );
    let output = kyotoagent(&home, &["new", "missing"], Some(&home)).await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown repository missing"));
    let output = kyotoagent(&home, &["sessions"], None).await;
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    handle.abort();
    let _ = fs::remove_dir_all(&home);
}
