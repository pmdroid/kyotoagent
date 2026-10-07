use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kyotoagent::config::Config;
use kyotoagent::server::{Server, SOCKET_FILE};

const ACCESS: &str = "doctor-access-token-secret";
const REFRESH: &str = "doctor-refresh-token-secret";
const API_KEY: &str = "sk-doctor-api-key-secret";

static NEXT: AtomicU64 = AtomicU64::new(0);

struct FakeModels {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeModels {
    fn start(status: u16, body: String) -> FakeModels {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the fake models server binds");
        listener
            .set_nonblocking(true)
            .expect("the listener does not block");
        let addr = listener
            .local_addr()
            .expect("the fake models server has an address");
        let stop = Arc::new(AtomicBool::new(false));
        let body = Arc::new(body);
        let handle = {
            let stop = Arc::clone(&stop);
            let body = Arc::clone(&body);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            serve_one(stream, status, &body);
                        }
                        Err(ref source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        FakeModels {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    fn listing(model: &str, length: u64) -> FakeModels {
        let body = serde_json::json!({
            "data": [{ "id": model, "context_length": length }]
        })
        .to_string();
        FakeModels::start(200, body)
    }

    fn omitting() -> FakeModels {
        let body = serde_json::json!({
            "data": [{ "id": "other-model", "context_length": 1000 }]
        })
        .to_string();
        FakeModels::start(200, body)
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }
}

impl Drop for FakeModels {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_one(mut stream: TcpStream, status: u16, body: &str) {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        if raw.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
        }
    }
    let reason = if status == 200 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn temp_home(name: &str) -> (PathBuf, PathBuf) {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let temp = if cfg!(target_os = "macos") {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let home = temp.join(format!(
        "kyotoagent-doctor-{}-{name}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&home);
    let cwd = home.join("work");
    fs::create_dir_all(home.join(".kyotoagent")).expect("the root exists");
    fs::create_dir_all(&cwd).expect("the workspace exists");
    (home, cwd)
}

fn write_grok_config(home: &Path, base_url: &str) {
    let text = format!(
        "provider = \"grok\"\n\n[providers.grok]\nbase_url = \"{base_url}\"\nmodel = \"grok-4.6\"\n"
    );
    fs::write(home.join(".kyotoagent").join("config.toml"), text).expect("the config writes");
}

fn write_local_config(home: &Path, base_url: &str, api_key_env: Option<&str>) {
    let extra = match api_key_env {
        Some(name) => format!("api_key_env = \"{name}\"\n"),
        None => String::new(),
    };
    let text = format!(
        "provider = \"local\"\n\n[providers.local]\nbase_url = \"{base_url}\"\nmodel = \"grok-4.6\"\n{extra}"
    );
    fs::write(home.join(".kyotoagent").join("config.toml"), text).expect("the config writes");
}

fn write_auth(home: &Path) {
    let text = format!(
        "{{\"access_token\":\"{ACCESS}\",\"refresh_token\":\"{REFRESH}\",\"expires_at\":\"2099-01-01T00:00:00.000Z\"}}"
    );
    fs::write(home.join(".kyotoagent").join("auth.json"), text).expect("auth.json writes");
}

fn write_closeout(cwd: &Path, text: &str) {
    fs::create_dir_all(cwd.join(".kyotoagent")).expect("the closeout dir exists");
    fs::write(cwd.join(".kyotoagent").join("closeout.yaml"), text).expect("closeout writes");
}

const VALID_CLOSEOUT: &str = "\
version: 1
items:
  - id: test
    kind: command
    run: cargo test
    hint: Fix the failing test
    paths:
      - \"src/**\"
  - id: fmt
    kind: command
    run: cargo fmt --check
    hint: Format
";

async fn wait_for_socket(socket: &Path) {
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

async fn start_serve(home: &Path) -> tokio::task::JoinHandle<()> {
    let root = home.join(".kyotoagent");
    let config =
        Config::from_toml("base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"test/model\"\n")
            .expect("the config parses");
    let server = Server::new(&root, &config).expect("the server is built");
    let handle = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_socket(&root.join(SOCKET_FILE)).await;
    handle
}

async fn doctor(home: &Path, cwd: &Path, url: Option<&str>) -> (bool, String) {
    let args = match url {
        Some(url) => vec!["doctor", "--url", url],
        None => vec!["doctor"],
    };
    let output = kyotoagent_bin(home, cwd, &args).await;
    let text = String::from_utf8(output.stdout).expect("doctor output is text");
    (output.status.success(), text)
}

fn assert_no_secrets(text: &str) {
    for needle in [ACCESS, REFRESH, API_KEY] {
        assert!(
            !text.contains(needle),
            "doctor output names a secret {needle}: {text}"
        );
    }
}

fn line<'a>(text: &'a str, name: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(name))
        .unwrap_or_else(|| panic!("missing {name} line in {text}"))
}

async fn kyotoagent_bin(home: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    let home = home.to_path_buf();
    let cwd = cwd.to_path_buf();
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
            .args(&args)
            .env("HOME", &home)
            .env_remove("KYOTOAGENT_URL")
            .env_remove("KYOTOAGENT_ROOT")
            .env_remove("EXA_API_KEY")
            .env_remove("FIRECRAWL_API_KEY")
            .current_dir(&cwd)
            .output()
            .expect("the kyotoagent binary runs")
    })
    .await
    .expect("the subprocess thread joins")
}

#[tokio::test]
async fn a_live_setup_prints_six_ok_lines() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, cwd) = temp_home("ok");
    write_grok_config(&home, &models.base_url());
    write_auth(&home);
    write_closeout(&cwd, VALID_CLOSEOUT);
    let serve = start_serve(&home).await;

    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(ok, "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("ok    config"), "{text}");
    assert!(lines[0].contains("grok"), "{text}");
    assert!(lines[0].contains("grok-4.6"), "{text}");
    assert!(lines[1].starts_with("ok    auth"), "{text}");
    assert!(lines[1].contains("auth.json"), "{text}");
    assert!(lines[2].starts_with("ok    models"), "{text}");
    assert!(lines[2].contains("GET "), "{text}");
    assert!(lines[2].contains("grok-4.6"), "{text}");
    assert!(lines[2].contains("256000"), "{text}");
    assert!(lines[3].starts_with("ok    serve"), "{text}");
    assert!(lines[3].contains("GET /v1/sessions"), "{text}");
    assert!(lines[3].contains("unix://"), "{text}");
    assert!(lines[4].starts_with("ok    closeout"), "{text}");
    assert!(lines[4].contains("2 items"), "{text}");
    assert!(text.contains("test  command  cargo test  src/**"), "{text}");
    assert!(text.contains("fmt  command  cargo fmt --check"), "{text}");
    assert!(line(&text, "skills").starts_with("ok    skills"), "{text}");
    assert!(
        line(&text, "skills").contains("listed with descriptions"),
        "{text}"
    );
    assert_no_secrets(&text);

    let output = kyotoagent_bin(&home, &cwd, &["doctor"]).await;
    let stdout = String::from_utf8(output.stdout).expect("stdout is text");
    assert!(
        output.status.success(),
        "kyotoagent doctor succeeds: {} {stdout}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("ok    "))
            .count(),
        6,
        "{stdout}"
    );
    assert_no_secrets(&stdout);

    serve.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_down_socket_fails_serve_and_keeps_the_other_checks() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, cwd) = temp_home("down");
    write_grok_config(&home, &models.base_url());
    write_auth(&home);
    write_closeout(&cwd, VALID_CLOSEOUT);

    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(!ok, "{text}");
    assert!(line(&text, "config").starts_with("ok    config"), "{text}");
    assert!(line(&text, "auth").starts_with("ok    auth"), "{text}");
    assert!(line(&text, "models").starts_with("ok    models"), "{text}");
    assert!(line(&text, "serve").starts_with("fail  serve"), "{text}");
    assert!(
        line(&text, "serve").contains("kyotoagent serve is not running"),
        "{text}"
    );
    assert!(
        line(&text, "closeout").starts_with("ok    closeout"),
        "{text}"
    );
    assert_no_secrets(&text);
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_catalog_that_omits_the_model_fails_models() {
    let models = FakeModels::omitting();
    let (home, cwd) = temp_home("omit");
    write_grok_config(&home, &models.base_url());
    write_auth(&home);
    let serve = start_serve(&home).await;

    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(!ok, "{text}");
    assert!(line(&text, "models").starts_with("fail  models"), "{text}");
    assert!(
        line(&text, "models").contains("is not in the catalog"),
        "{text}"
    );
    assert!(line(&text, "config").starts_with("ok    config"), "{text}");
    assert!(line(&text, "serve").starts_with("ok    serve"), "{text}");
    assert_no_secrets(&text);
    serve.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn grok_without_auth_asks_for_login() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, cwd) = temp_home("nologin");
    write_grok_config(&home, &models.base_url());
    let serve = start_serve(&home).await;

    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(!ok, "{text}");
    assert!(line(&text, "auth").starts_with("fail  auth"), "{text}");
    assert!(
        line(&text, "auth").contains("Open Providers in Kyoto Agent to sign in."),
        "{text}"
    );
    assert!(line(&text, "config").starts_with("ok    config"), "{text}");
    assert!(line(&text, "serve").starts_with("ok    serve"), "{text}");
    assert_no_secrets(&text);
    serve.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_duplicate_closeout_id_fails_parse() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, cwd) = temp_home("dup");
    write_grok_config(&home, &models.base_url());
    write_auth(&home);
    write_closeout(
        &cwd,
        "version: 1\nitems:\n  - id: test\n    kind: command\n    run: a\n    hint: a\n  - id: test\n    kind: command\n    run: b\n    hint: b\n",
    );
    let serve = start_serve(&home).await;

    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(!ok, "{text}");
    assert!(
        line(&text, "closeout").starts_with("fail  closeout"),
        "{text}"
    );
    assert!(line(&text, "closeout").contains("appears twice"), "{text}");
    serve.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_visual_kind_fails_closeout() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, cwd) = temp_home("visual");
    write_grok_config(&home, &models.base_url());
    write_auth(&home);
    write_closeout(
        &cwd,
        "version: 1\nitems:\n  - id: shots\n    kind: visual\n    run: capture\n    hint: Fix the shots\n",
    );
    let serve = start_serve(&home).await;

    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(!ok, "{text}");
    assert!(
        line(&text, "closeout").starts_with("fail  closeout"),
        "{text}"
    );
    assert!(line(&text, "closeout").contains("visual"), "{text}");
    serve.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_missing_closeout_file_is_ok() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, cwd) = temp_home("noclose");
    write_grok_config(&home, &models.base_url());
    write_auth(&home);
    let serve = start_serve(&home).await;

    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(ok, "{text}");
    assert!(
        line(&text, "closeout").contains("no closeout file"),
        "{text}"
    );
    assert!(
        line(&text, "closeout").starts_with("ok    closeout"),
        "{text}"
    );
    serve.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_malformed_closeout_file_fails() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, cwd) = temp_home("badyaml");
    write_grok_config(&home, &models.base_url());
    write_auth(&home);
    write_closeout(&cwd, "version: 1\nitems: [");
    let serve = start_serve(&home).await;

    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(!ok, "{text}");
    assert!(
        line(&text, "closeout").starts_with("fail  closeout"),
        "{text}"
    );
    serve.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_provider_with_no_key_skips_auth() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, cwd) = temp_home("nokey");
    write_local_config(&home, &models.base_url(), None);
    let serve = start_serve(&home).await;

    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(ok, "{text}");
    assert!(line(&text, "auth").contains("no auth"), "{text}");
    serve.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_missing_api_key_env_fails_auth() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, cwd) = temp_home("keyenv");
    write_local_config(
        &home,
        &models.base_url(),
        Some("KYOTOAGENT_DOCTOR_MISSING_KEY"),
    );
    std::env::remove_var("KYOTOAGENT_DOCTOR_MISSING_KEY");
    let serve = start_serve(&home).await;

    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(!ok, "{text}");
    assert!(line(&text, "auth").starts_with("fail  auth"), "{text}");
    assert!(
        line(&text, "auth").contains("KYOTOAGENT_DOCTOR_MISSING_KEY is unset"),
        "{text}"
    );
    serve.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_non_https_url_fails_serve() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, cwd) = temp_home("httpurl");
    write_grok_config(&home, &models.base_url());
    write_auth(&home);

    let (ok, text) = doctor(&home, &cwd, Some("http://127.0.0.1:1")).await;
    assert!(!ok, "{text}");
    assert!(line(&text, "serve").starts_with("fail  serve"), "{text}");
    assert!(line(&text, "serve").contains("https"), "{text}");
    assert!(line(&text, "config").starts_with("ok    config"), "{text}");
    assert_no_secrets(&text);
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_missing_opencode_key_is_reported_and_not_printed() {
    let (home, cwd) = temp_home("oc-missing");
    let secret = "oc-doctor-secret-value";
    fs::write(
        home.join(".kyotoagent").join("config.toml"),
        "provider = \"opencode\"\n\n[providers.opencode]\nkind = \"opencode\"\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"kimi-k2.7-code\"\n",
    )
    .expect("the config writes");
    let (ok, text) = doctor(&home, &cwd, None).await;
    assert!(!ok);
    let auth = line(&text, "auth");
    assert!(
        auth.contains("OpenCode API key is missing"),
        "doctor names the missing key"
    );
    assert!(!text.contains(secret), "doctor printed a key");
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_present_opencode_key_is_not_printed() {
    let (home, cwd) = temp_home("oc-present");
    let secret = "oc-doctor-present-secret";
    fs::write(
        home.join(".kyotoagent").join("config.toml"),
        "provider = \"opencode\"\n\n[providers.opencode]\nkind = \"opencode\"\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"kimi-k2.7-code\"\n",
    )
    .expect("the config writes");
    kyotoagent::auth::write_opencode_key(
        &home.join(".kyotoagent").join("opencode-auth.json"),
        secret,
    )
    .expect("the key writes");
    let (_ok, text) = doctor(&home, &cwd, None).await;
    let auth = line(&text, "auth");
    assert!(auth.starts_with("ok"), "auth did not pass");
    assert!(auth.contains("opencode-auth.json"), "auth names the file");
    assert!(!text.contains(secret), "doctor printed the key");
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn systemprompt_prints_the_workspace_prompt() {
    let (home, cwd) = temp_home("prompt");
    fs::write(cwd.join("AGENTS.md"), "Ship the small change.\n").expect("agents writes");
    let output = kyotoagent_bin(&home, &cwd, &["systemprompt"]).await;
    let stdout = String::from_utf8(output.stdout).expect("prompt is text");
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "You are Kyoto Agent, a coding agent working in {}.",
            cwd.display()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("Ship the small change."), "{stdout}");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn help_names_doctor() {
    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .arg("--help")
        .output()
        .expect("the kyotoagent binary runs");
    let stdout = String::from_utf8(output.stdout).expect("help is text");
    assert!(stdout.contains("doctor"), "{stdout}");
    assert!(stdout.contains("systemprompt"), "{stdout}");
    assert!(
        stdout.contains("Print the system prompt with systemprompt"),
        "{stdout}"
    );
    assert!(stdout.contains("Kyoto Agent"), "{stdout}");
}

#[tokio::test]
async fn the_repo_closeout_file_is_ok() {
    let models = FakeModels::listing("grok-4.6", 256000);
    let (home, _) = temp_home("repo");
    write_local_config(&home, &models.base_url(), None);
    let serve = start_serve(&home).await;
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let output = kyotoagent_bin(&home, &repo, &["doctor"]).await;
    let stdout = String::from_utf8(output.stdout).expect("stdout is text");
    assert!(
        output.status.success(),
        "kyotoagent doctor succeeds: {} {stdout}",
        String::from_utf8_lossy(&output.stderr)
    );
    let closeout = line(&stdout, "closeout");
    assert!(closeout.starts_with("ok    closeout"), "{stdout}");
    assert!(!closeout.contains("no closeout file"), "{stdout}");
    assert!(closeout.contains("3 items"), "{stdout}");
    assert!(
        stdout.contains("cargo-test  command  cargo test --offline"),
        "{stdout}"
    );
    assert!(
        stdout.contains("cargo-fmt  command  cargo fmt --check"),
        "{stdout}"
    );
    assert!(
        stdout
            .contains("cargo-clippy  command  cargo clippy --all-targets --offline -- -D warnings"),
        "{stdout}"
    );

    serve.abort();
    let _ = fs::remove_dir_all(&home);
}
