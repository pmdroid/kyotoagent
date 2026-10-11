use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kyotoagent::config::Config;
use kyotoagent::events::EventKind;
use kyotoagent::permit::Answer;
use kyotoagent::screen::Status;
use kyotoagent::session::{Session, SessionMeta};
use kyotoagent::turn::Runner;
use kyotoagent::view::{self, CardKind};

const AT: &str = "2026-09-29T00:00:00.000Z";

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
    fn start(replies: Vec<Canned>) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the fake server binds");
        listener
            .set_nonblocking(true)
            .expect("the listener does not block the thread");
        let addr = listener
            .local_addr()
            .expect("the fake server has an address");
        let replies = Arc::new(Mutex::new(replies));
        let stop = Arc::new(AtomicBool::new(false));
        let bodies = Arc::new(Mutex::new(Vec::new()));

        let handle = {
            let replies = Arc::clone(&replies);
            let stop = Arc::clone(&stop);
            let bodies = Arc::clone(&bodies);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            serve_one(stream, &replies, &bodies);
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

    fn bodies(&self) -> Vec<String> {
        self.bodies
            .lock()
            .expect("the bodies are not poisoned")
            .clone()
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
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
    bodies: &Arc<Mutex<Vec<String>>>,
) {
    let Some((path, request_body)) = read_request(&mut stream) else {
        return;
    };
    if path.contains("chat/completions") {
        bodies
            .lock()
            .expect("the bodies are not poisoned")
            .push(request_body.clone());
    }
    if path.contains("/models") {
        let catalog = serde_json::json!({
            "data": [{ "id": "test/model", "context_length": 200000 }, { "id": "test/reviewer", "context_length": 200000 }]
        })
        .to_string();
        respond(&mut stream, 200, &catalog);
        return;
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
        Canned::Json(body) => {
            kyotoagent::chat::answer_completion(&mut stream, 200, &body, &request_body)
        }
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
    let path = start.next()?;
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
    Some((
        path.to_string(),
        String::from_utf8_lossy(&body).into_owned(),
    ))
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

fn tool_call_reply(calls: Vec<(&str, serde_json::Value)>) -> String {
    let tool_calls: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(index, (name, args))| {
            serde_json::json!({
                "id": format!("call_{}", index + 1),
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": args.to_string()
                }
            })
        })
        .collect();
    serde_json::json!({
        "choices": [{
            "message": { "role": "assistant", "content": null, "tool_calls": tool_calls }
        }]
    })
    .to_string()
}

struct Fixture {
    runner: Arc<Runner>,
    _server: FakeServer,
    root: PathBuf,
}

#[tokio::test]
async fn task_scoped_closeout_uses_the_session_id_when_task_id_is_missing() {
    let fixture = Fixture::new(
        "task-default-session",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "write_file",
                serde_json::json!({"path":"changed.txt","contents":"changed"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "run_closeout",
                serde_json::json!({"id":"test"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text":"Done."}),
            )])),
        ],
    );
    let workspace = fixture.add_session("91bc");
    Session::at(&fixture.root.join("session-91bc"))
        .update(|meta| {
            meta.task_id = None;
            true
        })
        .unwrap();
    fs::create_dir_all(workspace.join(".agents")).unwrap();
    fs::write(workspace.join(".agents/closeout.yaml"), "specVersion: '0.1'\nretry:\n  maxFailedAttemptsPerItem: 3\n  scope: task\nitems:\n  - id: test\n    kind: command\n    gate: beforePR\n    exec: ['true']\n    timeoutSeconds: 5\n").unwrap();
    fixture.ask("91bc", "Write and check");
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.allow_closeout("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(fixture.closeout_runs("91bc"), 1);
    assert!(!fixture.log("91bc").contains("requires a stable task ID"));
    let evidence = fs::read_dir(fixture.root.join("closeout"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("retry.json");
    let entries: serde_json::Value = serde_json::from_slice(&fs::read(evidence).unwrap()).unwrap();
    assert_eq!(entries[0]["identity"]["task"], "91bc");
}

#[tokio::test]
async fn accepting_exhausted_closeout_persists_only_for_the_approved_session() {
    let write = |path: &str| {
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path":path,"contents":"changed"}),
        )]))
    };
    let check = |id: &str| {
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":id}),
        )]))
    };
    let report = || {
        Canned::Json(tool_call_reply(vec![(
            "get_closeout",
            serde_json::json!({}),
        )]))
    };
    let finish = || {
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"Done."}),
        )]))
    };
    let fixture = Fixture::new(
        "accept-session-closeout",
        vec![
            write("first.txt"),
            check("test"),
            report(),
            check("other"),
            finish(),
            write("second.txt"),
            report(),
            finish(),
            write("third.txt"),
            report(),
            finish(),
        ],
    );
    let workspace = fixture.add_session("91bc");
    let first = Session::at(&fixture.root.join("session-91bc"));
    first
        .update(|meta| {
            meta.task_id = Some("shared-task".into());
            true
        })
        .unwrap();
    fs::create_dir_all(workspace.join(".agents")).unwrap();
    fs::write(workspace.join(".agents/closeout.yaml"), "specVersion: '0.1'\nretry:\n  maxFailedAttemptsPerItem: 1\n  scope: task\nitems:\n  - id: test\n    kind: command\n    gate: beforePR\n    exec: ['false']\n    timeoutSeconds: 5\n  - id: other\n    kind: command\n    gate: beforePR\n    exec: ['false']\n    timeoutSeconds: 5\n").unwrap();
    fixture.ask("91bc", "Write and check");
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.allow_closeout("91bc").await;
    let question = fixture.wait_for_waiting_question("91bc").await;
    assert!(question.body["text"]
        .as_str()
        .unwrap()
        .contains("failed 1 time"));
    assert_eq!(
        question.body["choices"],
        serde_json::json!(["Stop", "Accept failed closeout for this session"])
    );
    fixture.answer_question("91bc", "Accept failed closeout for this session");
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(fixture.closeout_runs("91bc"), 1);
    let events = first.events().unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind.label() == "closeout_bypassed"));
    let proof = events
        .iter()
        .rev()
        .find(|event| event.kind == EventKind::Proof)
        .unwrap();
    assert_eq!(proof.body["items"][0]["outcome"], "failed");
    assert!(proof.body["text"]
        .as_str()
        .unwrap()
        .contains("accepted by the user"));
    fixture.ask("91bc", "Continue this session");
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let reports: Vec<serde_json::Value> = tool_outputs(&fixture.log("91bc"), "get_closeout")
        .iter()
        .map(|output| serde_json::from_str(output).unwrap())
        .collect();
    assert_eq!(reports.len(), 2);
    for report in reports {
        assert_eq!(report["bypassed"], true);
        assert_eq!(report["pending"], serde_json::json!([]));
        assert!(report["blocked"].is_null());
        assert_eq!(report["required"][0]["status"], "exhausted");
        assert_eq!(report["required"][0]["failed_attempts"], 1);
    }
    let second = Session::at(&fixture.root.join("session-92bc"));
    let mut meta = SessionMeta::new("92bc", &workspace, "test/model", AT);
    meta.task_id = Some("shared-task".into());
    second.create(&meta).unwrap();
    fixture.runner.add_session(&second).unwrap();
    fixture.ask("92bc", "Continue the shared task in a new session");
    fixture.respond("92bc", Answer::allow_once()).await;
    fixture.wait_for_waiting_question("92bc").await;
    let reports = tool_outputs(&fixture.log("92bc"), "get_closeout");
    let report: serde_json::Value = serde_json::from_str(&reports[0]).unwrap();
    assert_eq!(report["bypassed"], false);
    assert_eq!(report["required"][0]["status"], "exhausted");
    assert!(report["blocked"].is_string());
    fixture.answer_question("92bc", "Stop");
    fixture.wait_for_status("92bc", Status::Idle).await;
    assert_eq!(fixture.closeout_runs("92bc"), 0);
}

#[tokio::test]
async fn task_retry_exhaustion_survives_another_session_and_cannot_be_cleared_by_continue() {
    let write = |path: &str| {
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path":path,"contents":"changed"}),
        )]))
    };
    let check = || {
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":"test"}),
        )]))
    };
    let fixture = Fixture::new(
        "task-retry-persistent",
        vec![
            write("first.txt"),
            check(),
            write("second.txt"),
            Canned::Json(tool_call_reply(vec![(
                "get_closeout",
                serde_json::json!({}),
            )])),
            check(),
        ],
    );
    let workspace = fixture.add_session("91bc");
    let first = Session::at(&fixture.root.join("session-91bc"));
    first
        .update(|meta| {
            meta.task_id = Some("stable-task".into());
            true
        })
        .unwrap();
    fs::create_dir_all(workspace.join(".agents")).unwrap();
    fs::write(workspace.join(".agents/closeout.yaml"), "specVersion: '0.1'\nretry:\n  maxFailedAttemptsPerItem: 1\n  scope: task\nitems:\n  - id: test\n    kind: command\n    gate: beforePR\n    exec: [sh, -c, 'exit 1']\n    timeoutSeconds: 5\n").unwrap();
    fixture.ask("91bc", "Write and run the check twice");
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.respond("91bc", Answer::allow_once()).await;
    let question = fixture.wait_for_waiting_question("91bc").await;
    assert!(question.body["text"]
        .as_str()
        .unwrap()
        .contains("Retry limit reached"));
    assert_eq!(
        question.body["choices"],
        serde_json::json!(["Stop", "Accept failed closeout for this session"])
    );
    fixture.answer_question("91bc", "continue");
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(fixture.closeout_runs("91bc"), 1);
    let second = Session::at(&fixture.root.join("session-92bc"));
    let mut meta = SessionMeta::new("92bc", &workspace, "test/model", AT);
    meta.task_id = Some("stable-task".into());
    second.create(&meta).unwrap();
    fixture.runner.add_session(&second).unwrap();
    fixture.ask("92bc", "Continue the same task in another session");
    fixture.respond("92bc", Answer::allow_once()).await;
    let question = fixture.wait_for_waiting_question("92bc").await;
    assert!(question.body["text"]
        .as_str()
        .unwrap()
        .contains("Retry limit reached"));
    let outputs = tool_outputs(&fixture.log("92bc"), "get_closeout");
    let report: serde_json::Value = serde_json::from_str(&outputs[0]).unwrap();
    assert_eq!(report["pending"], serde_json::json!([]));
    assert_eq!(report["required"][0]["status"], "exhausted");
    fixture.answer_question("92bc", "stop");
    fixture.wait_for_status("92bc", Status::Idle).await;
    assert_eq!(fixture.closeout_runs("92bc"), 0);
}

#[tokio::test]
async fn a_public_policy_without_retry_allows_more_than_three_failed_attempts() {
    let mut replies = vec![Canned::Json(tool_call_reply(vec![(
        "write_file",
        serde_json::json!({"path":"changed.txt","contents":"changed"}),
    )]))];
    for _ in 0..4 {
        replies.push(Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":"test"}),
        )])));
    }
    replies.push(Canned::Json(tool_call_reply(vec![(
        "ask",
        serde_json::json!({"text":"Help","choices":["stop"]}),
    )])));
    let fixture = Fixture::new("public-unlimited", replies);
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".agents")).unwrap();
    fs::write(workspace.join(".agents/closeout.yaml"), "specVersion: '0.1'\nitems:\n  - id: test\n    kind: command\n    gate: beforePR\n    exec: [sh, -c, 'exit 1']\n    timeoutSeconds: 5\n").unwrap();
    fixture.ask("91bc", "Write and run four checks");
    for _ in 0..5 {
        fixture.respond("91bc", Answer::allow_once()).await;
    }
    fixture.wait_for_waiting_question("91bc").await;
    assert_eq!(fixture.closeout_runs("91bc"), 4);
    fixture.runner.cancel("91bc");
    fixture.wait_for_status("91bc", Status::Idle).await;
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for entry in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            if let Ok(meta) = Session::at(&entry.path()).meta() {
                self.runner.cancel(&meta.id);
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(name: &str, replies: Vec<Canned>) -> Fixture {
        Self::configured(name, replies, "")
    }

    fn configured(name: &str, replies: Vec<Canned>, project: &str) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("kyotoagent-closeout-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let server = FakeServer::start(replies);
        let config = Config::from_toml(&format!(
            "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n{}",
            server.base_url(),
            project.replace("WORKSPACE", &root.join("w-91bc").to_string_lossy())
        ))
        .expect("the config parses");
        let runner = Runner::new(&config).expect("the runner is built");
        Fixture {
            runner,
            _server: server,
            root,
        }
    }

    fn add_session(&self, id: &str) -> PathBuf {
        let workspace = self.root.join(format!("w-{id}"));
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let session = Session::at(&self.root.join(format!("session-{id}")));
        session
            .create(&SessionMeta::new(id, &workspace, "test/model", AT))
            .expect("the session is created");
        self.runner
            .add_session(&session)
            .expect("the session is added");
        workspace
    }

    fn hide_closeout(&self, id: &str) {
        Session::at(&self.root.join(format!("session-{id}")))
            .set_show_closeout(false)
            .expect("the flag writes");
    }

    fn prompts(&self) -> Vec<String> {
        self._server.bodies()
    }

    fn write_closeout(&self, workspace: &Path, run: &str) {
        fs::create_dir_all(workspace.join(".kyotoagent"))
            .expect("the .kyotoagent directory exists");
        fs::write(
            workspace.join(".kyotoagent").join("closeout.yaml"),
            format!(
                "version: 1\nitems:\n  - id: test\n    kind: command\n    run: {run}\n    hint: Fix the failing test\n"
            ),
        )
        .expect("the closeout file is written");
    }

    fn view(&self, id: &str) -> view::View {
        self.runner.view(id).expect("the view reads")
    }

    fn ask(&self, id: &str, text: &str) {
        self.runner.ask(id, text).expect("the turn starts");
    }

    fn answer(&self, id: &str, answer: Answer) {
        self.runner.answer(id, answer).expect("the answer lands");
    }

    fn answer_question(&self, id: &str, text: &str) {
        self.runner
            .answer_question_for(
                id,
                Some(
                    self.runner
                        .view(id)
                        .unwrap()
                        .cards
                        .iter()
                        .rev()
                        .find(|card| card.kind == kyotoagent::view::CardKind::Question)
                        .and_then(|card| card.body["eventId"].as_str())
                        .unwrap_or(""),
                ),
                text,
            )
            .expect("the answer lands");
    }

    fn log(&self, id: &str) -> String {
        fs::read_to_string(self.root.join(format!("session-{id}")).join("events.jsonl"))
            .expect("the log reads")
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

    async fn wait_for_cards(&self, id: &str, count: usize) -> Vec<view::Card> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let view = self.view(id);
            if view.cards.len() >= count {
                return view.cards;
            }
            assert!(
                Instant::now() < deadline,
                "the session did not reach {count} cards"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_for_turn_to_start(&self, id: &str) {
        self.wait_for_cards(id, 1).await;
    }

    async fn wait_for_log(&self, id: &str, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.log(id).contains(text) {
                return;
            }
            assert!(Instant::now() < deadline, "the log does not contain {text}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_for_waiting_permission(&self, id: &str) -> view::Card {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let view = self.view(id);
            if let Some(card) = view.cards.iter().rev().find(|card| {
                view.status == Status::Waiting
                    && card.kind == CardKind::Permission
                    && card
                        .body
                        .get("decision")
                        .map(|value| value.is_null())
                        .unwrap_or(true)
            }) {
                return card.clone();
            }
            assert!(
                Instant::now() < deadline,
                "the session did not wait on a permission"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn respond(&self, id: &str, answer: Answer) {
        let card = self.wait_for_waiting_permission(id).await;
        let event_id = card.body["eventId"].as_str().unwrap();
        self.answer(id, answer.for_permission(event_id));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let events = Session::at(&self.root.join(format!("session-{id}")))
                .events()
                .unwrap();
            if events.iter().any(|event| {
                event.kind == EventKind::PermissionAnswer && event.body["permissionId"] == event_id
            }) {
                return;
            }
            assert!(Instant::now() < deadline, "the permission was not settled");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn closeout_runs(&self, id: &str) -> usize {
        self.log(id).matches("closeout_run").count()
    }

    async fn wait_for_closeout_runs(&self, id: &str, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.closeout_runs(id) >= count {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the log did not reach {count} closeout_run events"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn allow_closeout(&self, id: &str) {
        let before = self.closeout_runs(id);
        self.wait_for_waiting_permission(id).await;
        self.answer(id, Answer::allow_once());
        self.wait_for_closeout_runs(id, before + 1).await;
    }

    async fn wait_for_waiting_question(&self, id: &str) -> view::Card {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let view = self.view(id);
            if let Some(card) = view.cards.iter().rev().find(|card| {
                view.status == Status::Waiting
                    && card.kind == CardKind::Question
                    && card
                        .body
                        .get("answer")
                        .map(|value| value.is_null())
                        .unwrap_or(true)
            }) {
                return card.clone();
            }
            assert!(
                Instant::now() < deadline,
                "the session did not wait on a question"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test]
async fn a_run_that_matches_a_pinned_check_is_refused() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run",
            serde_json::json!({ "argv": ["sh", "-c", "echo out; exit 0"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("rejected-run", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo out; exit 0");

    fixture.ask("91bc", "Run the check.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture
        .wait_for_log("91bc", "Use run_closeout with id test")
        .await;
    let log = fixture.log("91bc");
    assert!(
        log.contains("run_closeout"),
        "the tool result names run_closeout: {log}"
    );
    assert!(log.contains("test"), "the tool result names the id: {log}");
    assert!(!log.contains("closeout_run"), "no check ran yet: {log}");

    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;
    let log = fixture.log("91bc");
    assert!(
        log.contains("closeout_run"),
        "the check ran after run_closeout: {log}"
    );
}

#[tokio::test]
async fn a_closeout_run_is_recorded_and_draws_no_card() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("recorded", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo out; exit 0");

    fixture.ask("91bc", "Run the check.");

    let card = fixture.wait_for_waiting_permission("91bc").await;
    assert_eq!(card.kind, CardKind::Permission);
    let action = card.body["action"].as_str().expect("the action");
    assert!(
        action.contains("closeout test"),
        "the action names the check: {action}"
    );
    let argv = card.body["argv"].as_array().expect("the argv");
    assert_eq!(
        argv,
        &vec![
            serde_json::json!("sh"),
            serde_json::json!("-c"),
            serde_json::json!("echo out; exit 0")
        ],
        "the argv preserves the command arguments"
    );

    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;

    let log = fixture.log("91bc");
    assert!(
        log.contains("closeout_run"),
        "the event is in the log: {log}"
    );
    assert!(
        log.contains("\"id\":\"test\""),
        "the event names the id: {log}"
    );
    assert!(
        log.contains("\"attempt\":1"),
        "the event records the attempt: {log}"
    );
    assert!(
        log.contains("\"exit\":0"),
        "the event records the exit code: {log}"
    );

    let view = fixture.view("91bc");
    let kinds: Vec<CardKind> = view.cards.iter().map(|card| card.kind).collect();
    assert_eq!(
        kinds,
        vec![CardKind::Ask, CardKind::Result, CardKind::Proof]
    );
    let events = Session::at(&fixture.root.join("session-91bc"))
        .events()
        .unwrap();
    assert!(!events.iter().any(|event| event.kind == EventKind::Artifact));
    let run: kyotoagent::events::CloseoutRunBody = events
        .iter()
        .find(|event| event.kind == EventKind::CloseoutRun)
        .unwrap()
        .body_as()
        .unwrap();
    let file = run.transcript.unwrap();
    let bytes = fs::read(fixture.root.join("session-91bc/proof").join(&file.id)).unwrap();
    let transcript = String::from_utf8(bytes).unwrap();
    assert!(transcript.contains("exit: Some(0)"));
    assert!(transcript.contains("truncated: false"));
    assert!(kyotoagent::proof::artifact_history(&events)
        .unwrap()
        .is_empty());
    assert_eq!(
        view.closeout[0].runs[0].transcript.as_ref().unwrap().id,
        file.id
    );
    assert!(events
        .iter()
        .any(|event| event.kind == EventKind::ToolResult
            && event.body["output"]
                .as_str()
                .is_some_and(|output| output.contains(&file.id))));
    let json = serde_json::to_string(&view.cards).expect("cards serialize");
    assert!(
        !json.contains("closeout_run"),
        "no closeout_run card: {json}"
    );
}

#[tokio::test]
async fn finish_before_a_pass_is_refused_and_after_a_pass_it_ends_the_turn() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path": "changed.txt", "contents": "change"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("refused-finish", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo out; exit 0");

    fixture.ask("91bc", "Finish.");
    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_turn_to_start("91bc").await;

    fixture.wait_for_log("91bc", "Cannot finish yet").await;
    let log = fixture.log("91bc");
    assert!(
        log.contains("Cannot finish yet. Check test is missing. Attempts 0 of 3. Hint: Fix the failing test."),
        "the tool result names the id, status, and hint: {log}"
    );
    assert!(
        !log.contains("\"kind\":\"result\""),
        "no result event: {log}"
    );

    fixture.allow_closeout("91bc").await;

    fixture.wait_for_status("91bc", Status::Idle).await;

    let log = fixture.log("91bc");
    assert!(
        log.contains("\"kind\":\"result\""),
        "the result event is in the log: {log}"
    );
}

#[tokio::test]
async fn a_write_after_the_pass_makes_finish_wait_for_another_pass() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "notes.md", "contents": "hello" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("stale", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo out; exit 0");

    fixture.ask("91bc", "Run the check, write, finish.");

    fixture.allow_closeout("91bc").await;

    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::allow_once());

    fixture.wait_for_log("91bc", "Cannot finish yet").await;
    let log = fixture.log("91bc");
    assert!(
        log.contains("Cannot finish yet. Check test is missing. Attempts 0 of 3. Hint: Fix the failing test."),
        "the pass is stale after the write: {log}"
    );

    fixture.allow_closeout("91bc").await;

    fixture.wait_for_status("91bc", Status::Idle).await;

    assert_eq!(
        fs::read_to_string(workspace.join("notes.md")).expect("the file reads"),
        "hello"
    );
}

fn tool_outputs(log: &str, tool: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["kind"] == "tool_result" && event["body"]["tool"] == tool)
        .map(|event| {
            event["body"]["output"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn get_closeout_tracks_paths_passes_and_later_edits() {
    let calls = vec![
        ("get_closeout", serde_json::json!({})),
        (
            "write_file",
            serde_json::json!({"path":"src/lib.rs","contents":"first"}),
        ),
        ("get_closeout", serde_json::json!({})),
        ("run_closeout", serde_json::json!({"id":"test"})),
        ("get_closeout", serde_json::json!({})),
        (
            "write_file",
            serde_json::json!({"path":"src/lib.rs","contents":"second"}),
        ),
        ("get_closeout", serde_json::json!({})),
        ("finish", serde_json::json!({"text":"Done"})),
        ("run_closeout", serde_json::json!({"id":"test"})),
        ("get_closeout", serde_json::json!({})),
        ("finish", serde_json::json!({"text":"Done"})),
    ];
    let fixture = Fixture::new(
        "get-closeout-paths",
        calls
            .into_iter()
            .map(|call| Canned::Json(tool_call_reply(vec![call])))
            .collect(),
    );
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".agents")).unwrap();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join(".agents/closeout.yaml"), "specVersion: '0.1'\nsetup:\n  - id: prepare\n    exec: ['true']\n    timeoutSeconds: 5\nitems:\n  - id: test\n    kind: command\n    gate: beforePR\n    exec: ['true']\n    timeoutSeconds: 5\n    paths: ['src/**']\n  - id: docs\n    kind: command\n    gate: beforePR\n    exec: ['false']\n    timeoutSeconds: 5\n    paths: ['docs/**']\n").unwrap();
    fixture.hide_closeout("91bc");
    fixture.ask(
        "91bc",
        "Discover and run the required checks after editing source",
    );
    for _ in 0..5 {
        fixture.respond("91bc", Answer::allow_once()).await;
    }
    fixture.wait_for_status("91bc", Status::Idle).await;
    let log = fixture.log("91bc");
    let reports: Vec<serde_json::Value> = tool_outputs(&log, "get_closeout")
        .iter()
        .map(|output| serde_json::from_str(output).expect("closeout returns JSON"))
        .collect();
    assert_eq!(reports.len(), 5);
    assert_eq!(reports[0]["required"], serde_json::json!([]));
    assert_eq!(reports[0]["pending"], serde_json::json!([]));
    assert_eq!(
        reports[1]["pending"],
        serde_json::json!(["prepare", "test"])
    );
    assert_eq!(reports[1]["required"].as_array().unwrap().len(), 2);
    assert_eq!(
        reports[1]["required"][1]["matched_paths"],
        serde_json::json!(["src/lib.rs"])
    );
    assert_eq!(reports[1]["required"][1]["status"], "missing");
    assert!(reports[1]["required"][1]["remaining_attempts"].is_null());
    assert_eq!(reports[2]["pending"], serde_json::json!([]));
    assert_eq!(reports[2]["required"][1]["status"], "passed");
    assert_eq!(reports[3]["pending"], serde_json::json!(["test"]));
    assert_eq!(reports[3]["required"][0]["status"], "passed");
    assert_eq!(reports[3]["required"][1]["status"], "stale");
    assert_eq!(
        reports[3]["required"][1]["matched_paths"],
        serde_json::json!(["src/lib.rs"])
    );
    assert_eq!(reports[4]["pending"], serde_json::json!([]));
    assert!(reports.iter().all(|report| report["blocked"].is_null()));
    assert!(tool_outputs(&log, "finish")[0].contains("Check test is missing"));
    assert_eq!(fixture.closeout_runs("91bc"), 3);
}

#[tokio::test]
async fn get_closeout_without_a_policy_has_no_requirements() {
    let fixture = Fixture::new(
        "get-closeout-empty",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "get_closeout",
                serde_json::json!({}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text":"Done"}),
            )])),
        ],
    );
    fixture.add_session("91bc");
    fixture.ask("91bc", "Discover closeout checks");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let outputs = tool_outputs(&fixture.log("91bc"), "get_closeout");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&outputs[0]).unwrap(),
        serde_json::json!({"required":[],"pending":[],"blocked":null,"bypassed":false})
    );
}

#[tokio::test]
async fn get_closeout_reports_failed_and_exhausted_retries_without_running_checks() {
    let calls = vec![
        (
            "write_file",
            serde_json::json!({"path":"changed.txt","contents":"first"}),
        ),
        ("run_closeout", serde_json::json!({"id":"test"})),
        ("get_closeout", serde_json::json!({})),
        ("run_closeout", serde_json::json!({"id":"test"})),
        ("get_closeout", serde_json::json!({})),
        (
            "ask",
            serde_json::json!({"text":"The check is exhausted","choices":["stop"]}),
        ),
    ];
    let fixture = Fixture::new(
        "get-closeout-retries",
        calls
            .into_iter()
            .map(|call| Canned::Json(tool_call_reply(vec![call])))
            .collect(),
    );
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".agents")).unwrap();
    fs::write(workspace.join(".agents/closeout.yaml"), "specVersion: '0.1'\nretry:\n  maxFailedAttemptsPerItem: 2\n  scope: task\nitems:\n  - id: test\n    kind: command\n    gate: beforePR\n    exec: ['false']\n    timeoutSeconds: 5\n").unwrap();
    Session::at(&fixture.root.join("session-91bc"))
        .update(|meta| {
            meta.task_id = Some("get-closeout-retries".into());
            true
        })
        .unwrap();
    fixture.ask("91bc", "Discover retries after each failure");
    for _ in 0..3 {
        fixture.respond("91bc", Answer::allow_once()).await;
    }
    fixture.wait_for_waiting_question("91bc").await;
    fixture.answer_question("91bc", "Stop");
    fixture.wait_for_status("91bc", Status::Idle).await;
    fixture.ask("91bc", "Show the exhausted check without running it");
    fixture.wait_for_waiting_question("91bc").await;
    let reports: Vec<serde_json::Value> = tool_outputs(&fixture.log("91bc"), "get_closeout")
        .iter()
        .map(|output| serde_json::from_str(output).unwrap())
        .collect();
    assert_eq!(reports[0]["pending"], serde_json::json!(["test"]));
    assert_eq!(reports[0]["required"][0]["status"], "failed");
    assert_eq!(reports[0]["required"][0]["remaining_attempts"], 1);
    assert_eq!(reports[1]["pending"], serde_json::json!([]));
    assert_eq!(reports[1]["required"][0]["status"], "exhausted");
    assert_eq!(reports[1]["required"][0]["remaining_attempts"], 0);
    assert!(reports[1]["blocked"]
        .as_str()
        .unwrap()
        .contains("exhausted"));
    assert_eq!(fixture.closeout_runs("91bc"), 2);
    fixture.runner.cancel("91bc");
    fixture.wait_for_status("91bc", Status::Idle).await;
}

#[tokio::test]
async fn a_failed_check_says_to_retry_or_ask_and_a_repeat_says_the_output_is_unchanged() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path": "changed.txt", "contents": "change"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done." }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
    ];
    let fixture = Fixture::new("retry-or-ask", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo out; exit 1");

    fixture.ask("91bc", "Run the check.");
    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::allow_once());
    tokio::time::sleep(Duration::from_millis(30)).await;
    fixture.allow_closeout("91bc").await;

    fixture.wait_for_log("91bc", "Cannot finish yet").await;
    let first = tool_outputs(&fixture.log("91bc"), "run_closeout");
    assert_eq!(first.len(), 1, "one check ran: {first:?}");
    assert!(
        first[0].contains("Fix this and run_closeout again, or ask if you are stuck."),
        "the tool result tells the model to retry or ask: {}",
        first[0]
    );
    assert!(
        first[0].contains("Fix the failing test"),
        "the tool result includes the hint: {}",
        first[0]
    );
    assert!(
        !first[0].contains("This output is unchanged."),
        "the first failure is not a repeat: {}",
        first[0]
    );
    let finishes = tool_outputs(&fixture.log("91bc"), "finish");
    assert!(
        finishes
            .iter()
            .any(|text| text.contains("Cannot finish yet.")),
        "finish stays closed: {finishes:?}"
    );

    fixture.allow_closeout("91bc").await;
    fixture
        .wait_for_log("91bc", "This output is unchanged.")
        .await;
    let second = tool_outputs(&fixture.log("91bc"), "run_closeout");
    assert_eq!(second.len(), 2, "the check ran again: {second:?}");
    assert!(
        second[1].contains("This output is unchanged."),
        "the same exit and tail are named: {}",
        second[1]
    );
    assert!(
        second[1].contains("Fix this and run_closeout again, or ask if you are stuck."),
        "the retry sentence stays: {}",
        second[1]
    );

    fixture.allow_closeout("91bc").await;
    let card = fixture.wait_for_waiting_question("91bc").await;
    let text = card.body["text"].as_str().expect("the question text");
    assert!(text.contains("Check test failed 3 times. Retry limit reached."));
    let choices = card.body["choices"].as_array().expect("the choices");
    assert_eq!(choices[0].as_str().unwrap(), "Stop");
    assert_eq!(
        choices[1].as_str().unwrap(),
        "Accept failed closeout for this session"
    );

    fixture.answer_question("91bc", "stop");
    fixture.wait_for_status("91bc", Status::Idle).await;
}

#[tokio::test]
async fn legacy_closeout_acceptance_skips_further_runs_without_clearing_failures() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("exhausted-continue", replies);
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".kyotoagent")).expect("the .kyotoagent directory exists");
    fs::write(
        workspace.join(".kyotoagent").join("closeout.yaml"),
        concat!(
            "version: 1\n",
            "items:\n",
            "  - id: test\n",
            "    kind: command\n",
            "    run: \"n=0; test -f n && n=$(cat n); n=$((n+1)); echo $n > n; test $n -gt 3\"\n",
            "    hint: Fix the failing test\n",
        ),
    )
    .expect("the closeout file is written");

    fixture.ask("91bc", "Run the check.");

    for _ in 0..3 {
        fixture.allow_closeout("91bc").await;
    }

    let card = fixture.wait_for_waiting_question("91bc").await;
    fixture.wait_for_status("91bc", Status::Waiting).await;
    assert_eq!(card.kind, CardKind::Question);
    let text = card.body["text"].as_str().expect("the question text");
    assert!(text.contains("Check test failed 3 times. Retry limit reached."));
    let choices = card.body["choices"].as_array().expect("the choices");
    assert_eq!(choices.len(), 2);
    assert_eq!(choices[0].as_str().unwrap(), "Stop");
    assert_eq!(
        choices[1].as_str().unwrap(),
        "Accept failed closeout for this session"
    );

    fixture.answer_question("91bc", "Accept failed closeout for this session");

    fixture.wait_for_status("91bc", Status::Idle).await;

    let log = fixture.log("91bc");
    let runs = log.matches("closeout_run").count();
    assert_eq!(runs, 3, "only the three failed attempts ran: {log}");
    assert!(log.contains("closeout_bypassed"));
    assert!(!log.contains("failure count for test was cleared"));
}

#[tokio::test]
async fn the_exhausted_question_stop_ends_the_turn() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
    ];
    let fixture = Fixture::new("exhausted-stop", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo out; exit 1");

    fixture.ask("91bc", "Run the check.");

    for _ in 0..3 {
        fixture.allow_closeout("91bc").await;
    }

    fixture.wait_for_waiting_question("91bc").await;
    fixture.wait_for_status("91bc", Status::Waiting).await;

    fixture.answer_question("91bc", "stop");

    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    let result = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Result)
        .expect("a result card");
    let text = result.body["text"].as_str().expect("the result text");
    assert!(text.contains("test"), "the result names the id: {text}");
    assert!(
        text.contains("did not pass"),
        "the result says the check did not pass: {text}"
    );
}

#[tokio::test]
async fn a_workspace_with_no_closeout_file_still_finishes() {
    let replies = vec![Canned::Json(tool_call_reply(vec![(
        "finish",
        serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
    )]))];
    let fixture = Fixture::new("no-closeout", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "Finish.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    let result = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Result)
        .expect("a result card");
    assert_eq!(result.body["text"], serde_json::Value::from("Done."));
    assert!(view.closeout.is_empty());
}

#[tokio::test]
async fn an_item_with_paths_that_match_nothing_is_absent_from_the_cannot_finish_text() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "README.md", "contents": "hello" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("paths-absent", replies);
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".kyotoagent")).expect("the .kyotoagent directory exists");
    fs::write(
        workspace.join(".kyotoagent").join("closeout.yaml"),
        "version: 1\nitems:\n  - id: test\n    kind: command\n    run: echo out; exit 0\n    hint: Fix the failing test\n    paths:\n      - \"src/**\"\n",
    )
    .expect("the closeout file is written");

    fixture.ask("91bc", "Write and finish.");

    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::allow_once());

    fixture.wait_for_status("91bc", Status::Idle).await;

    let log = fixture.log("91bc");
    assert!(
        !log.contains("Cannot finish yet"),
        "the item is not required: {log}"
    );
    assert_eq!(
        serde_json::to_value(&fixture.view("91bc").closeout).unwrap()[0]["status"],
        "not_required"
    );
}

#[tokio::test]
async fn a_matching_write_keeps_finish_waiting_for_a_current_pass() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "src/main.rs", "contents": "fn main() {}" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("paths-match", replies);
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join("src")).expect("src exists");
    fs::create_dir_all(workspace.join(".kyotoagent")).expect("the .kyotoagent directory exists");
    fs::write(
        workspace.join(".kyotoagent").join("closeout.yaml"),
        "version: 1\nitems:\n  - id: test\n    kind: command\n    run: echo out; exit 0\n    hint: Fix the failing test\n    paths:\n      - \"src/**\"\n",
    )
    .expect("the closeout file is written");

    fixture.ask("91bc", "Write src and finish.");

    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::allow_once());

    fixture.wait_for_log("91bc", "Cannot finish yet").await;
    let log = fixture.log("91bc");
    assert!(log.contains("Check test is missing"), "{log}");
    let view = fixture.view("91bc");
    assert_eq!(view.closeout.len(), 1);
    assert!(matches!(
        view.closeout[0].status,
        view::CloseoutStatus::Missing | view::CloseoutStatus::Running
    ));
    assert!(
        !log.contains("\"kind\":\"result\""),
        "no result event: {log}"
    );

    fixture.allow_closeout("91bc").await;

    fixture.wait_for_status("91bc", Status::Idle).await;

    let log = fixture.log("91bc");
    assert!(log.contains("\"kind\":\"result\""), "{log}");
}

#[tokio::test]
async fn a_denied_permission_does_not_count_as_a_failure() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("denied", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo out; exit 0");

    fixture.ask("91bc", "Run the check.");

    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::deny());
    fixture.wait_for_log("91bc", "Not allowed").await;

    fixture.wait_for_status("91bc", Status::Idle).await;
    fixture.ask("91bc", "Run the check now.");

    fixture.allow_closeout("91bc").await;

    fixture.wait_for_status("91bc", Status::Idle).await;

    let log = fixture.log("91bc");
    let runs = log.matches("closeout_run").count();
    assert_eq!(runs, 1, "only the allowed run is recorded: {log}");
}

#[tokio::test]
async fn a_malformed_file_fails_the_turn_at_start() {
    let replies = vec![Canned::Json(tool_call_reply(vec![(
        "finish",
        serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
    )]))];
    let fixture = Fixture::new("malformed", replies);
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".kyotoagent")).expect("the .kyotoagent directory exists");
    fs::write(
        workspace.join(".kyotoagent").join("closeout.yaml"),
        "version: 1\nitems:\n  - id: test\n    kind: command\n    run: a\n    hint: a\n    paths:\n      - \"../secret\"\n",
    )
    .expect("the closeout file is written");

    fixture.ask("91bc", "Finish.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    let result = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Result)
        .expect("a result card");
    let text = result.body["text"].as_str().expect("the result text");
    assert!(
        text.contains("../secret"),
        "the result names the parse error: {text}"
    );
    let log = fixture.log("91bc");
    assert!(
        !log.contains("\"tool\":\"finish\""),
        "finish is never reached: {log}"
    );
}

#[tokio::test]
async fn a_finished_turn_lists_each_closeout_run_on_the_proof_card() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "lint" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("proof-items", replies);
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".kyotoagent")).expect("the .kyotoagent directory exists");
    fs::write(
        workspace.join(".kyotoagent").join("closeout.yaml"),
        "version: 1\nitems:\n  - id: test\n    kind: command\n    run: echo test; exit 0\n    hint: Fix the failing test\n  - id: lint\n    kind: command\n    run: echo lint; exit 1\n    hint: Fix the lint\n    paths:\n      - \"src/**\"\n",
    )
    .expect("the closeout file is written");

    fixture.ask("91bc", "Run the checks.");

    fixture.allow_closeout("91bc").await;
    fixture.allow_closeout("91bc").await;

    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    let proof = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Proof)
        .expect("a proof card");
    let items = proof.body["items"].as_array().expect("proof items");
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["id"], serde_json::Value::from("test"));
    assert_eq!(items[0]["kind"], serde_json::Value::from("command"));
    assert_eq!(items[0]["outcome"], serde_json::Value::from("passed"));
    assert_eq!(items[1]["id"], serde_json::Value::from("lint"));
    assert_eq!(items[1]["kind"], serde_json::Value::from("command"));
    assert_eq!(items[1]["outcome"], serde_json::Value::from("failed"));
    assert_eq!(items[1]["exit"], serde_json::Value::from(1));
    assert!(
        items[1]["tail"].as_str().expect("tail").contains("lint"),
        "{items:?}"
    );
}

#[tokio::test]
async fn an_item_with_no_paths_is_not_required_when_the_turn_wrote_nothing() {
    let replies = vec![Canned::Json(tool_call_reply(vec![(
        "finish",
        serde_json::json!({"text": "Done."}),
    )]))];
    let fixture = Fixture::new("no-change", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "exit 1");
    assert_eq!(
        serde_json::to_value(&fixture.view("91bc").closeout).unwrap()[0]["status"],
        "not_required"
    );
    fixture.ask("91bc", "Finish without editing.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let log = fixture.log("91bc");
    assert!(!log.contains("Cannot finish yet"), "{log}");
    assert!(!log.contains("closeout_run"), "{log}");
    assert_eq!(
        serde_json::to_value(&fixture.view("91bc").closeout).unwrap()[0]["status"],
        "not_required"
    );
}

#[tokio::test]
async fn a_live_run_closeout_is_running_on_the_view() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "slept." }),
        )])),
    ];
    let fixture = Fixture::new("running-closeout", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(
        &workspace,
        "echo live-stdout; echo live-stderr >&2; sleep 2",
    );
    fixture.ask("91bc", "Run the check.");
    fixture.wait_for_waiting_permission("91bc").await;
    let waiting = fixture.view("91bc");
    assert_eq!(waiting.closeout.len(), 1);
    assert_eq!(waiting.closeout[0].id, "test");
    assert_eq!(
        waiting.closeout[0].status,
        kyotoagent::view::CloseoutStatus::Running
    );
    fixture.answer("91bc", Answer::allow_once());
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_running = false;
    let mut saw_output = false;
    loop {
        let view = fixture.view("91bc");
        if view.closeout.first().map(|row| row.status)
            == Some(kyotoagent::view::CloseoutStatus::Running)
        {
            saw_running = true;
            saw_output |= view.closeout[0].tail.contains("live-stdout")
                && view.closeout[0].tail.contains("live-stderr");
        }
        if fixture.closeout_runs("91bc") >= 1 {
            break;
        }
        assert!(Instant::now() < deadline, "the check did not finish");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        saw_running,
        "the strip stayed missing while the command ran"
    );
    assert!(
        saw_output,
        "stdout and stderr must arrive before the command exits"
    );
    fixture.wait_for_status("91bc", Status::Idle).await;
    let done = fixture.view("91bc");
    assert_eq!(
        done.closeout[0].status,
        kyotoagent::view::CloseoutStatus::Passed
    );
    assert_eq!(done.closeout[0].exit, Some(0));
}

#[tokio::test]
async fn hidden_checks_allow_read_only_finish_and_leave_the_file() {
    let replies = vec![Canned::Json(tool_call_reply(vec![(
        "finish",
        serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
    )]))];
    let fixture = Fixture::new("hidden-finish", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo out; exit 0");
    fixture.hide_closeout("91bc");

    fixture.ask("91bc", "Finish.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_log("91bc", "\"kind\":\"result\"").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    assert!(view.closeout.is_empty());
    let result = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Result)
        .expect("a result card");
    assert_eq!(result.body["text"], serde_json::Value::from("Done."));
    let log = fixture.log("91bc");
    assert!(!log.contains("Cannot finish yet"), "{log}");
    assert!(workspace
        .join(".kyotoagent")
        .join("closeout.yaml")
        .is_file());
    let prompts = fixture.prompts();
    assert!(
        prompts.iter().any(|body| body.contains("closeout checks")),
        "the prompt includes the checks even when hidden: {prompts:?}"
    );
}

#[tokio::test]
async fn hidden_required_checks_block_finish_until_they_pass() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path":"changed.txt", "contents":"changed"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"Too early", "proof":"claimed pass"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":"test"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"Verified", "proof":"check passed"}),
        )])),
    ];
    let fixture = Fixture::new("hidden-required-finish", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "true");
    fixture.hide_closeout("91bc");
    fixture.ask("91bc", "Write and finish");
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.wait_for_waiting_permission("91bc").await;
    assert!(fixture.log("91bc").contains("Cannot finish yet"));
    assert!(!fixture
        .view("91bc")
        .cards
        .iter()
        .any(|card| card.kind == CardKind::Result));
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(
        fixture
            .view("91bc")
            .cards
            .iter()
            .find(|card| card.kind == CardKind::Result)
            .unwrap()
            .body["text"],
        "Verified"
    );
}

#[tokio::test]
async fn shown_checks_still_refuse_finish() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path": "changed.txt", "contents": "change"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("shown-finish", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo out; exit 0");

    fixture.ask("91bc", "Finish.");
    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_log("91bc", "Cannot finish yet").await;
    let prompts = fixture.prompts();
    assert!(
        prompts.iter().any(|body| body.contains("closeout checks")),
        "the prompt lists the checks: {prompts:?}"
    );
    assert!(workspace
        .join(".kyotoagent")
        .join("closeout.yaml")
        .is_file());
    fixture.runner.cancel("91bc");
    fixture.wait_for_status("91bc", Status::Idle).await;
}

#[tokio::test]
async fn hidden_checks_still_run_closeout() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({ "id": "test" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "the check ran." }),
        )])),
    ];
    let fixture = Fixture::new("hidden-run", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo hidden-ran; exit 0");
    fixture.hide_closeout("91bc");

    fixture.ask("91bc", "Run the check.");
    fixture.allow_closeout("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let log = fixture.log("91bc");
    assert!(log.contains("hidden-ran"), "{log}");
    assert!(log.contains("closeout_run"), "{log}");
    assert!(!log.contains("Cannot finish yet"), "{log}");
    assert!(fixture.view("91bc").closeout.is_empty());
}

#[tokio::test]
async fn hidden_checks_still_run_the_stop_hook() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done after the hook.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":"test"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"Done after the hook.", "proof":"check passed"}),
        )])),
    ];
    let fixture = Fixture::new("hidden-stop", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "echo out; exit 0");
    fixture.hide_closeout("91bc");
    fs::write(
        workspace.join("stop.sh"),
        "if [ -f hook-allow ]; then exit 0; fi\necho hook-blocked-finish >&2\nexit 2\n",
    )
    .expect("the hook script writes");
    fs::create_dir_all(workspace.join(".agents")).expect("the hooks directory exists");
    fs::write(
        workspace.join(".agents").join("hooks.json"),
        serde_json::json!({
            "hooks": {
                "Stop": [{
                    "matcher": "",
                    "hooks": [{ "type": "command", "command": "sh ./stop.sh" }]
                }]
            }
        })
        .to_string(),
    )
    .expect("the hooks file writes");

    fixture.ask("91bc", "Finish.");
    fixture.wait_for_log("91bc", "hook-blocked-finish").await;
    let log = fixture.log("91bc");
    assert!(!log.contains("Cannot finish yet"), "{log}");
    assert_eq!(fixture.view("91bc").status, Status::Working);

    fs::write(workspace.join("hook-allow"), "").expect("the allow file writes");
    fixture.allow_closeout("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let view = fixture.view("91bc");
    assert_eq!(
        view.cards
            .iter()
            .find(|card| card.kind == CardKind::Result)
            .expect("a result")
            .body["text"],
        serde_json::Value::from("Done after the hook.")
    );
}

#[tokio::test]
async fn an_agents_closeout_file_still_blocks_finish() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path": "changed.txt", "contents": "change"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("agents-closeout", replies);
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".agents")).expect("the agents directory exists");
    fs::write(
        workspace.join(".agents").join("closeout.yaml"),
        "version: 1\nitems:\n  - id: test\n    kind: command\n    run: echo out; exit 0\n    hint: Fix the failing test\n",
    )
    .expect("the agents file writes");

    fixture.ask("91bc", "Finish.");
    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_log("91bc", "Cannot finish yet").await;
    assert!(workspace.join(".agents").join("closeout.yaml").is_file());
    fixture.runner.cancel("91bc");
    fixture.wait_for_status("91bc", Status::Idle).await;
}

#[tokio::test]
async fn get_closeout_discovers_project_fallback_requirements_after_shell_changes() {
    let fixture = Fixture::configured(
        "project-fallback",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "run",
                serde_json::json!({"argv": ["sh", "-c", "mkdir src; printf changed > src/lib.rs"]}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "get_closeout",
                serde_json::json!({}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text": "Done"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "run_closeout",
                serde_json::json!({"id": "test"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text": "Done"}),
            )])),
        ],
        r#"
[projects.repo]
path = "WORKSPACE"
closeout = "fallback.yaml"
"#,
    );
    let workspace = fixture.add_session("91bc");
    fs::write(workspace.join("fallback.yaml"), "version: 1\nitems:\n  - id: test\n    kind: command\n    run: true\n    hint: Run the configured check\n    paths: ['src/**']\n  - id: docs\n    kind: command\n    run: false\n    hint: Check docs\n    paths: ['docs/**']\n").unwrap();
    assert!(!workspace.join(".kyotoagent/closeout.yaml").exists());
    let rows = fixture.view("91bc").closeout;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| !row.required));
    fixture.ask("91bc", "Edit source and finish");
    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_log("91bc", "Cannot finish yet").await;
    fixture.wait_for_waiting_permission("91bc").await;
    let rows = fixture.view("91bc").closeout;
    assert!(rows[0].required);
    assert_eq!(rows[1].status, view::CloseoutStatus::NotRequired);
    assert!(!rows[1].required);
    let outputs = tool_outputs(&fixture.log("91bc"), "get_closeout");
    let report: serde_json::Value = serde_json::from_str(&outputs[0]).unwrap();
    assert_eq!(report["pending"], serde_json::json!(["test"]));
    assert_eq!(report["required"][0]["hint"], "Run the configured check");
    assert_eq!(
        report["required"][0]["matched_paths"],
        serde_json::json!(["src/lib.rs"])
    );
    let prompt: serde_json::Value = serde_json::from_str(&fixture.prompts()[0]).unwrap();
    let system = prompt["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("get_closeout"));
    assert!(!system.contains("Run the configured check"));
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;
    let rows = fixture.view("91bc").closeout;
    assert_eq!(rows[0].status, view::CloseoutStatus::Passed);
    assert_eq!(rows[1].status, view::CloseoutStatus::NotRequired);
    assert_eq!(fixture.closeout_runs("91bc"), 1);
    assert_eq!(
        fs::read_to_string(workspace.join("src/lib.rs")).unwrap(),
        "changed"
    );
}

#[tokio::test]
async fn setup_steps_run_in_order_before_the_command_and_skip_unmatched_paths() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path":"src.txt", "contents":"changed"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":"test"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"Done", "proof":"passed"}),
        )])),
    ];
    let fixture = Fixture::new("setup-order", replies);
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".kyotoagent")).unwrap();
    fs::write(
        workspace.join(".kyotoagent/closeout.yaml"),
        r#"
specVersion: "0.1"
setup:
  - id: first
    exec: [sh, -c, "printf first > order.txt"]
    timeoutSeconds: 5
  - id: skipped
    exec: [sh, -c, "exit 1"]
    timeoutSeconds: 5
    paths: ["docs/"]
  - id: second
    exec: [sh, -c, "printf second >> order.txt"]
    timeoutSeconds: 5
items:
  - id: test
    kind: command
    gate: beforePR
    exec: [sh, -c, "test $(cat order.txt) = firstsecond"]
    timeoutSeconds: 5
"#,
    )
    .unwrap();
    fixture.ask("91bc", "Write and check");
    for _ in 0..4 {
        fixture.respond("91bc", Answer::allow_once()).await;
    }
    fixture.wait_for_status("91bc", Status::Idle).await;
    let rows = fixture.view("91bc").closeout;
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["first", "skipped", "second", "test"]
    );
    assert_eq!(rows[1].status, view::CloseoutStatus::NotRequired);
    for index in [0, 2, 3] {
        assert_eq!(rows[index].status, view::CloseoutStatus::Passed);
    }
    assert_eq!(
        fs::read_to_string(workspace.join("order.txt")).unwrap(),
        "firstsecond"
    );
}

#[tokio::test]
async fn failing_setup_stops_the_check() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path":"src.txt", "contents":"changed"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":"test"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"Stopped", "proof":"setup failed"}),
        )])),
    ];
    let fixture = Fixture::new("setup-failure", replies);
    let workspace = fixture.add_session("91bc");
    fixture.hide_closeout("91bc");
    fs::create_dir_all(workspace.join(".kyotoagent")).unwrap();
    fs::write(
        workspace.join(".kyotoagent/closeout.yaml"),
        r#"
specVersion: "0.1"
setup:
  - id: install
    exec: [sh, -c, "echo setup-failed >&2; exit 1"]
    timeoutSeconds: 5
items:
  - id: test
    kind: command
    gate: beforePR
    exec: [sh, -c, "touch check-ran"]
    timeoutSeconds: 5
"#,
    )
    .unwrap();
    fixture.ask("91bc", "Write and check");
    for _ in 0..2 {
        fixture.respond("91bc", Answer::allow_once()).await;
    }
    fixture.wait_for_log("91bc", "Check install failed").await;
    fixture.runner.cancel("91bc");
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(!workspace.join("check-ran").exists());
    assert_eq!(fixture.closeout_runs("91bc"), 1);
    assert!(fixture.log("91bc").contains("setup-failed"));
}

#[tokio::test]
async fn required_closeout_blocks_pr_creation_even_when_hidden() {
    for (index, (tool, argv)) in [
        ("run", vec!["gh", "pr", "create"]),
        ("run", vec!["sh", "-c", "gh pr create"]),
        ("start_task", vec!["gh", "pr", "create"]),
    ]
    .into_iter()
    .enumerate()
    {
        let replies = vec![
            Canned::Json(tool_call_reply(vec![
                (
                    "write_file",
                    serde_json::json!({"path":"src.txt", "contents":"changed"}),
                ),
                (tool, serde_json::json!({"argv": argv})),
            ])),
            Canned::Json(tool_call_reply(vec![(
                "run_closeout",
                serde_json::json!({"id":"test"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text":"Stopped", "proof":"PR blocked"}),
            )])),
        ];
        let fixture = Fixture::new(&format!("pr-gate-hidden-{index}"), replies);
        let workspace = fixture.add_session("91bc");
        fixture.write_closeout(&workspace, "true");
        fixture.hide_closeout("91bc");
        fixture.ask("91bc", "Write then open a PR");
        fixture.respond("91bc", Answer::allow_once()).await;
        fixture.respond("91bc", Answer::allow_once()).await;
        fixture.wait_for_status("91bc", Status::Idle).await;
        let log = fixture.log("91bc");
        assert!(log.contains("Cannot open a pull request"));
        assert!(log.contains("Check test is missing"));
        assert_eq!(log.matches("\"kind\":\"permission\"").count(), 2);
    }
}
#[tokio::test]
async fn pr_creation_runs_when_checks_are_not_required() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run",
            serde_json::json!({"argv":["gh", "pr", "create"]}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"Done", "proof":"read only"}),
        )])),
    ];
    let fixture = Fixture::new("pr-not-required", replies);
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "false");
    fixture.ask("91bc", "Open a PR");
    let permission = fixture.wait_for_waiting_permission("91bc").await;
    assert_eq!(
        permission.body["argv"],
        serde_json::json!(["gh", "pr", "create"])
    );
    fixture.respond("91bc", Answer::deny()).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(!fixture.log("91bc").contains("Cannot open a pull request"));
}

#[tokio::test]
async fn pr_creation_requires_another_pass_after_a_new_write() {
    let gh = std::env::temp_dir().join(format!(
        "kyotoagent-closeout-{}-pr-stale-pass/w-91bc/gh",
        std::process::id()
    ));
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path":"src.txt", "contents":"first"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":"test"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run",
            serde_json::json!({"argv":[gh, "pr", "create"]}),
        )])),
        Canned::Json(tool_call_reply(vec![
            (
                "write_file",
                serde_json::json!({"path":"src.txt", "contents":"second"}),
            ),
            ("run", serde_json::json!({"argv":[gh, "pr", "create"]})),
        ])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":"test"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"Stopped", "proof":"stale pass"}),
        )])),
    ];
    let fixture = Fixture::new("pr-stale-pass", replies);
    let workspace = fixture.add_session("91bc");
    fs::copy("/usr/bin/true", &gh).unwrap();
    fixture.write_closeout(&workspace, "true");
    fixture.hide_closeout("91bc");
    fixture.ask("91bc", "Write, check, open PR, then edit again");
    for _ in 0..2 {
        fixture.respond("91bc", Answer::allow_once()).await;
    }
    let pr = fixture.wait_for_waiting_permission("91bc").await;
    assert_eq!(pr.body["argv"], serde_json::json!([gh, "pr", "create"]));
    fs::write(
        workspace.join("src.txt"),
        "changed while awaiting permission",
    )
    .unwrap();
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(fixture.log("91bc").contains("Cannot open a pull request"));
}

#[tokio::test]
async fn imported_review_uses_a_fresh_read_only_session_and_retains_its_findings() {
    imported_review_candidate("imported-review", false, false, false).await;
}

#[tokio::test]
async fn imported_review_includes_committed_changes() {
    imported_review_candidate("committed-review", true, false, false).await;
}

#[tokio::test]
async fn imported_review_includes_committed_and_pending_changes() {
    imported_review_candidate("partial-review", true, true, false).await;
}

#[tokio::test]
async fn imported_review_is_not_required_after_restoring_the_diff() {
    imported_review_candidate("empty-review", false, false, true).await;
}

async fn imported_review_candidate(name: &str, committed: bool, pending: bool, empty: bool) {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path":"changed.txt", "contents":"candidate"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "get_closeout",
            serde_json::json!({}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":"quality/review", "model":"test/model"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"Too early", "proof":""}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run_closeout",
            serde_json::json!({"id":"quality/review", "model":"test/reviewer"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({"path":"changed.txt", "contents":"reviewer edit"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"[]", "proof":"Inspected candidate"}),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text":"Verified", "proof":"review passed"}),
        )])),
    ];
    let fixture = Fixture::new(name, replies);
    let workspace = fixture.add_session("91bc");
    fs::create_dir_all(workspace.join(".agents/closeout")).unwrap();
    fs::create_dir_all(workspace.join(".agents/skills")).unwrap();
    let shared_skill = fixture.root.join("shared-review");
    fs::create_dir_all(&shared_skill).unwrap();
    std::os::unix::fs::symlink(&shared_skill, workspace.join(".agents/skills/review")).unwrap();
    fs::write(
        shared_skill.join("SKILL.md"),
        "Inspect each changed file for correctness. Report only P0 findings.",
    )
    .unwrap();
    fs::write(
        workspace.join(".agents/closeout.yaml"),
        "specVersion: '0.1'\nimports:\n  - path: .agents/closeout/review.yaml\n    as: quality\n",
    )
    .unwrap();
    fs::write(workspace.join(".agents/closeout/review.yaml"), "specVersion: '0.1'\nitems:\n  - id: review\n    kind: review\n    gate: beforePR\n    skill: .agents/skills/review/SKILL.md\n    independence:\n      differentSession: true\n      differentModel: true\n    failOn: P1\n").unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    git(&["config", "user.name", "Reviewer Test"]);
    git(&["config", "user.email", "review@example.test"]);
    fs::write(workspace.join("changed.txt"), "original\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "Base candidate"]);
    git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    fixture.ask("91bc", "Write and review");
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.wait_for_waiting_permission("91bc").await;
    if committed {
        git(&["add", "changed.txt"]);
        git(&["commit", "-m", "Commit candidate before review"]);
    }
    if pending {
        fs::write(workspace.join("pending.txt"), "pending change\n").unwrap();
        git(&["add", "pending.txt"]);
        fs::write(workspace.join("changed.txt"), "candidate\npending edit\n").unwrap();
    }
    if empty {
        git(&["restore", "changed.txt"]);
        let row = &fixture.view("91bc").closeout[0];
        assert!(!row.required);
        fixture.runner.cancel("91bc");
        fixture.wait_for_status("91bc", Status::Idle).await;
        assert!(!fixture
            .prompts()
            .iter()
            .any(|body| body.contains("Tracked git diff:")));
        return;
    }
    fixture.respond("91bc", Answer::allow_once()).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let outputs = tool_outputs(&fixture.log("91bc"), "get_closeout");
    let report: serde_json::Value = serde_json::from_str(&outputs[0]).unwrap();
    assert_eq!(report["pending"], serde_json::json!(["quality/review"]));
    assert_eq!(report["required"][0]["different_model"], true);
    assert_eq!(report["required"][0]["kind"], "review");
    assert_eq!(
        fs::read_to_string(workspace.join("changed.txt")).unwrap(),
        if pending {
            "candidate\npending edit\n"
        } else {
            "candidate"
        }
    );
    let row = &fixture.view("91bc").closeout[0];
    assert_eq!(row.id, "quality/review");
    assert_eq!(row.status, view::CloseoutStatus::Passed);
    assert!(row.tail.contains("Findings: []"));
    assert!(row.tail.contains("test/reviewer"));
    let log = fixture.log("91bc");
    assert!(log.contains("requires a different model"));
    assert!(log.contains("Cannot finish yet"));
    assert!(!log
        .lines()
        .any(|line| line.contains("\"kind\":\"result\"") && line.contains("Too early")));
    let prompts = fixture.prompts();
    let review_prompt = prompts
        .iter()
        .find(|body| body.contains("Inspect each changed file"))
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(review_prompt).unwrap();
    assert!(review_prompt.contains("-original"));
    assert!(review_prompt.contains("+candidate"));
    if pending {
        assert!(review_prompt.contains("+pending edit"));
        assert!(review_prompt.contains("+pending change"));
    }
    assert!(review_prompt.contains("Report findings at every severity P0, P1, P2, and P3"));
    assert!(review_prompt.contains("Ignore any severity limit in the skill"));
    assert!(review_prompt.contains("the host applies failOn"));
    assert_eq!(body["model"], "test/reviewer");
    assert!(body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .all(|tool| tool["function"]["name"] != "write_file"));
    assert!(log.contains("quality_review-attempt-1.txt"));
}

#[tokio::test]
async fn stop_hook_changes_require_closeout_before_finish() {
    let fixture = Fixture::new(
        "stop-hook-write",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text":"Too early"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "run_closeout",
                serde_json::json!({"id":"test"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text":"Verified"}),
            )])),
        ],
    );
    let workspace = fixture.add_session("91bc");
    fixture.write_closeout(&workspace, "true");
    fs::create_dir_all(workspace.join(".agents")).unwrap();
    fs::write(workspace.join(".agents/hooks.json"), serde_json::json!({"hooks":{"Stop":[{"matcher":"", "hooks":[{"type":"command", "command":"printf changed > hook-output.txt"}]}]}}).to_string()).unwrap();
    fixture.ask("91bc", "Finish after the hook");
    fixture.allow_closeout("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(fixture.closeout_runs("91bc"), 1);
    assert!(fixture.log("91bc").contains("Cannot finish yet"));
    assert_eq!(
        fs::read_to_string(workspace.join("hook-output.txt")).unwrap(),
        "changed"
    );
}

#[tokio::test]
async fn merging_main_through_a_tool_only_requires_engine_checks() {
    for conflict in [false, true] {
        let mut report_reply: serde_json::Value = serde_json::from_str(&tool_call_reply(vec![(
            "get_closeout",
            serde_json::json!({}),
        )]))
        .unwrap();
        report_reply["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] =
            serde_json::json!("");
        let fixture = Fixture::new(
            if conflict {
                "merge-conflict-paths"
            } else {
                "merge-paths"
            },
            vec![
                Canned::Json(tool_call_reply(vec![(
                    "run",
                    serde_json::json!({"argv": ["sh", "-c", "printf feature > apps/engine/x; git add apps/engine/x; git commit -m Engine; git merge origin/main --no-edit"]}),
                )])),
                Canned::Json(report_reply.to_string()),
                Canned::Json(tool_call_reply(vec![(
                    "ask_question",
                    serde_json::json!({"question": "Ready for checks?"}),
                )])),
            ],
        );
        let workspace = fixture.add_session("91bc");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&workspace)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        fixture.write_closeout(&workspace, "true");
        fs::write(workspace.join(".kyotoagent/closeout.yaml"), "version: 1\nitems:\n  - id: engine-local\n    kind: command\n    run: true\n    hint: Check engine\n    paths: ['apps/engine/**']\n  - id: engine-review\n    kind: command\n    run: true\n    hint: Review engine\n    paths: ['apps/engine/**']\n  - id: worker-local\n    kind: command\n    run: false\n    hint: Check worker\n    paths: ['apps/worker/**']\n").unwrap();
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Closeout Test"]);
        git(&["config", "user.email", "closeout@example.test"]);
        for path in ["apps/engine/x", "apps/worker/y"] {
            fs::create_dir_all(workspace.join(path).parent().unwrap()).unwrap();
            fs::write(workspace.join(path), "base").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-m", "Base"]);
        git(&["branch", "feature"]);
        fs::write(workspace.join("apps/worker/y"), "main").unwrap();
        if conflict {
            fs::write(workspace.join("apps/engine/x"), "main").unwrap();
        }
        git(&["commit", "-am", "Main"]);
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&["checkout", "feature"]);
        fixture.ask("91bc", "Update engine and merge main");
        fixture.wait_for_waiting_permission("91bc").await;
        fixture.answer("91bc", Answer::allow_once());
        fixture.wait_for_log("91bc", "Ready for checks?").await;
        let log = fixture.log("91bc");
        let changed: Vec<serde_json::Value> = log
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["kind"] == "closeout_changed")
            .collect();
        assert_eq!(changed.len(), 1, "{log}");
        assert_eq!(
            changed[0]["body"]["paths"],
            serde_json::json!(["apps/engine/x"])
        );
        let reports = tool_outputs(&log, "get_closeout");
        let report: serde_json::Value = serde_json::from_str(&reports[0]).unwrap();
        assert_eq!(
            report["pending"],
            serde_json::json!(["engine-local", "engine-review"])
        );
        let rows = fixture.view("91bc").closeout;
        assert!(rows
            .iter()
            .filter(|row| row.id.starts_with("engine-"))
            .all(|row| row.required));
        assert!(
            !rows
                .iter()
                .find(|row| row.id == "worker-local")
                .unwrap()
                .required
        );
        assert_eq!(workspace.join(".git/MERGE_HEAD").exists(), conflict);
        fixture.runner.cancel("91bc");
        fixture.wait_for_status("91bc", Status::Idle).await;
    }
}
