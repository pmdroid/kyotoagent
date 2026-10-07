use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kyotoagent::config::Config;
use kyotoagent::events::EventKind;
use kyotoagent::screen::Status;
use kyotoagent::server::{Server, SOCKET_FILE};
use kyotoagent::session::Session;
use kyotoagent::turn::Runner;
use kyotoagent::view;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
enum Canned {
    Json(String),
    Status(u16, String),
}

struct FakeServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    bodies: Arc<Mutex<Vec<String>>>,
}

impl FakeServer {
    fn start(catalog: Value, replies: Vec<Canned>, compact_delay: Duration) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the fake server binds");
        listener
            .set_nonblocking(true)
            .expect("the listener does not block the thread");
        let addr = listener
            .local_addr()
            .expect("the fake server has an address");
        let replies = Arc::new(Mutex::new(replies));
        let catalog = Arc::new(catalog);
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let replies = Arc::clone(&replies);
            let catalog = Arc::clone(&catalog);
            let bodies = Arc::clone(&bodies);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            serve_one(stream, &replies, &catalog, &bodies, compact_delay);
                        }
                        Err(ref source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        FakeServer {
            addr,
            stop,
            handle: Some(handle),
            bodies,
        }
    }

    fn base_url(&self) -> String {
        let mut url = String::from("http:");
        url.push('/');
        url.push('/');
        url.push_str(&self.addr.to_string());
        url
    }

    fn chat_bodies(&self) -> Vec<Value> {
        self.bodies
            .lock()
            .expect("the bodies are not poisoned")
            .iter()
            .filter_map(|body| serde_json::from_str(body).ok())
            .collect()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_one(
    mut stream: TcpStream,
    replies: &Arc<Mutex<Vec<Canned>>>,
    catalog: &Value,
    bodies: &Arc<Mutex<Vec<String>>>,
    compact_delay: Duration,
) {
    let Some((path, body)) = read_request(&mut stream) else {
        return;
    };
    if path.contains("/models") {
        let reply = if path.trim_end_matches('/').ends_with("/models") {
            catalog.clone()
        } else {
            let id = path.rsplit('/').next().unwrap_or("");
            catalog
                .get("data")
                .and_then(Value::as_array)
                .and_then(|rows| {
                    rows.iter()
                        .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
                        .cloned()
                })
                .unwrap_or_else(|| serde_json::json!({ "id": id }))
        };
        respond(&mut stream, 200, &reply.to_string());
        return;
    }
    bodies
        .lock()
        .expect("the bodies are not poisoned")
        .push(body.clone());
    let compact = serde_json::from_str::<Value>(&body)
        .ok()
        .is_some_and(|json| json.get("tools").is_none());
    if compact && compact_delay > Duration::ZERO {
        std::thread::sleep(compact_delay);
    }
    let reply = {
        let mut queue = replies.lock().expect("the queue is not poisoned");
        if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue
                .first()
                .cloned()
                .unwrap_or(Canned::Status(500, "no reply left".into()))
        }
    };
    match reply {
        Canned::Json(reply) => kyotoagent::chat::answer_completion(&mut stream, 200, &reply, &body),
        Canned::Status(status, body) => respond(&mut stream, status, &body),
    }
}

fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 1024];
    let head_end = loop {
        if let Some(at) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break at;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let mut lines = head.lines();
    let mut start = lines.next()?.split_whitespace();
    let _method = start.next()?;
    let path = start.next()?.to_string();
    let mut length = 0;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = raw[head_end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    Some((path, String::from_utf8_lossy(&body).into_owned()))
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn text_reply(text: &str, prompt_tokens: u64) -> String {
    serde_json::json!({
        "choices": [{ "message": { "role": "assistant", "content": text } }],
        "usage": { "prompt_tokens": prompt_tokens }
    })
    .to_string()
}

fn summary_reply(text: &str) -> String {
    text_reply(
        &format!(
            "{text}\n{}",
            "Verified task state and pending requirements. ".repeat(12)
        ),
        20,
    )
}

fn tool_reply(name: &str, args: Value, prompt_tokens: u64) -> String {
    serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": name, "arguments": args.to_string() }
                }]
            }
        }],
        "usage": { "prompt_tokens": prompt_tokens }
    })
    .to_string()
}

fn catalog(length: Option<u64>) -> Value {
    let mut row = serde_json::json!({ "id": "grok-4.6" });
    if let Some(length) = length {
        row["context_length"] = Value::from(length);
    }
    serde_json::json!({ "data": [row] })
}

const AT: &str = "2026-09-29T00:00:00.000Z";

struct Fixture {
    runner: Arc<Runner>,
    server: FakeServer,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.runner.release_all();
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(name: &str, catalog: Value, replies: Vec<Canned>, delay: Duration) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("kyotoagent-compact-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let server = FakeServer::start(catalog, replies, delay);
        let config = Config::from_toml(&format!(
            "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"grok-4.6\"\n",
            server.base_url()
        ))
        .expect("the config parses");
        let runner = Runner::new(&config).expect("the runner is built");
        Fixture {
            runner,
            server,
            root,
        }
    }

    fn with_provider_window(
        name: &str,
        catalog: Value,
        replies: Vec<Canned>,
        window: u64,
    ) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("kyotoagent-compact-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let server = FakeServer::start(catalog, replies, Duration::ZERO);
        let config = Config::from_toml(&format!(
            "provider = \"office\"\ntitle_model = \"\"\n\n[providers.office]\nbase_url = \"{}\"\nmodel = \"grok-4.6\"\ncontext_window = {window}\n",
            server.base_url()
        ))
        .expect("the config parses");
        let runner = Runner::new(&config).expect("the runner is built");
        Fixture {
            runner,
            server,
            root,
        }
    }

    fn add_session(&self, id: &str) -> PathBuf {
        let workspace = self.root.join(format!("w-{id}"));
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let session = kyotoagent::session::Session::at(&self.root.join(format!("session-{id}")));
        session
            .create(&kyotoagent::session::SessionMeta::new(
                id, &workspace, "grok-4.6", AT,
            ))
            .expect("the session is created");
        self.runner
            .add_session(&session)
            .expect("the session is added");
        workspace
    }

    fn view(&self, id: &str) -> view::View {
        self.runner.view(id).expect("the view reads")
    }

    fn prior_history(&self, id: &str, text: &str) {
        let session = Session::at(&self.root.join(format!("session-{id}")));
        for (event_id, kind, body) in [
            ("e1", EventKind::UserAsk, serde_json::json!({"text": text})),
            (
                "e2",
                EventKind::Result,
                serde_json::json!({"text": "old work done"}),
            ),
        ] {
            session
                .append(
                    &kyotoagent::events::Event::new(event_id, AT, "t0", kind)
                        .with_body(&body)
                        .unwrap(),
                )
                .unwrap();
        }
        session
            .update(|meta| {
                meta.prompt_tokens = Some(100);
                true
            })
            .unwrap();
    }

    async fn wait_for_status(&self, id: &str, status: Status) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.view(id).status == status {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the session did not reach {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_for_cards(&self, id: &str, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.view(id).cards.len() >= count {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the session did not reach {count} cards"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_compacting(&self, id: &str, want: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.runner.is_compacting(id) == want {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "compacting did not become {want}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn events(&self, id: &str) -> Vec<kyotoagent::events::Event> {
        Session::at(&self.root.join(format!("session-{id}")))
            .events()
            .expect("the log reads")
    }

    fn chat(&self) -> Vec<Value> {
        self.server.chat_bodies()
    }
}

fn user_contents(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str().map(str::to_string))
        .collect()
}

fn is_compact_body(body: &Value) -> bool {
    body.get("tools").is_none()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compaction_fits_oversized_history_before_sending() {
    let fixture = Fixture::new(
        "uncached-manual-capacity",
        catalog(Some(10000)),
        vec![Canned::Json(summary_reply("bounded history summary"))],
        Duration::from_millis(250),
    );
    fixture.add_session("91bc");
    fixture.prior_history("91bc", &"h".repeat(80000));
    fixture.runner.compact("91bc").unwrap();
    fixture.wait_compacting("91bc", true).await;
    fixture.wait_compacting("91bc", false).await;
    let posts = fixture.chat();
    assert!(is_compact_body(&posts[0]));
    assert!(posts[0]["messages"].to_string().len() < 20000);
    assert!(fixture
        .events("91bc")
        .iter()
        .any(|event| event.kind == EventKind::Compact));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_history_is_fitted_before_compaction_and_inference() {
    let fixture = Fixture::new(
        "oversized-history",
        catalog(Some(10000)),
        vec![Canned::Json(summary_reply("bounded history summary"))],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture.prior_history("91bc", &"h".repeat(80000));
    fixture
        .runner
        .ask("91bc", "Keep this current request")
        .unwrap();
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let posts = fixture.chat();
    assert!(is_compact_body(&posts[0]));
    assert!(posts[0]["messages"].to_string().len() < 20000);
    assert!(fixture
        .events("91bc")
        .iter()
        .any(|event| event.kind == EventKind::Compact));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_summary_that_enlarges_history_stops_without_replacing_history() {
    let fixture = Fixture::new(
        "expanding-summary",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "list_dir",
                serde_json::json!({"path": "."}),
                90000,
            )),
            Canned::Json(text_reply(&"oversized summary ".repeat(2000), 20)),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({"text": "done", "proof": "checked"}),
                50,
            )),
        ],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture.prior_history("91bc", "older requirements still readable");
    fixture
        .runner
        .ask("91bc", "Keep this current request")
        .unwrap();
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(fixture
        .events("91bc")
        .iter()
        .all(|event| event.kind != EventKind::Compact));
    assert_eq!(
        fixture
            .chat()
            .iter()
            .filter(|body| is_compact_body(body))
            .count(),
        1
    );
    assert!(fixture
        .events("91bc")
        .iter()
        .any(|event| event.kind == EventKind::Result
            && event.body["text"]
                .as_str()
                .is_some_and(|text| text.contains("did not reduce context usage"))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_request_stops_before_inference_and_keeps_the_ask() {
    let fixture = Fixture::new(
        "oversized-request",
        catalog(Some(512)),
        vec![Canned::Json(text_reply("unexpected model call", 1))],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture
        .runner
        .ask("91bc", "Keep this exact request")
        .unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(fixture.chat().is_empty());
    assert!(fixture
        .events("91bc")
        .iter()
        .any(|event| event.kind == EventKind::UserAsk
            && event.body["text"] == "Keep this exact request"));
    assert!(fixture
        .view("91bc")
        .cards
        .iter()
        .any(|card| card.kind == view::CardKind::Result
            && card.body["text"]
                .as_str()
                .is_some_and(|text| text.contains("context limit 512"))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_goal_pauses_with_the_context_limit_reason() {
    let fixture = Fixture::new(
        "oversized-goal",
        catalog(Some(512)),
        vec![Canned::Json(text_reply("unexpected model call", 1))],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture
        .runner
        .ask("91bc", "/goal Check the workspace")
        .unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let goal = Session::at(&fixture.root.join("session-91bc"))
        .meta()
        .unwrap()
        .goal
        .unwrap();
    assert_eq!(goal.status, kyotoagent::goal::GoalStatus::Paused);
    assert!(goal.verification.contains("context limit 512"));
    assert!(fixture.chat().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compaction_preserves_a_running_ask_and_its_permission() {
    let fixture = Fixture::new(
        "manual-during-permission",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "write_file",
                serde_json::json!({"path": "notes.md", "contents": "hello"}),
                100,
            )),
            Canned::Json(summary_reply("older task summary")),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({"text": "done", "proof": "checked"}),
                50,
            )),
        ],
        Duration::from_millis(250),
    );
    let workspace = fixture.add_session("91bc");
    fixture.prior_history(
        "91bc",
        &"An older task with enough readable background to summarize accurately. ".repeat(40),
    );
    fixture
        .runner
        .ask("91bc", "Create notes.md without losing this request")
        .unwrap();
    fixture.wait_for_status("91bc", Status::Waiting).await;
    fixture.runner.compact("91bc").unwrap();
    assert!(!fixture.runner.is_compacting("91bc"));
    assert!(fixture.chat().iter().all(|body| !is_compact_body(body)));
    fixture
        .runner
        .answer("91bc", kyotoagent::permit::Answer::allow_once())
        .unwrap();
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let posted = fixture
        .chat()
        .into_iter()
        .rfind(|body| !is_compact_body(body))
        .unwrap();
    assert!(user_contents(&posted)
        .iter()
        .any(|text| text.contains("Create notes.md without losing this request")));
    assert!(user_contents(&posted)
        .iter()
        .any(|text| text.contains("older task summary")));
    assert!(user_contents(&posted)
        .iter()
        .all(|text| !text.contains("An older task with enough readable background")));
    assert_eq!(
        fs::read_to_string(workspace.join("notes.md")).unwrap(),
        "hello"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_new_ask_without_older_history_is_not_summarized() {
    let fixture = Fixture::new(
        "single-large-ask",
        catalog(Some(32000)),
        vec![Canned::Json(tool_reply(
            "finish",
            serde_json::json!({ "text": "done", "proof": "checked" }),
            100,
        ))],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    let fixed_tokens = fixture.view("91bc").context.unwrap().used;
    let request = "x".repeat(((30_000 - fixed_tokens) * 4) as usize);
    fixture.runner.ask("91bc", &request).unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let posts = fixture.chat();
    assert_eq!(posts.len(), 1);
    assert!(!is_compact_body(&posts[0]));
    assert!(user_contents(&posts[0])
        .iter()
        .any(|text| text.contains(&format!("<user_query>\n{request}\n</user_query>"))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_new_ask_compacts_before_its_first_completion() {
    let fixture = Fixture::new(
        "large-new-ask",
        catalog(Some(64000)),
        vec![
            Canned::Json(summary_reply("Keep the new request requirement")),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({ "text": "done", "proof": "checked" }),
                50,
            )),
        ],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture.prior_history("91bc", &"h".repeat(180000));
    let request = "new request requirement ".repeat(2000);
    fixture.runner.ask("91bc", &request).unwrap();
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let posts = fixture.chat();
    assert!(
        is_compact_body(&posts[0]),
        "compact before inference: {posts:?}"
    );
    assert!(posts.iter().skip(1).any(|body| !is_compact_body(body)));
    assert!(user_contents(&posts[1])
        .iter()
        .any(|text| text == &format!("<user_query>\n{request}\n</user_query>")));
    assert!(fixture
        .events("91bc")
        .iter()
        .any(|event| event.kind == EventKind::Compact));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_tool_output_compacts_before_the_next_completion() {
    let fixture = Fixture::new(
        "large-tool-output",
        catalog(Some(64000)),
        vec![
            Canned::Json(tool_reply(
                "read_file",
                serde_json::json!({ "path": "large.txt" }),
                100,
            )),
            Canned::Json(summary_reply("Keep the request and the file findings")),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({ "text": "done", "proof": "checked" }),
                50,
            )),
        ],
        Duration::ZERO,
    );
    let workspace = fixture.add_session("91bc");
    fixture.prior_history("91bc", &"h".repeat(120000));
    fs::write(workspace.join("large.txt"), "e".repeat(32768)).unwrap();
    let request = "x".repeat(60000);
    fixture.runner.ask("91bc", &request).unwrap();
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let posts = fixture.chat();
    assert!(!is_compact_body(&posts[0]), "first inference fits");
    assert!(
        is_compact_body(&posts[1]),
        "tool output requires compaction"
    );
    assert!(posts.iter().skip(2).any(|body| !is_compact_body(body)));
    assert!(user_contents(&posts[2])
        .iter()
        .any(|text| text == &format!("<user_query>\n{request}\n</user_query>")));
    assert!(posts[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|message| message["role"] != "tool"));
    assert!(user_contents(&posts[2])
        .iter()
        .any(|text| text.contains("file findings")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advertised_hundred_thousand_tokens_compacts_before_the_next_complete() {
    let fixture = Fixture::new(
        "thousand",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "list_dir",
                serde_json::json!({ "path": "." }),
                90000,
            )),
            Canned::Json(summary_reply("files and unfinished work")),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({ "text": "done", "proof": "cargo test passed." }),
                50,
            )),
        ],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture.prior_history("91bc", &"older request context ".repeat(100));
    fixture
        .runner
        .ask("91bc", "first ask")
        .expect("the turn starts");
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let posts = fixture.chat();
    assert!(posts.len() >= 3, "complete, compact, complete: {posts:?}");
    assert!(!is_compact_body(&posts[0]), "the first call is a complete");
    assert!(
        user_contents(&posts[0])
            .iter()
            .any(|text| text.contains("first ask")),
        "the first complete has the ask: {}",
        posts[0]
    );
    let compact = posts
        .iter()
        .find(|body| is_compact_body(body))
        .expect("a compact call ran");
    assert!(compact.get("tools").is_none(), "{compact}");
    let second = posts
        .iter()
        .rev()
        .find(|body| !is_compact_body(body))
        .expect("a later complete");
    let users = user_contents(second);
    assert!(
        users
            .iter()
            .any(|text| text.contains("files and unfinished work")),
        "the later complete has the summary: {second}"
    );
    assert!(
        users
            .iter()
            .any(|text| text == "<user_query>\nfirst ask\n</user_query>"),
        "the active ask remains intact: {second}"
    );
    assert!(
        fixture
            .events("91bc")
            .iter()
            .any(|event| event.kind == EventKind::Compact),
        "the compact event is in the log"
    );
    assert!(fixture
        .view("91bc")
        .cards
        .iter()
        .all(|card| format!("{:?}", card.kind) != "Compact"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advertised_two_hundred_thousand_skips_compact() {
    let fixture = Fixture::new(
        "skip",
        catalog(Some(200000)),
        vec![
            Canned::Json(tool_reply(
                "list_dir",
                serde_json::json!({ "path": "." }),
                90000,
            )),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({ "text": "done", "proof": "cargo test passed." }),
                90000,
            )),
        ],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture
        .runner
        .ask("91bc", "first ask")
        .expect("the turn starts");
    fixture.wait_for_cards("91bc", 1).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let posts = fixture.chat();
    assert!(
        posts.iter().all(|body| !is_compact_body(body)),
        "no compact call: {posts:?}"
    );
    assert!(fixture
        .events("91bc")
        .iter()
        .all(|event| event.kind != EventKind::Compact));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_catalog_length_starts_no_compact() {
    let fixture = Fixture::new(
        "missing",
        catalog(None),
        vec![
            Canned::Json(tool_reply(
                "list_dir",
                serde_json::json!({ "path": "." }),
                90000,
            )),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({ "text": "done", "proof": "cargo test passed." }),
                90000,
            )),
        ],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture
        .runner
        .ask("91bc", "first ask")
        .expect("the turn starts");
    fixture.wait_for_cards("91bc", 1).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(
        fixture.chat().iter().all(|body| !is_compact_body(body)),
        "no compact without a window"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_window_is_used_when_the_catalog_omits_length() {
    let fixture = Fixture::with_provider_window(
        "provider-window",
        catalog(None),
        vec![
            Canned::Json(tool_reply(
                "list_dir",
                serde_json::json!({ "path": "." }),
                90000,
            )),
            Canned::Json(summary_reply("provider summary")),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({ "text": "done", "proof": "cargo test passed." }),
                50,
            )),
        ],
        100000,
    );
    fixture.add_session("91bc");
    fixture.prior_history("91bc", &"older request context ".repeat(100));
    fixture
        .runner
        .ask("91bc", "first ask")
        .expect("the turn starts");
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(
        fixture.chat().iter().any(is_compact_body),
        "the provider window is enough to compact"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_window_cached_for_another_model_is_replaced_by_the_running_model() {
    let fixture = Fixture::new(
        "switched",
        catalog(Some(1_000_000)),
        vec![Canned::Json(tool_reply(
            "finish",
            serde_json::json!({ "text": "done", "proof": "cargo test passed." }),
            900,
        ))],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    let session = Session::at(&fixture.root.join("session-91bc"));
    session
        .update(|meta| {
            meta.model = "gpt-6.1-sol".into();
            meta.context_length = Some(272_000);
            true
        })
        .expect("the stale window is stored");
    fixture
        .runner
        .ask("91bc", "first ask")
        .expect("the turn starts");
    fixture.wait_for_cards("91bc", 1).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let meta = session.meta().expect("the meta reads");
    assert_eq!(meta.model, "grok-4.6");
    assert_eq!(meta.context_length, Some(1_000_000));
    let window = fixture.view("91bc").context.and_then(|usage| usage.window);
    assert_eq!(window, Some(1_000_000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_at_seventy_five_percent_does_not_start_background_compaction() {
    let fixture = Fixture::new(
        "no-prefire",
        catalog(Some(100000)),
        vec![
            Canned::Json(text_reply("turn one", 75000)),
            Canned::Json(text_reply("woke", 60)),
        ],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture.runner.ask("91bc", "first ask").unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!fixture.runner.is_compacting("91bc"));
    assert_eq!(fixture.chat().len(), 1);
    fixture.runner.ask("91bc", "wake").unwrap();
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(fixture.chat().iter().all(|body| !is_compact_body(body)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_compact_stops_with_a_visible_error_and_keeps_history() {
    let fixture = Fixture::new(
        "fail",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "list_dir",
                serde_json::json!({"path": "."}),
                90000,
            )),
            Canned::Status(400, "invalid summary request".into()),
        ],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture.prior_history("91bc", "older request");
    fixture.runner.ask("91bc", "first ask").unwrap();
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(fixture
        .events("91bc")
        .iter()
        .all(|event| event.kind != EventKind::Compact));
    assert_eq!(fixture.chat().len(), 2);
    assert!(fixture
        .events("91bc")
        .iter()
        .any(|event| event.kind == EventKind::Result
            && event.body["text"]
                .as_str()
                .is_some_and(|text| text.contains("Context compaction failed"))));
    assert!(!fixture.runner.is_compacting("91bc"));
}

struct SocketFixture {
    _handle: tokio::task::JoinHandle<()>,
    _fake: FakeServer,
    root: PathBuf,
    socket: PathBuf,
}

impl Drop for SocketFixture {
    fn drop(&mut self) {
        self._handle.abort();
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl SocketFixture {
    async fn new(
        name: &str,
        catalog: Value,
        replies: Vec<Canned>,
        delay: Duration,
    ) -> SocketFixture {
        let root = std::env::temp_dir().join(format!(
            "kyotoagent-compact-sock-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the root exists");
        let fake = FakeServer::start(catalog, replies, delay);
        let config = Config::from_toml(&format!(
            "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"grok-4.6\"\n",
            fake.base_url()
        ))
        .expect("the config parses");
        let server = Server::new(&root, &config).expect("the server is built");
        let handle = tokio::spawn(async move {
            let _ = server.serve().await;
        });
        let socket = root.join(SOCKET_FILE);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "the socket did not appear");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        SocketFixture {
            _handle: handle,
            _fake: fake,
            root,
            socket,
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
        stream.write_all(head.as_bytes()).await.expect("head");
        stream.write_all(body.as_bytes()).await.expect("body");
        stream.flush().await.expect("flush");
        let mut raw = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let n = stream.read(&mut chunk).await.expect("read");
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
            if raw.windows(4).any(|window| window == b"\r\n\r\n") {
                let text = String::from_utf8_lossy(&raw);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(key, _)| key.trim().eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
            }
        }
        let text = String::from_utf8_lossy(&raw);
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    async fn create_session(&self, workspace: &str) -> String {
        let body = serde_json::json!({ "workspace": workspace }).to_string();
        let (status, response) = self.request("POST", "/v1/sessions", Some(&body)).await;
        assert_eq!(status, 201, "{response}");
        serde_json::from_str::<Value>(&response).expect("json")["id"]
            .as_str()
            .expect("id")
            .to_string()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_during_compact_is_queued_and_runs_when_compact_finishes() {
    let fixture = SocketFixture::new(
        "queued",
        catalog(Some(100000)),
        vec![
            Canned::Json(text_reply("turn one", 75000)),
            Canned::Json(summary_reply("the session so far")),
            Canned::Json(text_reply("ran next", 20)),
        ],
        Duration::from_millis(800),
    )
    .await;
    let workspace = fixture.root.join("w-91bc");
    fs::create_dir_all(&workspace).expect("workspace");
    let id = fixture
        .create_session(workspace.to_str().expect("path"))
        .await;
    let body = serde_json::json!({ "text": "first ask" }).to_string();
    let (status, response) = fixture
        .request("POST", &format!("/v1/sessions/{id}/messages"), Some(&body))
        .await;
    assert_eq!(status, 202, "{response}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, response) = fixture
        .request("POST", &format!("/v1/sessions/{id}/compact"), None)
        .await;
    assert_eq!(status, 202, "{response}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, list) = fixture.request("GET", "/v1/sessions", None).await;
        assert_eq!(status, 200, "{list}");
        let rows: Vec<Value> = serde_json::from_str(&list).expect("list");
        if rows
            .iter()
            .any(|row| row["id"].as_str() == Some(id.as_str()) && row["compacting"] == true)
        {
            break;
        }
        assert!(Instant::now() < deadline, "compact did not start: {list}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let body = serde_json::json!({ "text": "again" }).to_string();
    let (status, response) = fixture
        .request("POST", &format!("/v1/sessions/{id}/messages"), Some(&body))
        .await;
    assert_eq!(status, 202, "a compact queues the ask: {response}");
    let queued: Value = serde_json::from_str(&response).expect("json");
    assert_eq!(queued["queued"], true, "{response}");
    let (status, view) = fixture
        .request("GET", &format!("/v1/sessions/{id}/view"), None)
        .await;
    assert_eq!(status, 200, "{view}");
    let view: Value = serde_json::from_str(&view).expect("json");
    assert_eq!(view["queue"], serde_json::json!(["again"]), "{view}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, log) = fixture
            .request("GET", &format!("/v1/sessions/{id}/events"), None)
            .await;
        assert_eq!(status, 200, "{log}");
        let (view_status, view_body) = fixture
            .request("GET", &format!("/v1/sessions/{id}/view"), None)
            .await;
        assert_eq!(view_status, 200, "{view_body}");
        let view: Value = serde_json::from_str(&view_body).expect("json");
        if log.contains("again") && view["status"] == "idle" && view.get("queue").is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the queued ask did not start: {log} {view_body}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_turn_compacts_completed_tools_repeatedly() {
    let fixture = Fixture::new(
        "first-turn-repeated",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "read_file",
                serde_json::json!({"path": "large.txt"}),
                90000,
            )),
            Canned::Json(summary_reply("first file findings")),
            Canned::Json(tool_reply(
                "read_file",
                serde_json::json!({"path": "large.txt"}),
                90000,
            )),
            Canned::Json(summary_reply("second file findings")),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({"text": "done", "proof": "checked"}),
                50,
            )),
        ],
        Duration::ZERO,
    );
    let workspace = fixture.add_session("91bc");
    fs::write(workspace.join("large.txt"), "file evidence ".repeat(1000)).unwrap();
    fixture
        .runner
        .ask("91bc", "Keep my original requirements")
        .unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let posts = fixture.chat();
    assert_eq!(posts.iter().filter(|body| is_compact_body(body)).count(), 2);
    for body in posts.iter().filter(|body| !is_compact_body(body)).skip(1) {
        assert!(user_contents(body)
            .iter()
            .any(|text| text.contains("Keep my original requirements")));
        assert!(body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| message["role"] != "tool"));
    }
    assert_eq!(
        fixture
            .events("91bc")
            .iter()
            .filter(|event| event.kind == EventKind::Compact)
            .count(),
        2
    );
    assert!(fixture
        .events("91bc")
        .iter()
        .any(|event| event.kind == EventKind::ToolResult
            && event.body["output"]
                .as_str()
                .is_some_and(|text| text.contains("file evidence"))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_read_offsets_and_limits_select_lines() {
    let fixture = Fixture::new(
        "model-line-offset",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "read_file",
                serde_json::json!({"path": "source.txt", "offset": 1360, "limit": 30}),
                100,
            )),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({"text": "done", "proof": "checked"}),
                100,
            )),
        ],
        Duration::ZERO,
    );
    let workspace = fixture.add_session("91bc");
    fs::write(
        workspace.join("source.txt"),
        (1..=2000)
            .map(|line| format!("source line {line}\n"))
            .collect::<String>(),
    )
    .unwrap();
    fixture
        .runner
        .ask("91bc", "Read the requested range")
        .unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let output = fixture
        .events("91bc")
        .into_iter()
        .find(|event| event.kind == EventKind::ToolResult && event.body["tool"] == "read_file")
        .unwrap();
    let text = output.body["output"].as_str().unwrap();
    assert!(text.contains("1360→source line 1360"), "{text}");
    assert!(text.contains("1389→source line 1389"), "{text}");
    assert!(!text.contains("source line 1390"), "{text}");
    assert!(text.contains("line 1390"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_overflow_compacts_and_resubmits_once() {
    let fixture = Fixture::new(
        "overflow-recovery",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply("read_file", serde_json::json!({"path": "large.txt"}), 100)),
            Canned::Status(400, serde_json::json!({"error": "[input_too_large] The prompt is too long for this model's context window"}).to_string()),
            Canned::Json(summary_reply("file findings")),
            Canned::Json(tool_reply("finish", serde_json::json!({"text": "recovered", "proof": "checked"}), 50)),
        ],
        Duration::ZERO,
    );
    let workspace = fixture.add_session("91bc");
    fs::write(workspace.join("large.txt"), "file evidence ".repeat(1000)).unwrap();
    fixture.runner.ask("91bc", "Keep the task").unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let posts = fixture.chat();
    assert_eq!(posts.len(), 4);
    assert!(is_compact_body(&posts[2]));
    assert!(user_contents(&posts[3])
        .iter()
        .any(|text| text.contains("Keep the task")));
    assert!(posts[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|message| message["role"] != "tool"));
    assert!(fixture
        .view("91bc")
        .cards
        .iter()
        .any(|card| card.body["text"] == "recovered"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_usage_includes_new_tool_output_before_compaction() {
    let fixture = Fixture::new(
        "provider-usage-growth",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "read_file",
                serde_json::json!({"path": "large.txt"}),
                80000,
            )),
            Canned::Json(summary_reply("file findings")),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({"text": "done", "proof": "checked"}),
                50,
            )),
        ],
        Duration::ZERO,
    );
    let workspace = fixture.add_session("91bc");
    fs::write(
        workspace.join("large.txt"),
        format!("{}\n", "x".repeat(150)).repeat(200),
    )
    .unwrap();
    fixture.runner.ask("91bc", "Inspect the file").unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(is_compact_body(&fixture.chat()[1]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_provider_overflow_stops_without_another_retry() {
    let overflow = Canned::Status(400, "context_length_exceeded".into());
    let fixture = Fixture::new(
        "bounded-overflow-retry",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "read_file",
                serde_json::json!({"path": "large.txt"}),
                100,
            )),
            overflow.clone(),
            Canned::Json(summary_reply("file findings")),
            overflow,
        ],
        Duration::ZERO,
    );
    let workspace = fixture.add_session("91bc");
    fs::write(workspace.join("large.txt"), "evidence\n".repeat(200)).unwrap();
    fixture.runner.ask("91bc", "Keep the task").unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(fixture.chat().len(), 4);
    assert_eq!(
        fixture
            .chat()
            .iter()
            .filter(|body| is_compact_body(body))
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_tool_output_is_saved_and_referenced_in_live_history() {
    let fixture = Fixture::new(
        "saved-tool-output",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "read_file",
                serde_json::json!({"path": "large.txt"}),
                100,
            )),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({"text": "done", "proof": "checked"}),
                100,
            )),
        ],
        Duration::ZERO,
    );
    let workspace = fixture.add_session("91bc");
    let first_line = "x".repeat(32764);
    fs::write(
        workspace.join("large.txt"),
        format!("{first_line}\nlast line\n"),
    )
    .unwrap();
    fixture.runner.ask("91bc", "Inspect the file").unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let saved = fs::read_dir(fixture.root.join("session-91bc/tool-output"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(fs::read_to_string(&saved).unwrap().contains(&first_line));
    let posts = fixture.chat();
    let output = posts[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(output.len() <= 32768);
    assert!(output.contains(&saved.to_string_lossy().to_string()));
    assert!(output.contains("line 2"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrelated_bad_requests_are_not_compacted_or_retried() {
    let fixture = Fixture::new(
        "unrelated-bad-request",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "list_dir",
                serde_json::json!({"path": "."}),
                100,
            )),
            Canned::Status(400, "invalid tool schema".into()),
        ],
        Duration::ZERO,
    );
    fixture.add_session("91bc");
    fixture.runner.ask("91bc", "Inspect the workspace").unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(fixture.chat().len(), 2);
    assert!(fixture.chat().iter().all(|body| !is_compact_body(body)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overflowing_compaction_input_is_reduced_before_retry() {
    let fixture = Fixture::new(
        "compact-overflow-retry",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "read_file",
                serde_json::json!({"path": "large.txt"}),
                90000,
            )),
            Canned::Status(400, "input_too_large".into()),
            Canned::Json(summary_reply("file findings")),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({"text": "done", "proof": "checked"}),
                50,
            )),
        ],
        Duration::ZERO,
    );
    let workspace = fixture.add_session("91bc");
    fs::write(workspace.join("large.txt"), "evidence\n".repeat(200)).unwrap();
    fixture.runner.ask("91bc", "Keep the task").unwrap();
    fixture.wait_for_cards("91bc", 2).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let posts = fixture.chat();
    assert_eq!(posts.len(), 4);
    assert!(is_compact_body(&posts[1]));
    assert!(is_compact_body(&posts[2]));
    assert!(posts[2]["messages"].to_string().len() < posts[1]["messages"].to_string().len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compaction_cannot_rewind_an_active_checkpoint() {
    let fixture = Fixture::new(
        "monotonic-checkpoint",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "read_file",
                serde_json::json!({"path": "large.txt"}),
                90000,
            )),
            Canned::Json(summary_reply("active file findings")),
            Canned::Json(tool_reply(
                "write_file",
                serde_json::json!({"path": "notes.md", "contents": "hello"}),
                100,
            )),
            Canned::Json(summary_reply("updated checkpoint")),
            Canned::Json(text_reply("done", 50)),
        ],
        Duration::ZERO,
    );
    let workspace = fixture.add_session("91bc");
    fixture.prior_history(
        "91bc",
        &"An older task with enough readable background to summarize accurately. ".repeat(40),
    );
    fs::write(workspace.join("large.txt"), "evidence\n".repeat(200)).unwrap();
    fixture
        .runner
        .ask("91bc", "Read the file and create notes.md")
        .unwrap();
    fixture.wait_for_status("91bc", Status::Waiting).await;
    let posts = fixture.chat().len();
    fixture.runner.compact("91bc").unwrap();
    fixture.wait_compacting("91bc", false).await;
    assert_eq!(fixture.chat().len(), posts);
    fixture
        .runner
        .answer("91bc", kyotoagent::permit::Answer::allow_once())
        .unwrap();
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(
        fs::read_to_string(workspace.join("notes.md")).unwrap(),
        "hello"
    );
    assert_eq!(
        fixture
            .events("91bc")
            .iter()
            .filter(|event| event.kind == EventKind::Compact)
            .count(),
        2
    );
}

async fn closeout_across_turns(name: &str, external_edit: bool, commit: bool) {
    let finish = || {
        Canned::Json(tool_reply(
            "finish",
            serde_json::json!({"text": "done", "proof": "checked"}),
            100,
        ))
    };
    let check = || {
        Canned::Json(tool_reply(
            "run_closeout",
            serde_json::json!({"id": "docs"}),
            100,
        ))
    };
    let mut replies = vec![
        Canned::Json(tool_reply(
            "write_file",
            serde_json::json!({"path": "docs/x.md", "contents": "first"}),
            100,
        )),
        check(),
    ];
    if commit {
        replies.push(Canned::Json(tool_reply(
            "run",
            serde_json::json!({"argv": ["git", "commit", "-am", "turn one"]}),
            100,
        )));
    }
    replies.extend([finish(), finish()]);
    if external_edit {
        replies.extend([check(), finish()]);
    }
    let fixture = Fixture::new(name, catalog(Some(100000)), replies, Duration::ZERO);
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".kyotoagent")).unwrap();
    fs::create_dir_all(workspace.join("docs")).unwrap();
    fs::write(workspace.join("docs/x.md"), "baseline").unwrap();
    fs::write(workspace.join(".kyotoagent/closeout.yaml"), "version: 1\nitems:\n  - id: docs\n    kind: command\n    run: 'true'\n    hint: docs\n    paths: [docs/**]\n").unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test"],
        vec!["add", "."],
        vec!["commit", "-m", "baseline"],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&workspace)
            .output()
            .unwrap()
            .status
            .success());
    }
    fixture
        .runner
        .ask_with("91bc", "Edit docs and check them", Some(false))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let current = fixture.view("91bc");
        if current.status == Status::Waiting {
            fixture
                .runner
                .answer("91bc", kyotoagent::permit::Answer::allow_once())
                .unwrap();
        }
        if fixture
            .events("91bc")
            .iter()
            .any(|event| event.kind == EventKind::Proof)
        {
            break;
        }
        assert!(Instant::now() < deadline, "first turn did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        fixture
            .events("91bc")
            .iter()
            .filter(|event| event.kind == EventKind::CloseoutRun)
            .count(),
        1
    );
    if external_edit {
        fs::write(workspace.join("docs/x.md"), "changed by user").unwrap();
    }
    fixture
        .runner
        .ask_with("91bc", "Explain the result without editing", Some(false))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if fixture.view("91bc").status == Status::Waiting {
            fixture
                .runner
                .answer("91bc", kyotoagent::permit::Answer::allow_once())
                .unwrap();
        }
        if fixture
            .events("91bc")
            .iter()
            .filter(|event| event.kind == EventKind::Proof)
            .count()
            == 2
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "second turn did not finish: {:?} {:?}",
            fixture.view("91bc"),
            fixture.events("91bc")
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let events = fixture.events("91bc");
    let proof = events
        .iter()
        .rfind(|event| event.kind == EventKind::Proof)
        .unwrap();
    assert_eq!(proof.body["items"][0]["id"], "docs");
    assert_eq!(
        proof.body["items"][0]["outcome"],
        if external_edit {
            "passed"
        } else {
            "passed_earlier"
        }
    );
    let blocked = events
        .iter()
        .filter(|event| event.turn_id == proof.turn_id && event.kind == EventKind::ToolResult)
        .any(|event| {
            event.body["output"]
                .as_str()
                .is_some_and(|output| output.contains("Cannot finish yet"))
        });
    assert_eq!(blocked, external_edit);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == EventKind::CloseoutRun)
            .count(),
        if external_edit { 2 } else { 1 }
    );
    let rows = &fixture.view("91bc").closeout;
    assert!(rows
        .iter()
        .any(|row| row.required && row.status == view::CloseoutStatus::Passed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closeout_passes_and_proof_carry_across_turns() {
    closeout_across_turns("closeout-carry", false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closeout_passes_expire_after_between_turn_edits() {
    closeout_across_turns("closeout-external-edit", true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closeout_fingerprint_uses_the_head_at_turn_end() {
    closeout_across_turns("closeout-commit", false, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn degenerate_summary_is_retried_before_history_is_replaced() {
    let fixture = Fixture::new(
        "degenerate-retry",
        catalog(Some(100000)),
        vec![
            Canned::Json(tool_reply(
                "list_dir",
                serde_json::json!({"path": "."}),
                90000,
            )),
            Canned::Json(text_reply(
                "The height helper is past the readable window.",
                20,
            )),
            Canned::Json(summary_reply(
                "Preserve the filters and archive tool requirements",
            )),
            Canned::Json(tool_reply(
                "finish",
                serde_json::json!({"text": "done"}),
                50,
            )),
        ],
        Duration::from_millis(50),
    );
    fixture.add_session("91bc");
    fixture.prior_history("91bc", &"Original requirements and decisions. ".repeat(100));
    fixture
        .runner
        .ask("91bc", "Add filters and archive-only tool")
        .unwrap();
    fixture.wait_compacting("91bc", true).await;
    assert_eq!(fixture.view("91bc").status, Status::Working);
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let posts = fixture.chat();
    assert_eq!(posts.iter().filter(|body| is_compact_body(body)).count(), 2);
    assert!(posts[1]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("Pending Tasks"));
    let compacts = fixture
        .events("91bc")
        .into_iter()
        .filter(|event| event.kind == EventKind::Compact)
        .collect::<Vec<_>>();
    assert_eq!(compacts.len(), 1);
    assert!(compacts[0].body["summary"]
        .as_str()
        .unwrap()
        .contains("filters and archive"));
    assert!(!fixture.runner.is_compacting("91bc"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_summary_retry_keeps_history_and_clears_compacting() {
    let fixture = Fixture::new(
        "cancel-retry",
        catalog(Some(100000)),
        vec![Canned::Json(text_reply("too short", 1))],
        Duration::from_millis(50),
    );
    fixture.add_session("91bc");
    fixture.prior_history("91bc", &"Original task details. ".repeat(100));
    fixture.runner.compact("91bc").unwrap();
    fixture.wait_compacting("91bc", true).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    fixture.runner.cancel("91bc");
    fixture.wait_compacting("91bc", false).await;
    assert_eq!(fixture.chat().len(), 1);
    assert!(fixture
        .events("91bc")
        .iter()
        .all(|event| event.kind != EventKind::Compact));
}
