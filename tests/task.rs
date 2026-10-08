use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kyotoagent::config::Config;
use kyotoagent::events::{Event, EventKind, TaskDoneBody, TaskStartBody, TaskStatus};
use kyotoagent::permit::Answer;
use kyotoagent::screen::Status;
use kyotoagent::server::{Server, SOCKET_FILE};
use kyotoagent::session::{Session, SessionMeta};
use kyotoagent::turn::Runner;
use kyotoagent::view::{self, CardKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    replies: Arc<Mutex<Vec<Canned>>>,
    requests: Arc<Mutex<Vec<String>>>,
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
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let replies = Arc::clone(&replies);
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                            serve_one(stream, &replies, &requests);
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
            replies,
            requests,
        }
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
    requests: &Arc<Mutex<Vec<String>>>,
) {
    let Some((path, body)) = read_request(&mut stream) else {
        return;
    };
    let request_body = body.clone();
    requests.lock().expect("requests").push(body);
    if path.contains("/models") {
        let catalog = serde_json::json!({
            "data": [{ "id": "test/model", "context_length": 200000 }]
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
    let reason = if status == 200 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn text_reply(text: &str) -> String {
    serde_json::json!({
        "choices": [{ "message": { "role": "assistant", "content": text } }]
    })
    .to_string()
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
    fn new(name: &str, replies: Vec<Canned>) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("kyotoagent-task-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let server = FakeServer::start(replies);
        let config = Config::from_toml(&format!(
            "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
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

    fn set_replies(&self, replies: Vec<Canned>) {
        *self.server.replies.lock().expect("replies") = replies;
    }

    fn requests(&self) -> Vec<String> {
        self.server.requests.lock().expect("requests").clone()
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

    fn session(&self, id: &str) -> Session {
        Session::at(&self.root.join(format!("session-{id}")))
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
            .answer_question(
                id,
                self.runner
                    .view(id)
                    .unwrap()
                    .cards
                    .iter()
                    .rev()
                    .find(|card| card.kind == kyotoagent::view::CardKind::Question)
                    .and_then(|card| card.body["eventId"].as_str())
                    .unwrap_or(""),
                text,
            )
            .expect("the answer lands");
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

    async fn wait_until(&self, id: &str, pred: impl Fn(&Session) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if pred(&self.session(id)) {
                return;
            }
            assert!(Instant::now() < deadline, "the wait timed out");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn fifo(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("a path");
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo {}", std::io::Error::last_os_error());
    path
}

fn unblock(path: &Path) {
    fs::write(path, "line from the pipe\n").expect("the pipe accepts a line");
}

fn start_argv() -> Vec<String> {
    vec!["sh".into(), "-c".into(), "cat block; exit 1".into()]
}

fn start_args() -> serde_json::Value {
    serde_json::json!({ "argv": start_argv() })
}

fn task_id_from(session: &Session) -> String {
    session
        .events()
        .expect("the log reads")
        .into_iter()
        .rev()
        .find_map(|event| {
            if event.kind == EventKind::TaskStart {
                event.body_as::<TaskStartBody>().ok().map(|body| body.id)
            } else {
                None
            }
        })
        .expect("a task started")
}

fn has_kind(session: &Session, kind: EventKind) -> bool {
    session
        .events()
        .expect("the log reads")
        .iter()
        .any(|event| event.kind == kind)
}

fn asks(session: &Session) -> Vec<String> {
    body_texts(session, EventKind::UserAsk)
}

fn body_texts(session: &Session, kind: EventKind) -> Vec<String> {
    session
        .events()
        .expect("the log reads")
        .iter()
        .filter(|event| event.kind == kind)
        .filter_map(|event| {
            event
                .body
                .get("text")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .collect()
}

fn task_exit_asks(session: &Session) -> Vec<String> {
    asks(session)
        .into_iter()
        .filter(|text| {
            let Some(rest) = text.strip_prefix("Task ") else {
                return false;
            };
            rest.contains(" exited ")
                || rest.ends_with(" timed out.")
                || rest.ends_with(" stopped.")
        })
        .collect()
}

fn tool_outputs(session: &Session) -> Vec<String> {
    session
        .events()
        .expect("the log reads")
        .iter()
        .filter(|event| event.kind == EventKind::ToolResult)
        .filter_map(|event| {
            event
                .body
                .get("output")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .collect()
}

#[tokio::test]
async fn start_task_returns_running_and_completion_waits_for_it() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(text_reply("Started.")),
    ];
    let fixture = Fixture::new("idle-while-running", replies);
    let workspace = fixture.add_session("91bc");
    let block = fifo(&workspace, "block");

    fixture.ask("91bc", "Run tests in the background.");
    let cards = fixture.wait_for_cards("91bc", 2).await;
    assert_eq!(cards[1].kind, CardKind::Permission);
    assert_eq!(
        cards[1].body["action"],
        serde_json::Value::from("Start sh -c cat block; exit 1")
    );
    assert!(
        !has_kind(&fixture.session("91bc"), EventKind::TaskStart),
        "the process waits for the allow"
    );

    fixture.answer("91bc", Answer::allow_once());
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::TaskStart))
        .await;

    let id = task_id_from(&fixture.session("91bc"));
    assert_eq!(id.len(), 8);
    assert!(
        !has_kind(&fixture.session("91bc"), EventKind::TaskDone),
        "the process is still up"
    );
    assert_eq!(fixture.view("91bc").status, Status::Working);
    let view = fixture.view("91bc");
    assert_eq!(view.tasks.len(), 1);
    assert_eq!(view.tasks[0].id, id);
    assert_eq!(view.tasks[0].argv, start_argv());
    assert_eq!(view.tasks[0].state, TaskStatus::Running);

    unblock(&block);
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::TaskDone))
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let session = fixture.session("91bc");
    assert!(
        task_exit_asks(&session).is_empty(),
        "an exit while idle is not an ask: {:?}",
        asks(&session)
    );
    assert_eq!(fixture.view("91bc").status, Status::Idle);
    assert_eq!(body_texts(&session, EventKind::Result), vec!["Started."]);
}

#[tokio::test]
async fn kill_task_cancels_a_background_command_and_leaves_the_session() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Started.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(text_reply("The background command stopped.")),
    ];
    let fixture = Fixture::new("kill-command", replies);
    let workspace = fixture.add_session("91bc");
    let block = fifo(&workspace, "block");
    fixture.ask("91bc", "Run tests in the background.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::TaskStart))
        .await;
    let id = task_id_from(&fixture.session("91bc"));
    let session_dir = fixture.root.join("session-91bc");
    assert!(session_dir.join("meta.json").is_file());
    let killed = fixture.runner.kill_task("91bc", &id).await;
    assert!(killed.contains("\"state\":\"stopped\""), "{killed}");
    assert!(!killed.starts_with("closed "), "{killed}");
    fixture
        .wait_until("91bc", |session| {
            session
                .events()
                .expect("the log reads")
                .iter()
                .any(|event| {
                    event.kind == EventKind::TaskDone
                        && event.body.get("id").and_then(|value| value.as_str())
                            == Some(id.as_str())
                        && event.body.get("state").and_then(|value| value.as_str())
                            == Some("stopped")
                })
        })
        .await;
    assert!(session_dir.join("meta.json").is_file());
    assert!(fixture.runner.view("91bc").is_ok());
    assert_eq!(
        fs::read_dir(&fixture.root)
            .expect("root")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().join("meta.json").is_file())
            .count(),
        1
    );
    fixture.wait_for_status("91bc", Status::Idle).await;
    drop(block);
}

#[tokio::test]
async fn a_deny_leaves_the_process_unstarted() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Did not start.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("deny", replies);
    let workspace = fixture.add_session("91bc");
    let _block = fifo(&workspace, "block");

    fixture.ask("91bc", "Start it.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::deny());
    fixture.wait_for_status("91bc", Status::Idle).await;

    assert!(!has_kind(&fixture.session("91bc"), EventKind::TaskStart));
    assert!(!has_kind(&fixture.session("91bc"), EventKind::TaskDone));
    assert!(fixture.view("91bc").tasks.is_empty());
    let json = serde_json::to_string(&fixture.view("91bc").cards).expect("cards serialize");
    assert!(!json.contains("start_task"), "{json}");
}

#[tokio::test]
async fn check_task_during_the_wait_returns_running_and_adds_no_card() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(text_reply("Started.")),
    ];
    let fixture = Fixture::new("check-running", replies);
    let workspace = fixture.add_session("91bc");
    let _block = fifo(&workspace, "block");

    fixture.ask("91bc", "Start and check.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::TaskStart))
        .await;

    let id = task_id_from(&fixture.session("91bc"));
    let snap = fixture.runner.task("91bc", &id).expect("the task is here");
    assert_eq!(snap.state, TaskStatus::Running);
    assert!(snap.exit.is_none());

    fixture.set_replies(vec![
        Canned::Json(tool_call_reply(vec![(
            "check_task",
            serde_json::json!({ "id": id }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Still running.", "proof": "cargo test passed." }),
        )])),
    ]);

    fixture
        .wait_until("91bc", |session| {
            session
                .events()
                .expect("the log reads")
                .iter()
                .any(|event| {
                    event.kind == EventKind::ToolCall
                        && event.body.get("tool").and_then(|v| v.as_str()) == Some("check_task")
                })
        })
        .await;
    fixture
        .wait_until("91bc", |session| {
            tool_outputs(session)
                .iter()
                .any(|output| output.contains("still running"))
        })
        .await;

    assert_eq!(fixture.view("91bc").tasks.len(), 1);
    let session = fixture.session("91bc");
    assert!(body_texts(&session, EventKind::Result).is_empty());
    assert!(
        task_exit_asks(&session).is_empty(),
        "the running task is not an ask"
    );
    let json = serde_json::to_string(&fixture.view("91bc").cards).expect("cards serialize");
    assert!(!json.contains("check_task"), "{json}");
    fixture.runner.kill_task("91bc", &id).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
}

#[tokio::test]
async fn the_task_leaves_the_list_and_the_tail_stays_off_the_view() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Started.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(text_reply("Saw the exit.")),
    ];
    let fixture = Fixture::new("card-tail", replies);
    let workspace = fixture.add_session("91bc");
    let block = fifo(&workspace, "block");

    fixture.ask("91bc", "Background it.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::TaskStart))
        .await;
    let id = task_id_from(&fixture.session("91bc"));
    unblock(&block);
    fixture
        .wait_until("91bc", |session| {
            session
                .events()
                .expect("the log reads")
                .iter()
                .any(|event| event.kind == EventKind::TaskDone)
        })
        .await;

    assert!(fixture.view("91bc").tasks.is_empty());
    let snap = fixture.runner.task("91bc", &id).expect("the finished task");
    assert_eq!(snap.exit, Some(1));
    assert_eq!(snap.state, TaskStatus::Exited);
    assert!(snap.tail.contains("line from the pipe"), "{}", snap.tail);
    let json = serde_json::to_string(&fixture.view("91bc")).expect("the view serializes");
    assert!(!json.contains("line from the pipe"), "{json}");
}

#[tokio::test]
async fn a_task_exit_allows_completion_without_an_extra_ask() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(text_reply("Started.")),
    ];
    let fixture = Fixture::new("idle-exit", replies);
    let workspace = fixture.add_session("91bc");
    let block = fifo(&workspace, "block");

    fixture.ask("91bc", "Run it behind me.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::TaskStart))
        .await;
    let id = task_id_from(&fixture.session("91bc"));
    unblock(&block);
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::TaskDone))
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let requests_before = fixture.requests().len();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let session = fixture.session("91bc");
    assert!(
        task_exit_asks(&session).is_empty(),
        "no ask for {id}: {:?}",
        asks(&session)
    );
    assert_eq!(fixture.view("91bc").status, Status::Idle);
    assert_eq!(body_texts(&session, EventKind::Result), vec!["Started."]);
    assert_eq!(
        fixture.requests().len(),
        requests_before,
        "an idle exit does not start a turn"
    );
}

#[tokio::test]
async fn a_busy_exit_does_not_become_a_later_ask() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({ "text": "Keep going?", "choices": ["yes"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done with the question.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("busy-exit", replies);
    let workspace = fixture.add_session("91bc");
    let block = fifo(&workspace, "block");

    fixture.ask("91bc", "Start then ask.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture
        .wait_until("91bc", |session| {
            session
                .events()
                .expect("the log reads")
                .iter()
                .any(|event| event.kind == EventKind::Question)
        })
        .await;
    unblock(&block);
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::TaskDone))
        .await;
    assert!(
        task_exit_asks(&fixture.session("91bc")).is_empty(),
        "a busy exit is not an ask: {:?}",
        asks(&fixture.session("91bc"))
    );
    assert!(body_texts(&fixture.session("91bc"), EventKind::Result).is_empty());
    fixture.answer_question("91bc", "yes");
    fixture.wait_for_status("91bc", Status::Idle).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let session = fixture.session("91bc");
    assert!(
        task_exit_asks(&session).is_empty(),
        "the exit does not start another turn: {:?}",
        asks(&session)
    );
    assert_eq!(
        body_texts(&session, EventKind::Result),
        vec!["Done with the question."]
    );
    assert_eq!(fixture.view("91bc").status, Status::Idle);
}

#[tokio::test]
async fn an_argv_allowed_for_run_starts_with_start_task() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run",
            serde_json::json!({ "argv": ["true"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Ran true.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "start_task",
            serde_json::json!({ "argv": ["true"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Started true.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(text_reply("true finished.")),
    ];
    let fixture = Fixture::new("shared-argv", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "Run true.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_session());
    fixture.wait_for_status("91bc", Status::Idle).await;

    fixture.set_replies(vec![
        Canned::Json(tool_call_reply(vec![(
            "start_task",
            serde_json::json!({ "argv": ["true"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Started true.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(text_reply("true finished.")),
    ]);
    fixture.ask("91bc", "Start true.");
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::TaskStart))
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let events = fixture.session("91bc").events().expect("the log reads");
    let start_permissions = events
        .iter()
        .filter(|event| {
            event.kind == EventKind::Permission
                && event
                    .body
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .starts_with("Start ")
        })
        .count();
    assert_eq!(start_permissions, 0, "the remembered argv was enough");
    assert!(has_kind(&fixture.session("91bc"), EventKind::TaskStart));
}

#[tokio::test]
async fn cancel_stops_this_session_and_leaves_the_other() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({"text":"Continue a?"}),
        )])),
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({"text":"Continue b?"}),
        )])),
        Canned::Json(text_reply("Finished b.")),
    ];
    let fixture = Fixture::new("cancel-one", replies);
    let workspace_a = fixture.add_session("aaaa");
    let workspace_b = fixture.add_session("bbbb");
    let _block_a = fifo(&workspace_a, "block");
    let _block_b = fifo(&workspace_b, "block");

    fixture.ask("aaaa", "Start a.");
    fixture.wait_for_cards("aaaa", 2).await;
    fixture.answer("aaaa", Answer::allow_once());
    fixture
        .wait_until("aaaa", |session| has_kind(session, EventKind::Question))
        .await;
    let id_a = task_id_from(&fixture.session("aaaa"));

    fixture.ask("bbbb", "Start b.");
    fixture.wait_for_cards("bbbb", 2).await;
    fixture.answer("bbbb", Answer::allow_once());
    fixture
        .wait_until("bbbb", |session| has_kind(session, EventKind::Question))
        .await;

    fixture.runner.cancel("aaaa");
    fixture
        .wait_until("aaaa", |session| {
            session
                .events()
                .expect("the log reads")
                .iter()
                .any(|event| event.kind == EventKind::TaskDone)
        })
        .await;
    fixture.wait_for_status("aaaa", Status::Idle).await;
    let done = fixture
        .session("aaaa")
        .events()
        .expect("the log reads")
        .into_iter()
        .find_map(|event| {
            if event.kind == EventKind::TaskDone {
                event.body_as::<TaskDoneBody>().ok()
            } else {
                None
            }
        })
        .expect("a stopped");
    assert_eq!(done.id, id_a);
    assert_eq!(done.state, TaskStatus::Stopped);
    assert!(
        task_exit_asks(&fixture.session("aaaa")).is_empty(),
        "cancel does not ask about the stop"
    );
    assert_eq!(fixture.view("aaaa").status, Status::Idle);
    assert!(
        !has_kind(&fixture.session("bbbb"), EventKind::TaskDone),
        "the other session kept its process"
    );
    fixture.runner.cancel("bbbb");
    fixture.wait_for_status("bbbb", Status::Idle).await;
}

#[tokio::test]
async fn a_running_task_in_the_log_stops_on_reload() {
    let replies = vec![Canned::Json(text_reply("Noted."))];
    let fixture = Fixture::new("reload-orphan", replies);
    fixture.add_session("91bc");
    let session = fixture.session("91bc");
    session
        .append(
            &Event::new("e1", AT, "t1", EventKind::TaskStart)
                .with_body(&TaskStartBody {
                    id: "ab12cd34".into(),
                    argv: vec!["cargo".into(), "test".into()],
                })
                .expect("a body serializes"),
        )
        .expect("the start is logged");
    fixture
        .runner
        .reload(&[session.dir().to_path_buf()])
        .expect("reload");
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::TaskDone))
        .await;
    let done = session
        .events()
        .expect("the log reads")
        .into_iter()
        .find_map(|event| {
            if event.kind == EventKind::TaskDone {
                event.body_as::<TaskDoneBody>().ok()
            } else {
                None
            }
        })
        .expect("stopped");
    assert_eq!(done.id, "ab12cd34");
    assert_eq!(done.state, TaskStatus::Stopped);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let session = fixture.session("91bc");
    assert!(
        task_exit_asks(&session).is_empty(),
        "a stopped orphan is not an ask: {:?}",
        asks(&session)
    );
    assert!(body_texts(&session, EventKind::Result).is_empty());
    assert_eq!(fixture.view("91bc").status, Status::Idle);
}

#[tokio::test]
async fn check_task_on_another_session_is_an_error() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({"text":"Continue a?"}),
        )])),
    ];
    let fixture = Fixture::new("other-session", replies);
    let workspace_a = fixture.add_session("aaaa");
    fixture.add_session("bbbb");
    let _block = fifo(&workspace_a, "block");

    fixture.ask("aaaa", "Start a.");
    fixture.wait_for_cards("aaaa", 2).await;
    fixture.answer("aaaa", Answer::allow_once());
    fixture
        .wait_until("aaaa", |session| has_kind(session, EventKind::Question))
        .await;
    let id_a = task_id_from(&fixture.session("aaaa"));

    let err = fixture
        .runner
        .task("bbbb", &id_a)
        .expect_err("the id belongs to the other session");
    assert!(err.contains("no task"), "{err}");

    fixture.set_replies(vec![
        Canned::Json(tool_call_reply(vec![(
            "check_task",
            serde_json::json!({ "id": id_a }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Checked.", "proof": "cargo test passed." }),
        )])),
    ]);
    fixture.ask("bbbb", "Check a.");
    fixture
        .wait_until("bbbb", |session| {
            session
                .events()
                .expect("the log reads")
                .iter()
                .any(|event| {
                    event.kind == EventKind::ToolCall
                        && event.body.get("tool").and_then(|v| v.as_str()) == Some("check_task")
                })
        })
        .await;
    fixture.wait_for_status("bbbb", Status::Idle).await;
    let events = fixture.session("bbbb").events().expect("the log reads");
    let result = events
        .iter()
        .rev()
        .find(|event| event.kind == EventKind::ToolResult && event.body["tool"] == "check_task");
    let output = result
        .and_then(|event| event.body.get("output").and_then(|v| v.as_str()))
        .unwrap_or("");
    assert!(
        output.contains("no task"),
        "the other session cannot see it: {output}"
    );
    fixture.runner.cancel("aaaa");
    fixture.wait_for_status("aaaa", Status::Idle).await;
}

#[tokio::test]
async fn finish_while_a_task_from_this_turn_is_live_stays_working() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("start_task", start_args())])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Too soon.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("live-finish", replies);
    let workspace = fixture.add_session("91bc");
    let block = fifo(&workspace, "block");

    fixture.ask("91bc", "Start it and finish.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture
        .wait_until("91bc", |session| {
            tool_outputs(session)
                .iter()
                .any(|output| output.contains("still running") && output.contains("check_task"))
        })
        .await;

    let session = fixture.session("91bc");
    assert_eq!(fixture.view("91bc").status, Status::Working);
    assert!(body_texts(&session, EventKind::Result).is_empty());
    assert!(!has_kind(&session, EventKind::Proof));

    let id = task_id_from(&session);
    let stopped = fixture.runner.kill_task("91bc", &id).await;
    assert!(stopped.contains("\"state\":\"stopped\""), "{stopped}");
    fixture.wait_for_status("91bc", Status::Idle).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let session = fixture.session("91bc");
    assert_eq!(
        body_texts(&session, EventKind::Result),
        vec!["Too soon.".to_string()]
    );
    assert!(!has_kind(&session, EventKind::Proof));
    assert!(
        task_exit_asks(&session).is_empty(),
        "stopping the task is not an ask: {:?}",
        asks(&session)
    );
    assert_eq!(fixture.view("91bc").status, Status::Idle);
    drop(block);
}

struct HttpClient {
    socket: PathBuf,
}

impl HttpClient {
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
            let n = stream.read(&mut chunk).await.expect("the answer reads");
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
            if raw.windows(4).any(|window| window == b"\r\n\r\n") {
                let text = String::from_utf8_lossy(&raw);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        if key.eq_ignore_ascii_case("content-length") {
                            value.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    });
                    if length.is_some_and(|length| body.len() >= length) {
                        break;
                    }
                }
            }
        }
        let text = String::from_utf8_lossy(&raw).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }
}

#[tokio::test]
async fn the_view_lists_a_running_task_and_the_task_route_returns_its_tail() {
    let root = std::env::temp_dir().join(format!("kyotoagent-task-http-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let config = Config::from_toml(
        "base_url = \"http://127.0.0.1:9\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
    )
    .expect("the config parses");
    fs::write(
        root.join("config.toml"),
        "base_url = \"http://127.0.0.1:9\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
    )
    .expect("the config writes");
    let server = Server::new(&root, &config).expect("the server is built");
    let handle = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    let client = HttpClient {
        socket: root.join(SOCKET_FILE),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::net::UnixStream::connect(&client.socket)
            .await
            .is_ok()
        {
            break;
        }
        assert!(Instant::now() < deadline, "the socket did not appear");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let workspace = root.join("w");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    let (status, response) = client
        .request(
            "POST",
            "/v1/sessions",
            Some(&serde_json::json!({ "workspace": workspace }).to_string()),
        )
        .await;
    assert_eq!(status, 201, "{response}");
    let created: serde_json::Value = serde_json::from_str(&response).expect("json");
    let id = created["id"].as_str().expect("an id").to_string();
    let session = Session::at(&root.join("sessions").join(&id));
    session
        .append(
            &Event::new("e9", AT, "t1", EventKind::TaskStart)
                .with_body(&TaskStartBody {
                    id: "ab12cd34".into(),
                    argv: vec!["sleep".into(), "30".into()],
                })
                .expect("a body"),
        )
        .expect("the start is logged");
    let (status, response) = client
        .request("GET", &format!("/v1/sessions/{id}/view"), None)
        .await;
    assert_eq!(status, 200, "{response}");
    let view: serde_json::Value = serde_json::from_str(&response).expect("json");
    assert_eq!(view["tasks"][0]["id"], "ab12cd34");
    assert_eq!(view["tasks"][0]["argv"][0], "sleep");
    assert_eq!(view["tasks"][0]["state"], "running");
    assert!(view["tasks"][0].get("tail").is_none(), "{view}");
    let (status, response) = client
        .request("GET", &format!("/v1/sessions/{id}/tasks/ab12cd34"), None)
        .await;
    assert_eq!(status, 200, "{response}");
    let task: serde_json::Value = serde_json::from_str(&response).expect("json");
    assert_eq!(task["id"], "ab12cd34");
    assert_eq!(task["state"], "running");
    assert!(task["exit"].is_null());
    assert_eq!(task["tail"], "");
    let (status, response) = client
        .request("GET", &format!("/v1/sessions/{id}/tasks/missing1"), None)
        .await;
    assert_eq!(status, 404, "{response}");
    handle.abort();
    let _ = fs::remove_dir_all(&root);
}
