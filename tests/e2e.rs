use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kyotoagent::chat::{parse_catalog, ChatClient, Message, ModelRow};
use kyotoagent::config::Config;
use kyotoagent::server::{Server, SOCKET_FILE};
use kyotoagent::skills;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PROBE_SECS: u64 = 5;
const IDLE_SECS: u64 = 240;

fn base_url() -> String {
    std::env::var("KYOTOAGENT_E2E_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .expect("Set KYOTOAGENT_E2E_BASE_URL to an OpenAI-compatible server URL ending in /v1")
}

fn e2e_enabled() -> bool {
    matches!(std::env::var("KYOTOAGENT_E2E"), Ok(value) if value == "1")
}

fn config_toml(base: &str, model: &str) -> String {
    format!("base_url = \"{base}\"\ntitle_model = \"\"\nmodel = \"{model}\"\n")
}

async fn probe_catalog(base: &str) -> Option<Vec<ModelRow>> {
    let url = format!("{}/models", base.trim_end_matches('/'));
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(PROBE_SECS))
        .build()
        .ok()?;
    let response = http.get(&url).send().await.ok()?;
    if !response.status().is_success() {
        eprintln!("GET {url} answered {}", response.status());
        return None;
    }
    let text = response.text().await.ok()?;
    parse_catalog(&text)
}

async fn live_target() -> Option<(String, Vec<ModelRow>)> {
    if !e2e_enabled() {
        eprintln!("KYOTOAGENT_E2E is not 1; skipping live goldbox");
        return None;
    }
    let base = base_url();
    match probe_catalog(&base).await {
        Some(rows) if !rows.is_empty() => Some((base, rows)),
        _ => {
            eprintln!("goldbox catalog at {base} is unreachable; skipping");
            None
        }
    }
}

#[test]
fn verify_kyotoagent_is_on_the_workspace_skill_index() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let skills = skills::index_in(&workspace, Path::new(""));
    let skill = skills
        .iter()
        .find(|skill| skill.name == "verify-kyotoagent")
        .expect("verify-kyotoagent is indexed from workspace .agents/skills");
    assert!(
        !skill.description.is_empty(),
        "verify-kyotoagent has a description for the picker"
    );
}

#[test]
fn verify_kyotoagent_skill_has_frontmatter_and_the_six_sections() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".agents/skills/verify-kyotoagent/SKILL.md");
    let text = fs::read_to_string(&path).expect("SKILL.md reads");
    assert!(text.starts_with("---\n"), "frontmatter opens: {text}");
    assert!(
        text.contains("name: verify-kyotoagent"),
        "frontmatter names the skill: {text}"
    );
    for heading in [
        "## Launch",
        "## Doctor",
        "## Drive",
        "## Evidence",
        "## Cleanup",
        "## Helpers",
    ] {
        assert!(text.contains(heading), "missing {heading}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn goldbox_catalog_has_an_id_and_an_advertised_length() {
    let Some((_, rows)) = live_target().await else {
        return;
    };
    let first = &rows[0];
    assert!(!first.id.is_empty(), "the first catalog row has an id");
    let length = first
        .context_length
        .expect("the first catalog row advertises a length");
    assert!(length > 0, "advertised length is positive: {length}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn goldbox_chat_replies_with_pong() {
    let Some((base, rows)) = live_target().await else {
        return;
    };
    let model = &rows[0].id;
    let config = Config::from_toml(&config_toml(&base, model)).expect("the config parses");
    let client = ChatClient::new(&config).expect("the client is built");
    let messages = vec![Message::User {
        content: "Reply with the single word pong.".into(),
    }];
    let reply = client
        .complete(&messages, &[])
        .await
        .expect("the completion comes back");
    let text = reply.text();
    assert!(
        text.to_ascii_lowercase().contains("pong"),
        "the reply contains pong: {text:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn a_serve_turn_against_goldbox_lands_a_result() {
    let Some((base, rows)) = live_target().await else {
        return;
    };
    let model = &rows[0].id;
    let root = std::env::temp_dir().join(format!(
        "kyotoagent-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after epoch")
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    let toml = config_toml(&base, model);
    fs::write(root.join(kyotoagent::config::CONFIG_FILE), &toml).expect("config.toml writes");
    let config = Config::from_toml(&toml).expect("the config parses");
    let server = Server::new(&root, &config).expect("the server is built");
    let runner = std::sync::Arc::clone(server.runner());
    let live = LiveServe {
        handle: tokio::spawn(async move {
            let _ = server.serve().await;
        }),
        runner,
        root,
    };
    let client = SocketClient {
        socket: live.root.join(SOCKET_FILE),
    };
    client.wait_for_socket().await;
    let workspace = live.root.join("workspace");
    let workspace = workspace.to_str().expect("a workspace path");
    let id = client.create_session(workspace).await;
    let (status, response) = client
        .message(&id, "Reply with the single word pong.")
        .await;
    assert_eq!(status, 202, "the turn starts: {response}");
    let view = client.wait_until_idle(&id).await;
    let cards = view["cards"].as_array().expect("cards");
    let result = cards
        .iter()
        .find(|card| card["kind"].as_str() == Some("result"))
        .expect("a result card");
    let text = result["body"]["text"].as_str().unwrap_or("");
    assert!(!text.trim().is_empty(), "the result has text: {view}");
    drop(live);
}

struct LiveServe {
    handle: tokio::task::JoinHandle<()>,
    runner: std::sync::Arc<kyotoagent::turn::Runner>,
    root: PathBuf,
}

impl Drop for LiveServe {
    fn drop(&mut self) {
        self.runner.release_all();
        self.handle.abort();
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct SocketClient {
    socket: PathBuf,
}

impl SocketClient {
    async fn wait_for_socket(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if tokio::net::UnixStream::connect(&self.socket).await.is_ok() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the server did not create the socket"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn request(&self, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
        let mut stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .expect("the socket connects");
        let body = body.unwrap_or("");
        let head = format!(
            "{method} {path} HTTP/1.1\r\nhost: kyotoagent\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .await
            .expect("the head sends");
        stream
            .write_all(body.as_bytes())
            .await
            .expect("the body sends");
        stream.flush().await.expect("the request flushes");
        let mut raw = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            if let Some(at) = head_end(&raw) {
                let length = content_length(&String::from_utf8_lossy(&raw[..at]));
                if raw.len() >= at + 4 + length {
                    break;
                }
            }
            let n = stream.read(&mut chunk).await.expect("the answer reads");
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
        }
        split_response(&String::from_utf8_lossy(&raw))
    }

    async fn create_session(&self, workspace: &str) -> String {
        let body = serde_json::json!({ "workspace": workspace }).to_string();
        let (status, response) = self.request("POST", "/v1/sessions", Some(&body)).await;
        assert_eq!(status, 201, "the session is created: {response}");
        let json: Value = serde_json::from_str(&response).expect("the reply is JSON");
        json["id"].as_str().expect("an id").to_string()
    }

    async fn view(&self, id: &str) -> Value {
        let (status, response) = self
            .request("GET", &format!("/v1/sessions/{id}/view"), None)
            .await;
        assert_eq!(status, 200, "the view reads: {response}");
        serde_json::from_str(&response).expect("the view is JSON")
    }

    async fn events(&self, id: &str) -> String {
        let (status, response) = self
            .request("GET", &format!("/v1/sessions/{id}/events"), None)
            .await;
        assert_eq!(status, 200, "the log reads: {response}");
        response
    }

    async fn message(&self, id: &str, text: &str) -> (u16, String) {
        let body = serde_json::json!({ "text": text }).to_string();
        self.request("POST", &format!("/v1/sessions/{id}/messages"), Some(&body))
            .await
    }

    async fn answer(&self, id: &str, event_id: &str, choice: &str) -> (u16, String) {
        let body = serde_json::json!({ "id": event_id, "choice": choice }).to_string();
        self.request("POST", &format!("/v1/sessions/{id}/answers"), Some(&body))
            .await
    }

    async fn open_permission_id(&self, id: &str) -> Option<String> {
        let log = self.events(id).await;
        let mut open = None;
        for line in log.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let event: Value = serde_json::from_str(line).expect("an event parses");
            match event["kind"].as_str() {
                Some("permission") => open = event["id"].as_str().map(str::to_string),
                Some("permission_answer") => open = None,
                _ => {}
            }
        }
        open
    }

    async fn wait_until_idle(&self, id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(IDLE_SECS);
        loop {
            let view = self.view(id).await;
            match view["status"].as_str() {
                Some("idle") => return view,
                Some("waiting") => {
                    if let Some(permission) = self.open_permission_id(id).await {
                        let (status, body) = self.answer(id, &permission, "allow_once").await;
                        assert!(
                            status == 204 || status == 409,
                            "allow_once: {status} {body}"
                        );
                    }
                }
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "the session did not become idle: {view}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

fn head_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(head: &str) -> usize {
    head.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok())
        .unwrap_or(0)
}

fn split_response(text: &str) -> (u16, String) {
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text, ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, body.to_string())
}
