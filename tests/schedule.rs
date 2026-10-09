use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kyotoagent::config::Config;
use kyotoagent::events::{format_millis, Event, EventKind, ScheduleBody};
use kyotoagent::schedule::Clock;
use kyotoagent::screen::Status;
use kyotoagent::server::{Server, SOCKET_FILE};
use kyotoagent::session::{Session, SessionMeta};
use kyotoagent::turn::Runner;
use kyotoagent::view;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const AT: &str = "2026-09-29T00:00:00.000Z";
const T0: i64 = 1_790_640_000_000;
const NOTE: &str = "Check gh comments";

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
}

impl FakeServer {
    fn start(replies: Vec<Canned>) -> FakeServer {
        let listener = TcpListener::bind(loopback_host()).expect("the fake server binds");
        listener
            .set_nonblocking(true)
            .expect("the listener does not block the thread");
        let addr = listener
            .local_addr()
            .expect("the fake server has an address");
        let replies = Arc::new(Mutex::new(replies));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let replies = Arc::clone(&replies);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                            serve_one(stream, &replies);
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
        }
    }

    fn base_url(&self) -> String {
        http_url(&self.addr.to_string())
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

fn loopback_host() -> String {
    let mut host = String::from("127.0.0.1:");
    host.push('0');
    host
}

fn http_url(host: &str) -> String {
    let mut url = String::from("http:");
    url.push('/');
    url.push('/');
    url.push_str(host);
    url
}

fn serve_one(mut stream: TcpStream, replies: &Arc<Mutex<Vec<Canned>>>) {
    let Some((path, request_body)) = read_request(&mut stream) else {
        return;
    };
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
    clock: Clock,
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
            std::env::temp_dir().join(format!("kyotoagent-sched-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let server = FakeServer::start(replies);
        let config = Config::from_toml(&format!(
            "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
            server.base_url()
        ))
        .expect("the config parses");
        let clock = Clock::at(T0);
        let runner = Runner::with_clock(&config, clock.clone()).expect("the runner is built");
        Fixture {
            runner,
            server,
            clock,
            root,
        }
    }

    fn set_replies(&self, replies: Vec<Canned>) {
        *self.server.replies.lock().expect("replies") = replies;
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

fn has_kind(session: &Session, kind: EventKind) -> bool {
    session
        .events()
        .expect("the log reads")
        .iter()
        .any(|event| event.kind == kind)
}

fn kind_count(session: &Session, kind: EventKind) -> usize {
    session
        .events()
        .expect("the log reads")
        .iter()
        .filter(|event| event.kind == kind)
        .count()
}

fn asks(session: &Session) -> Vec<String> {
    session
        .events()
        .expect("the log reads")
        .iter()
        .filter(|event| event.kind == EventKind::UserAsk)
        .filter_map(|event| {
            event
                .body
                .get("text")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .collect()
}

fn schedule_id_from(session: &Session) -> String {
    session
        .events()
        .expect("the log reads")
        .into_iter()
        .rev()
        .find_map(|event| {
            if event.kind == EventKind::Schedule {
                event.body_as::<ScheduleBody>().ok().map(|body| body.id)
            } else {
                None
            }
        })
        .expect("a schedule started")
}

fn tool_output(session: &Session, tool: &str) -> String {
    session
        .events()
        .expect("the log reads")
        .into_iter()
        .rev()
        .find_map(|event| {
            if event.kind != EventKind::ToolResult {
                return None;
            }
            if event.body.get("tool").and_then(|value| value.as_str()) != Some(tool) {
                return None;
            }
            event
                .body
                .get("output")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn schedule_args(minutes: i64, note: &str) -> serde_json::Value {
    serde_json::json!({ "minutes": minutes, "note": note })
}

fn finish(text: &str) -> Canned {
    Canned::Json(tool_call_reply(vec![(
        "finish",
        serde_json::json!({ "text": text, "proof": "cargo test passed." }),
    )]))
}

#[tokio::test]
async fn schedule_lists_one_pending_wake_and_stays_idle() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("schedule", schedule_args(10, NOTE))])),
        finish("I will check later."),
    ];
    let fixture = Fixture::new("pending-idle", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Wake me about the comments.");
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::Schedule))
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let session = fixture.session("91bc");
    assert!(!has_kind(&session, EventKind::ScheduleCancel));
    let id = schedule_id_from(&session);
    assert_eq!(id.len(), 8);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(tool_output(&session, "schedule"), id);
    let view = fixture.view("91bc");
    assert_eq!(view.status, Status::Idle);
    assert_eq!(view.schedules.len(), 1);
    assert_eq!(view.schedules[0].id, id);
    assert_eq!(view.schedules[0].note, NOTE);
    assert_eq!(view.schedules[0].due_at, "2026-09-29T00:10:00.000Z");
    assert_eq!(view.schedules[0].remaining_min, 10);
    let cards = serde_json::to_string(&view.cards).expect("cards serialize");
    assert!(!cards.contains("dueAt"), "{cards}");
    let json = serde_json::to_string(&view).expect("the view serializes");
    assert!(json.contains("\"remainingMin\":10"), "{json}");
}

#[tokio::test]
async fn advancing_the_clock_on_idle_starts_a_turn_with_the_note() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("schedule", schedule_args(10, NOTE))])),
        finish("I will check later."),
        Canned::Json(text_reply("The comments are quiet.")),
    ];
    let fixture = Fixture::new("idle-wake", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Wake me about the comments.");
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::Schedule))
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(fixture.view("91bc").schedules.len(), 1);
    fixture.clock.advance(Duration::from_secs(10 * 60));
    fixture
        .wait_until("91bc", |session| {
            asks(session).iter().any(|text| text == NOTE)
        })
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(fixture.view("91bc").schedules.is_empty());
    assert!(has_kind(
        &fixture.session("91bc"),
        EventKind::ScheduleCancel
    ));
}

#[tokio::test]
async fn a_working_session_holds_the_wake_until_idle() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("schedule", schedule_args(10, NOTE))])),
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({ "text": "Keep going?", "choices": ["yes"] }),
        )])),
        finish("Done with the question."),
        Canned::Json(text_reply("Now looking at the comments.")),
    ];
    let fixture = Fixture::new("held-wake", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Schedule then ask.");
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::Question))
        .await;
    fixture.clock.advance(Duration::from_secs(10 * 60));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let before = asks(&fixture.session("91bc"));
    assert!(
        before.iter().all(|text| text != NOTE),
        "the wake waits: {before:?}"
    );
    assert_eq!(fixture.view("91bc").schedules.len(), 1);
    assert_eq!(fixture.view("91bc").schedules[0].note, NOTE);
    fixture.answer_question("91bc", "yes");
    fixture
        .wait_until("91bc", |session| {
            asks(session).iter().any(|text| text == NOTE)
        })
        .await;
    assert!(fixture.view("91bc").schedules.is_empty());
}

#[tokio::test]
async fn cancel_removes_the_wake_and_an_unknown_id_adds_no_event() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("schedule", schedule_args(10, NOTE))])),
        finish("Scheduled."),
    ];
    let fixture = Fixture::new("cancel-wake", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Schedule then cancel.");
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::Schedule))
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let id = schedule_id_from(&fixture.session("91bc"));
    fixture.set_replies(vec![
        Canned::Json(tool_call_reply(vec![(
            "cancel_schedule",
            serde_json::json!({ "id": id }),
        )])),
        finish("Cancelled."),
    ]);
    fixture.ask("91bc", "Cancel that.");
    fixture
        .wait_until("91bc", |session| {
            has_kind(session, EventKind::ScheduleCancel)
        })
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(fixture.view("91bc").schedules.is_empty());
    assert_eq!(tool_output(&fixture.session("91bc"), "cancel_schedule"), id);
    fixture.set_replies(vec![
        Canned::Json(tool_call_reply(vec![(
            "cancel_schedule",
            serde_json::json!({ "id": "deadbeef" }),
        )])),
        finish("Nothing to cancel."),
    ]);
    fixture.ask("91bc", "Cancel a missing one.");
    fixture
        .wait_until("91bc", |session| {
            tool_output(session, "cancel_schedule").contains("no schedule deadbeef")
        })
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(
        kind_count(&fixture.session("91bc"), EventKind::ScheduleCancel),
        1
    );
    let asks_before = asks(&fixture.session("91bc")).len();
    fixture.clock.advance(Duration::from_secs(10 * 60));
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(asks(&fixture.session("91bc")).len(), asks_before);
    assert!(!asks(&fixture.session("91bc"))
        .iter()
        .any(|text| text == NOTE));
}

#[tokio::test]
async fn minutes_of_zero_and_over_a_day_add_no_event() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("schedule", schedule_args(0, NOTE))])),
        Canned::Json(tool_call_reply(vec![(
            "schedule",
            schedule_args(1441, NOTE),
        )])),
        finish("Gave up."),
    ];
    let fixture = Fixture::new("bounds", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Try bad minutes.");
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::Result))
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(!has_kind(&fixture.session("91bc"), EventKind::Schedule));
    assert!(fixture.view("91bc").schedules.is_empty());
}

#[tokio::test]
async fn the_ninth_pending_schedule_is_an_error_with_no_event() {
    let mut calls = Vec::new();
    for i in 0..9 {
        calls.push(Canned::Json(tool_call_reply(vec![(
            "schedule",
            schedule_args(10 + i as i64, &format!("n{i}")),
        )])));
    }
    calls.push(finish("Eight is the cap."));
    let fixture = Fixture::new("cap", calls);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Schedule many.");
    fixture
        .wait_until("91bc", |session| has_kind(session, EventKind::Result))
        .await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(kind_count(&fixture.session("91bc"), EventKind::Schedule), 8);
    assert_eq!(fixture.view("91bc").schedules.len(), 8);
    let output = tool_output(&fixture.session("91bc"), "schedule");
    assert!(
        output.contains("already has 8"),
        "the ninth is a tool error: {output}"
    );
}

#[tokio::test]
async fn a_restart_with_a_future_due_still_fires() {
    let replies = vec![Canned::Json(text_reply("Checking the comments."))];
    let fixture = Fixture::new("restart-future", replies);
    fixture.add_session("91bc");
    let session = fixture.session("91bc");
    session
        .append(
            &Event::new("e1", AT, "t1", EventKind::Schedule)
                .with_body(&ScheduleBody {
                    id: "aa11bb22".into(),
                    note: NOTE.into(),
                    due_at: "2026-09-29T00:10:00.000Z".into(),
                })
                .expect("a body serializes"),
        )
        .expect("the schedule is logged");
    fixture
        .runner
        .reload(&[session.dir().to_path_buf()])
        .expect("reload");
    assert_eq!(fixture.view("91bc").schedules.len(), 1);
    assert_eq!(fixture.view("91bc").schedules[0].remaining_min, 10);
    fixture.clock.advance(Duration::from_secs(10 * 60));
    fixture
        .wait_until("91bc", |session| {
            asks(session).iter().any(|text| text == NOTE)
        })
        .await;
    assert!(fixture.view("91bc").schedules.is_empty());
}

#[tokio::test]
async fn a_restart_with_a_past_due_fires_once_idle() {
    let replies = vec![Canned::Json(text_reply("Checking the comments."))];
    let fixture = Fixture::new("restart-past", replies);
    fixture.add_session("91bc");
    fixture.clock.advance(Duration::from_secs(10 * 60));
    let session = fixture.session("91bc");
    session
        .append(
            &Event::new("e1", AT, "t1", EventKind::Schedule)
                .with_body(&ScheduleBody {
                    id: "aa11bb22".into(),
                    note: NOTE.into(),
                    due_at: "2026-09-29T00:00:00.000Z".into(),
                })
                .expect("a body serializes"),
        )
        .expect("the schedule is logged");
    fixture
        .runner
        .reload(&[session.dir().to_path_buf()])
        .expect("reload");
    fixture
        .wait_until("91bc", |session| {
            asks(session).iter().any(|text| text == NOTE)
        })
        .await;
    assert!(fixture.view("91bc").schedules.is_empty());
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
async fn the_view_route_lists_pending_schedules_and_omits_an_empty_list() {
    let root = std::env::temp_dir().join(format!("kyotoagent-sched-http-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let base = http_url("127.0.0.1:9");
    let config = Config::from_toml(&format!(
        "base_url = \"{base}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n"
    ))
    .expect("the config parses");
    fs::write(
        root.join("config.toml"),
        format!("base_url = \"{base}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n"),
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
    let (status, response) = client
        .request("GET", &format!("/v1/sessions/{id}/view"), None)
        .await;
    assert_eq!(status, 200, "{response}");
    assert!(
        !response.contains("\"schedules\""),
        "an empty list is omitted: {response}"
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_millis() as i64;
    let due_at = format_millis(now + 10 * 60_000 + 45_000);
    let session = Session::at(&root.join("sessions").join(&id));
    session
        .append(
            &Event::new("e9", AT, "t1", EventKind::Schedule)
                .with_body(&ScheduleBody {
                    id: "cd34ef56".into(),
                    note: NOTE.into(),
                    due_at,
                })
                .expect("a body"),
        )
        .expect("the schedule is logged");
    let (status, response) = client
        .request("GET", &format!("/v1/sessions/{id}/view"), None)
        .await;
    assert_eq!(status, 200, "{response}");
    let view: serde_json::Value = serde_json::from_str(&response).expect("json");
    assert_eq!(view["schedules"][0]["id"], "cd34ef56");
    assert_eq!(view["schedules"][0]["note"], NOTE);
    assert_eq!(view["schedules"][0]["remainingMin"], 10);
    handle.abort();
    let _ = fs::remove_dir_all(&root);
}
