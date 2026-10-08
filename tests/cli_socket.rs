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
use kyotoagent::events::{Event, EventKind, TaskStartBody};
use kyotoagent::screen::Status;
use kyotoagent::server::{Server, SESSIONS_DIR, SOCKET_FILE};
use kyotoagent::session::{Session, SessionMeta};

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

/// Two processes starting together leave one owner, and a stale socket still restarts.
#[tokio::test]
async fn simultaneous_startup_has_one_owner_and_a_stale_socket_recovers() {
    let home = std::env::temp_dir().join(format!(
        "kyotoagent-cli-owner-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&home);
    let root = home.join(".kyotoagent");
    let workspace = home.join("work");
    fs::create_dir_all(&root).expect("the root exists");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    fs::write(
        root.join("config.toml"),
        "base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"test/model\"\n",
    )
    .expect("the config writes");
    let session = Session::at(&root.join(SESSIONS_DIR).join("restarted"));
    session
        .create(&SessionMeta::new(
            "restarted",
            &workspace,
            "test/model",
            "2026-10-08T00:00:00.000Z",
        ))
        .expect("the session exists");
    session
        .update(|meta| {
            meta.status = Status::Working;
            true
        })
        .expect("the session is working");
    session
        .append(
            &Event::new("e1", "2026-10-08T00:00:01.000Z", "t1", EventKind::TaskStart)
                .with_body(&TaskStartBody {
                    id: "task-stale".to_string(),
                    argv: vec!["sleep".to_string(), "30".to_string()],
                })
                .expect("the task start encodes"),
        )
        .expect("the task start is logged");
    fs::write(root.join(SOCKET_FILE), b"").expect("a stale socket file exists");
    let marker = root.join("owners.txt");
    let binary = env!("CARGO_BIN_EXE_kyotoagent");
    let script_for = |name: &str| {
        format!(
            "set -e; \"{binary}\" serve >\"{log}\" 2>&1 & pid=$!; for _ in $(seq 1 80); do if ! kill -0 \"$pid\" 2>/dev/null; then break; fi; if [ -S \"{socket}\" ]; then echo \"$pid\" >> \"{marker}\"; kill \"$pid\"; wait \"$pid\" || true; exit 0; fi; sleep 0.05; done; kill \"$pid\" || true; wait \"$pid\" || true; exit 1",
            binary = binary,
            log = root.join(name).display(),
            socket = root.join(SOCKET_FILE).display(),
            marker = marker.display(),
        )
    };
    let launch = |home: PathBuf, script: String| {
        tokio::task::spawn_blocking(move || {
            Command::new("sh")
                .arg("-c")
                .arg(&script)
                .env("HOME", &home)
                .output()
                .expect("startup runs")
        })
    };
    let first = launch(home.clone(), script_for("race-a.log"));
    let second = launch(home.clone(), script_for("race-b.log"));
    let (first, second) = tokio::join!(first, second);
    let first = first.expect("the first startup joins");
    let second = second.expect("the second startup joins");
    let owners = fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(
        owners.lines().filter(|line| !line.is_empty()).count(),
        1,
        "one process owns startup: {owners}\n{:?}\n{:?}",
        first.status,
        second.status
    );
    let logs = format!(
        "{}\n{}",
        fs::read_to_string(root.join("race-a.log")).unwrap_or_default(),
        fs::read_to_string(root.join("race-b.log")).unwrap_or_default()
    );
    assert!(
        logs.contains("already running"),
        "the losing process is refused: {logs}"
    );

    let events = fs::read_to_string(session.events_path()).expect("events");
    assert!(
        events.contains("The server stopped."),
        "the genuine restart recovers the dead turn: {events}"
    );
    assert!(
        events.contains("\"state\":\"stopped\""),
        "the genuine restart settles the dead task: {events}"
    );
    let _ = fs::remove_dir_all(&home);
}
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
