use std::fs;
use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use kyotoagent::config::Config;
use kyotoagent::events::{TodoItem, TodoStatus};
use kyotoagent::screen::{Card, ItemKind, ItemRun, Outcome, Overlay, SessionRow, Status, Wait};
use kyotoagent::server::{Server, SOCKET_FILE};
use kyotoagent::tui::{
    advance_requests, apply, apply_pane, capture_notices, capture_open_url, key, keystroke,
    last_opened_url, loop_error_is_fatal, mouse, poll, released_click, screen_model, take_notices,
    App, Client, Effect, Mode,
};
use kyotoagent::turn::Runner;
use kyotoagent::view::{CardKind, View};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use serde_json::Value;

#[derive(Clone)]
enum Canned {
    Json(String),
    Status(u16, String),
}

struct ChatHold {
    pause_at: Mutex<Option<usize>>,
    served: AtomicUsize,
    go: Mutex<bool>,
    cv: Condvar,
    holding: AtomicBool,
}

struct FakeServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    replies: Arc<Mutex<Vec<Canned>>>,
    received: Arc<Mutex<Vec<(String, String)>>>,
    models: Arc<Mutex<Option<String>>>,
    chat_hold: Arc<ChatHold>,
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
        let received = Arc::new(Mutex::new(Vec::new()));
        let models = Arc::new(Mutex::new(None));
        let chat_hold = Arc::new(ChatHold {
            pause_at: Mutex::new(None),
            served: AtomicUsize::new(0),
            go: Mutex::new(false),
            cv: Condvar::new(),
            holding: AtomicBool::new(false),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let replies = Arc::clone(&replies);
            let received = Arc::clone(&received);
            let models = Arc::clone(&models);
            let chat_hold = Arc::clone(&chat_hold);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            serve_one(stream, &replies, &received, &models, &chat_hold);
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
            received,
            models,
            chat_hold,
        }
    }

    fn pause_before(&self, index: usize) {
        *self.chat_hold.go.lock().expect("the hold") = false;
        *self.chat_hold.pause_at.lock().expect("the pause") = Some(index);
    }

    fn release(&self) {
        let mut go = self.chat_hold.go.lock().expect("the hold");
        *go = true;
        self.chat_hold.cv.notify_all();
    }

    fn is_holding(&self) -> bool {
        self.chat_hold.holding.load(Ordering::SeqCst)
    }

    fn set_replies(&self, replies: Vec<Canned>) {
        *self.replies.lock().expect("the queue is not poisoned") = replies;
    }

    fn set_models(&self, body: &str) {
        *self.models.lock().expect("the models slot is not poisoned") = Some(body.to_string());
    }

    fn posts(&self) -> Vec<Value> {
        self.received
            .lock()
            .expect("the log is not poisoned")
            .iter()
            .filter(|(path, _)| path.ends_with("/chat/completions"))
            .map(|(_, body)| serde_json::from_str(body).unwrap_or(Value::Null))
            .collect()
    }

    fn base_url(&self) -> String {
        let mut url = String::from("http:");
        url.push('/');
        url.push('/');
        url.push_str(&self.addr.to_string());
        url
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.release();
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_one(
    mut stream: TcpStream,
    replies: &Arc<Mutex<Vec<Canned>>>,
    received: &Arc<Mutex<Vec<(String, String)>>>,
    models: &Arc<Mutex<Option<String>>>,
    chat_hold: &Arc<ChatHold>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let Some((path, body)) = read_request(&mut stream) else {
        return;
    };
    let request_body = body.clone();
    received
        .lock()
        .expect("the log is not poisoned")
        .push((path.clone(), body));
    let reply = if path.contains("/models") {
        Some(Canned::Json(
            models
                .lock()
                .expect("the models slot is not poisoned")
                .clone()
                .unwrap_or_else(|| {
                    serde_json::json!({
                        "data": [{ "id": "test/model", "context_length": 200000 }]
                    })
                    .to_string()
                }),
        ))
    } else {
        None
    };
    if !path.contains("/models") {
        let index = chat_hold.served.load(Ordering::SeqCst);
        let paused = *chat_hold.pause_at.lock().expect("the pause") == Some(index);
        if paused {
            chat_hold.holding.store(true, Ordering::SeqCst);
            let mut go = chat_hold.go.lock().expect("the hold");
            while !*go {
                go = chat_hold.cv.wait(go).expect("the hold");
            }
            chat_hold.holding.store(false, Ordering::SeqCst);
        }
        chat_hold.served.fetch_add(1, Ordering::SeqCst);
    }
    let reply = reply.unwrap_or_else(|| {
        let mut queue = replies.lock().expect("the queue is not poisoned");
        if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue.first().cloned().unwrap_or(Canned::Json("{}".into()))
        }
    });
    match reply {
        Canned::Json(body) => {
            kyotoagent::chat::answer_completion(&mut stream, 200, &body, &request_body);
        }
        Canned::Status(status, body) => {
            kyotoagent::chat::answer_completion(&mut stream, status, &body, &request_body);
        }
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
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_string();
    let mut length = 0;
    for line in head.lines() {
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
    Some((path, String::from_utf8_lossy(&body).to_string()))
}

fn tool_call_reply(calls: Vec<(&str, Value)>) -> String {
    let tool_calls: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(index, (name, args))| {
            serde_json::json!({
                "id": format!("call_{}", index + 1),
                "type": "function",
                "function": { "name": name, "arguments": args.to_string() }
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

fn write_then_finish() -> Vec<Canned> {
    write_file_then_finish("notes.md", "hello", "Created notes.md.")
}

fn write_file_then_finish(path: &str, contents: &str, text: &str) -> Vec<Canned> {
    vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": path, "contents": contents }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": text, "proof": "cargo test passed." }),
        )])),
    ]
}

fn finish_only(text: &str) -> Vec<Canned> {
    vec![Canned::Json(tool_call_reply(vec![(
        "finish",
        serde_json::json!({ "text": text, "proof": "cargo test passed." }),
    )]))]
}

fn run_then_finish() -> Vec<Canned> {
    vec![
        Canned::Json(tool_call_reply(vec![(
            "run",
            serde_json::json!({ "argv": ["touch", "started"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Ran the command.", "proof": "cargo test passed." }),
        )])),
    ]
}

struct Fixture {
    _handle: tokio::task::JoinHandle<()>,
    _fake: FakeServer,
    root: PathBuf,
    client: Client,
    runner: Arc<Runner>,
    workspace: PathBuf,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.runner.release_all();
        self._handle.abort();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn ask_then_finish(text: &str, choices: &[&str]) -> Vec<Canned> {
    vec![
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({
                "text": text,
                "choices": choices
            }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ]
}

fn two_asks_then_finish() -> Vec<Canned> {
    vec![
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({
                "text": "Which way?",
                "choices": ["continue", "stop"]
            }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({
                "text": "And then?",
                "choices": ["left", "right"]
            }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ]
}

impl Fixture {
    async fn new(name: &str) -> Fixture {
        Fixture::with_replies(name, write_then_finish()).await
    }

    async fn with_replies(name: &str, replies: Vec<Canned>) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("kyotoagent-tui-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the root exists");
        let fake = FakeServer::start(replies);
        let config_text = format!(
            "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
            fake.base_url()
        );
        fs::write(root.join("config.toml"), &config_text).expect("the config writes");
        let config = Config::from_toml(&config_text).expect("the config parses");
        let server = Server::new(&root, &config).expect("the server is built");
        let runner = Arc::clone(server.runner());
        let handle = tokio::spawn(async move {
            let _ = server.serve().await;
        });
        let socket = root.join(SOCKET_FILE);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the server did not create the socket"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let workspace = root.join("work");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let workspace = fs::canonicalize(&workspace).unwrap_or(workspace);
        Fixture {
            _handle: handle,
            _fake: fake,
            client: Client::at(socket),
            runner,
            workspace,
            home: root.clone(),
            root,
        }
    }

    fn app(&self) -> App {
        App::new(self.workspace.clone(), self.home.clone(), String::new())
    }
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

async fn drive(app: &mut App, client: &Client, event: KeyEvent) {
    let effect = keystroke(app, event).expect("the key maps");
    apply(app, client, effect).await.expect("the key applies");
    tokio::time::timeout(Duration::from_secs(20), async {
        while matches!(screen_model(app).overlay, Some(Overlay::Text { text }) if text.contains("Fetching ") || text.contains("Saving project")) || matches!(screen_model(app).overlay, Some(Overlay::Delete { loading: true, .. })) {
            tokio::time::sleep(Duration::from_millis(5)).await;
            advance_requests(app, client).await;
        }
    }).await.expect("the catalog request completes");
}

async fn new_here(app: &mut App, client: &Client) {
    drive(app, client, ctrl('t')).await;
    drive(app, client, press(KeyCode::Char('1'))).await;
    drive(app, client, press(KeyCode::Char('1'))).await;
}

async fn type_text(app: &mut App, client: &Client, text: &str) {
    for c in text.chars() {
        drive(app, client, press(KeyCode::Char(c))).await;
    }
}

async fn wait_until(app: &mut App, client: &Client, pred: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        poll(app, client).await.expect("the screen polls");
        if pred(app) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the session did not reach the expected state"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn kinds(app: &App) -> Vec<&'static str> {
    app.cards
        .iter()
        .map(|card| match card {
            Card::Ask { .. } => "ask",
            Card::Question { .. } => "question",
            Card::Answer { .. } => "answer",
            Card::Permission { .. } => "permission",
            Card::Result { .. } => "result",
            Card::Proof { .. } => "proof",
            Card::Enhance { .. } => "enhance",
            Card::Artifact { .. } => "artifact",
        })
        .collect()
}

fn draw(app: &App) -> String {
    let backend = TestBackend::new(76, 24);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    let model = screen_model(app);
    terminal
        .draw(|frame| kyotoagent::screen::render(&model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[tokio::test]
async fn a_second_session_can_finish_while_the_first_waits() {
    let fixture = Fixture::new("two-sessions").await;
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    assert!(app.sessions.is_empty());
    assert!(
        draw(&app).contains("Kyoto Agent"),
        "the empty pane names the product"
    );

    new_here(&mut app, &fixture.client).await;
    assert_eq!(app.sessions.len(), 1);
    let first = app.selected.clone();

    type_text(&mut app, &fixture.client, "Create notes.md.").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session().is_some_and(|row| {
            row.status == Status::Waiting && row.waiting == Some(Wait::Permission)
        })
    })
    .await;
    assert_eq!(kyotoagent::tui::mode(&app), Mode::Permission);
    assert_eq!(
        key(
            press(KeyCode::Char('a')),
            kyotoagent::tui::mode(&app),
            false
        ),
        Some(Effect::AllowOnce)
    );

    new_here(&mut app, &fixture.client).await;
    assert_eq!(app.sessions.len(), 2);
    let second = app.selected.clone();
    assert_ne!(second, first);

    type_text(&mut app, &fixture.client, "Finish this.").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected == second
            && app
                .selected_session()
                .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;

    let first_row = app
        .sessions
        .iter()
        .find(|row| row.id == first)
        .expect("the first session stays in the list");
    assert_eq!(first_row.status, Status::Waiting);
    assert_eq!(first_row.waiting, Some(Wait::Permission));

    let pane = draw(&app);
    assert!(
        pane.contains("Created notes.md."),
        "the right pane shows the finished session: {pane}"
    );
    assert!(
        pane.contains("waiting permission"),
        "the waiting row stays on the list: {pane}"
    );

    drive(&mut app, &fixture.client, ctrl('n')).await;
    wait_until(&mut app, &fixture.client, |app| app.selected == first).await;
    assert_eq!(kyotoagent::tui::mode(&app), Mode::Permission);
    assert!(
        kinds(&app).contains(&"permission"),
        "the waiting session still shows its permission: {:?}",
        kinds(&app)
    );

    drive(&mut app, &fixture.client, press(KeyCode::Char('a'))).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected == first
            && app
                .selected_session()
                .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
            && kinds(app).contains(&"proof")
    })
    .await;

    let pane = draw(&app);
    assert!(
        pane.contains("Created notes.md."),
        "the result card is on screen: {pane}"
    );
    assert!(
        pane.contains("cargo test passed."),
        "the proof card is on screen: {pane}"
    );
}

#[test]
fn permission_keys_are_decisions_and_idle_types_them() {
    assert_eq!(
        key(press(KeyCode::Char('a')), Mode::Permission, false),
        Some(Effect::AllowOnce)
    );
    assert_eq!(
        key(press(KeyCode::Char('s')), Mode::Permission, false),
        Some(Effect::AllowSession)
    );
    assert_eq!(
        key(press(KeyCode::Char('d')), Mode::Permission, false),
        Some(Effect::Deny)
    );
    assert_eq!(
        key(press(KeyCode::Char('a')), Mode::Idle, false),
        Some(Effect::Type('a'))
    );
}

#[test]
fn ctrl_c_exits_and_ctrl_x_cancels() {
    assert_eq!(key(ctrl('c'), Mode::Permission, false), Some(Effect::Exit));
    assert_eq!(key(ctrl('x'), Mode::Working, false), Some(Effect::Cancel));
    assert_eq!(key(ctrl('n'), Mode::Idle, false), Some(Effect::SelectNext));
    assert_eq!(key(ctrl('p'), Mode::Idle, false), Some(Effect::SelectPrev));
}

#[test]
fn a_broken_pipe_is_fatal_and_a_plain_io_error_is_not() {
    assert!(loop_error_is_fatal(
        &std::io::Error::from(std::io::ErrorKind::BrokenPipe).to_string()
    ));
    assert!(!loop_error_is_fatal(
        &std::io::Error::other("the view body was not json").to_string()
    ));
}

fn question_overlay_text(app: &App) -> Option<String> {
    match screen_model(app).overlay {
        Some(Overlay::Question { text, .. }) => Some(text),
        _ => None,
    }
}

async fn waiting_question_card_id(client: &Client, id: &str) -> String {
    let (status, body) = client
        .request("GET", &format!("/v1/sessions/{id}/view"), None)
        .await
        .expect("the view loads");
    assert_eq!(status, 200);
    let view: View = serde_json::from_str(&body).expect("the view parses");
    view.cards
        .iter()
        .rev()
        .find_map(|card| {
            if card.kind != CardKind::Question {
                return None;
            }
            let answered = card
                .body
                .get("answer")
                .map(|value| !value.is_null())
                .unwrap_or(false);
            if answered {
                None
            } else {
                Some(
                    card.body
                        .get("eventId")
                        .and_then(Value::as_str)
                        .unwrap_or(card.id.as_str())
                        .to_string(),
                )
            }
        })
        .expect("a waiting question card")
}

fn overlay_click(app: &App) -> MouseEvent {
    let model = screen_model(app);
    let area = Rect::new(0, 0, 76, 24);
    let mut found = None;
    for y in 0..24u16 {
        for x in 0..76u16 {
            if kyotoagent::screen::overlay_card_at(&model, area, x, y) {
                found = Some((x, y));
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    let (column, row) = found.expect("a waiting question is on screen");
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

async fn start_question(name: &str, choices: &[&str]) -> (Fixture, App) {
    start_question_text(name, "Which way?", choices).await
}

async fn start_question_text(name: &str, text: &str, choices: &[&str]) -> (Fixture, App) {
    let fixture = Fixture::with_replies(name, ask_then_finish(text, choices)).await;
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    type_text(&mut app, &fixture.client, "Name the binary.").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Waiting && row.waiting == Some(Wait::Question))
    })
    .await;
    (fixture, app)
}

async fn question_answer(client: &Client, id: &str) -> Option<String> {
    let (status, body) = client
        .request("GET", &format!("/v1/sessions/{id}/view"), None)
        .await
        .expect("the view loads");
    assert_eq!(status, 200);
    let view: View = serde_json::from_str(&body).expect("the view parses");
    let log = log_text(client, id).await;
    let logged = log.lines().rev().find_map(|line| {
        let event: Value = serde_json::from_str(line).ok()?;
        (event["kind"].as_str() == Some("question_answer"))
            .then(|| event["body"]["answer"].as_str().map(str::to_string))
            .flatten()
    });
    let question = view
        .cards
        .iter()
        .rev()
        .find(|card| card.kind == CardKind::Question)
        .expect("the question stays");
    assert!(
        question.body["choices"]
            .as_array()
            .expect("choices")
            .is_empty(),
        "the answered question drops its choices"
    );
    assert_eq!(question.body["answer"].as_str(), logged.as_deref());
    let answer = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Answer)
        .expect("the answer card");
    assert_eq!(answer.body["text"].as_str(), logged.as_deref());
    assert_ne!(answer.body["text"].as_str(), Some("1"));
    logged
}

#[tokio::test]
async fn a_number_picks_the_listed_choice_while_the_prompt_is_empty() {
    let (fixture, mut app) = start_question("question-number", &["continue", "stop"]).await;
    assert_eq!(kyotoagent::tui::mode(&app), Mode::Question { choices: 2 });
    let model = screen_model(&app);
    assert_eq!(model.bottom_kind, kyotoagent::screen::Bottom::Prompt);
    assert_eq!(model.bottom, "");
    let pane = draw(&app);
    assert!(
        pane.contains("1  continue"),
        "choices stay on the card: {pane}"
    );
    assert!(pane.contains("2  stop"), "choices stay on the card: {pane}");
    assert!(
        pane.lines()
            .last()
            .expect("a bottom row")
            .trim()
            .ends_with('\u{2588}'),
        "the prompt keeps the cursor: {pane}"
    );

    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    let id = app.selected.clone();
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    assert_eq!(
        question_answer(&fixture.client, &id).await.as_deref(),
        Some("continue")
    );
}

#[tokio::test]
async fn a_numbered_pick_shows_the_choice_label_on_the_right() {
    let (fixture, mut app) = start_question_text(
        "question-label",
        "Which title should the heading use?",
        &["Kyoto Agent", "Kyoto Agent CLI"],
    )
    .await;
    assert!(question_overlay_text(&app).is_some());
    let waiting = draw(&app);
    assert!(
        waiting.contains("Which title should the heading use?"),
        "{waiting}"
    );
    assert!(waiting.contains("1  Kyoto Agent"), "{waiting}");
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    let id = app.selected.clone();
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"answer")
    })
    .await;
    assert_eq!(
        question_answer(&fixture.client, &id).await.as_deref(),
        Some("Kyoto Agent")
    );
    let pane = draw(&app);
    let question = pane
        .lines()
        .find(|line| line.contains("QUESTION"))
        .expect("the question stays on the left");
    let answer = pane
        .lines()
        .find(|line| line.contains("ANSWER"))
        .expect("the answer card");
    assert!(answer.find("ANSWER").unwrap() > question.find("QUESTION").unwrap());
    let label = pane
        .lines()
        .find(|line| line.contains("Kyoto Agent ▎"))
        .expect("the label");
    assert!(label.find("Kyoto Agent").unwrap() > question.find("QUESTION").unwrap());
    assert!(!label.contains('1'), "{label}");
    assert!(!label.contains('\u{25cf}'), "{label}");
    assert!(!answer.contains('1'), "{answer}");
}

#[tokio::test]
async fn typed_text_that_is_not_in_the_list_is_the_choice() {
    let (fixture, mut app) = start_question("question-typed", &["continue", "stop"]).await;
    match screen_model(&app).overlay {
        Some(Overlay::Question { .. }) => {}
        other => panic!("expected a question overlay, got {other:?}"),
    }
    type_text(&mut app, &fixture.client, "hello").await;
    assert_eq!(kyotoagent::tui::mode(&app), Mode::QuestionText);
    assert_eq!(screen_model(&app).bottom, "hello");
    assert_eq!(
        key(press(KeyCode::Char('1')), kyotoagent::tui::mode(&app), true),
        Some(Effect::Type('1'))
    );
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    let id = app.selected.clone();
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    assert_eq!(
        question_answer(&fixture.client, &id).await.as_deref(),
        Some("hello")
    );
    let pane = draw(&app);
    assert!(pane.contains("Which way?"), "{pane}");
    let question = pane
        .lines()
        .find(|line| line.contains("QUESTION"))
        .expect("the question");
    let label = pane
        .lines()
        .find(|line| line.contains("hello"))
        .expect("the typed answer");
    assert!(label.find("hello").unwrap() > question.find("QUESTION").unwrap());
    assert!(!label.contains('1'), "{label}");
}

#[tokio::test]
async fn an_empty_choice_list_still_uses_the_prompt() {
    let (fixture, mut app) = start_question("question-empty", &[]).await;
    assert_eq!(kyotoagent::tui::mode(&app), Mode::QuestionText);
    assert_eq!(
        screen_model(&app).bottom_kind,
        kyotoagent::screen::Bottom::Prompt
    );
    match screen_model(&app).overlay {
        Some(Overlay::Question { .. }) => {}
        other => panic!("expected a question overlay, got {other:?}"),
    }
    type_text(&mut app, &fixture.client, "free text").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    let id = app.selected.clone();
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    assert_eq!(
        question_answer(&fixture.client, &id).await.as_deref(),
        Some("free text")
    );
}

#[tokio::test]
async fn a_waiting_question_opens_the_overlay_on_poll() {
    let (fixture, app) = start_question("question-auto-open", &["continue", "stop"]).await;
    let text = question_overlay_text(&app).expect("the overlay opens on poll");
    assert!(text.contains("Which way?"), "{text}");
    let pane = draw(&app);
    assert!(pane.contains("Which way?"), "{pane}");
    let id = app.selected.clone();
    let card_id = waiting_question_card_id(&fixture.client, &id).await;
    assert!(!card_id.is_empty());
}

#[tokio::test]
async fn esc_leaves_the_same_question_closed_on_poll() {
    let (fixture, mut app) = start_question("question-esc-sticky", &["continue", "stop"]).await;
    assert!(question_overlay_text(&app).is_some());
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    assert!(question_overlay_text(&app).is_none());
    poll(&mut app, &fixture.client)
        .await
        .expect("the same question polls");
    assert!(question_overlay_text(&app).is_none());
    poll(&mut app, &fixture.client)
        .await
        .expect("a later poll of the same question");
    assert!(question_overlay_text(&app).is_none());
    assert_eq!(kyotoagent::tui::mode(&app), Mode::Question { choices: 2 });
}

#[tokio::test]
async fn click_or_enter_reopens_a_dismissed_question() {
    let (fixture, mut app) = start_question("question-reopen", &["continue", "stop"]).await;
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    poll(&mut app, &fixture.client)
        .await
        .expect("the dismissed question stays closed");
    assert!(question_overlay_text(&app).is_none());

    let event = overlay_click(&app);
    let effect = released_click(
        &screen_model(&app),
        Rect::new(0, 0, 76, 24),
        event.column,
        event.row,
    )
    .expect("a click on the card opens the overlay");
    apply(&mut app, &fixture.client, effect)
        .await
        .expect("the click applies");
    let text = question_overlay_text(&app).expect("click opens it again");
    assert!(text.contains("Which way?"), "{text}");

    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    assert!(question_overlay_text(&app).is_none());
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    let text = question_overlay_text(&app).expect("Enter opens it again");
    assert!(text.contains("Which way?"), "{text}");
}

#[tokio::test]
async fn a_new_question_id_opens_the_overlay_on_poll() {
    let fixture = Fixture::with_replies("question-new-id", two_asks_then_finish()).await;
    let mut app = fixture.app();
    start_session(&mut app, &fixture.client, "Name the binary.").await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Waiting && row.waiting == Some(Wait::Question))
    })
    .await;
    let id = app.selected.clone();
    let first = waiting_question_card_id(&fixture.client, &id).await;
    let text = question_overlay_text(&app).expect("the first question opens");
    assert!(text.contains("Which way?"), "{text}");

    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    poll(&mut app, &fixture.client)
        .await
        .expect("the first question stays closed");
    assert!(question_overlay_text(&app).is_none());

    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Waiting && row.waiting == Some(Wait::Question))
            && question_overlay_text(app).is_some_and(|text| text.contains("And then?"))
    })
    .await;
    let second = waiting_question_card_id(&fixture.client, &id).await;
    assert_ne!(first, second);
    let text = question_overlay_text(&app).expect("a new question opens on poll");
    assert!(text.contains("And then?"), "{text}");
}

async fn log_text(client: &Client, id: &str) -> String {
    let (status, body) = client
        .request("GET", &format!("/v1/sessions/{id}/events"), None)
        .await
        .expect("the log loads");
    assert_eq!(status, 200);
    body
}

fn decisions(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| {
            let event: Value = serde_json::from_str(line).ok()?;
            if event["kind"].as_str() == Some("permission_answer") {
                event["body"]["decision"].as_str().map(str::to_string)
            } else {
                None
            }
        })
        .collect()
}

async fn start_session(app: &mut App, client: &Client, ask: &str) {
    poll(app, client).await.expect("an empty list polls");
    new_here(app, client).await;
    type_text(app, client, ask).await;
    drive(app, client, press(KeyCode::Enter)).await;
}

#[tokio::test]
async fn a_new_background_task_preserves_panes_and_shows_its_tail_when_opened() {
    let fixture = Fixture::with_replies(
        "task-tail",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "start_task",
                serde_json::json!({
                    "argv": ["sh", "-c", "echo hello-from-task; sleep 30"]
                }),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "text": "Started.", "proof": "cargo test passed." }),
            )])),
        ],
    )
    .await;
    let mut app = fixture.app();
    app.start_yolo = true;
    start_session(&mut app, &fixture.client, "Start a sleeper.").await;
    wait_until(&mut app, &fixture.client, |app| {
        screen_model(app)
            .open_task
            .as_ref()
            .is_some_and(|task| task.tail.contains("hello-from-task"))
    })
    .await;
    assert!(!app.right_open);
    type_text(&mut app, &fixture.client, "/tasks").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    let drawn = draw(&app);
    assert!(drawn.contains("hello-from-task"), "{drawn}");
    let model = screen_model(&app);
    let split = kyotoagent::screen::split_of(&model, Rect::new(0, 0, 76, 24));
    assert_eq!(split.input.y, split.session.y + split.session.height);
}

#[tokio::test]
async fn yolo_allows_a_write_without_a_keypress() {
    let fixture = Fixture::new("yolo-write").await;
    let mut app = fixture.app();
    app.start_yolo = true;
    start_session(&mut app, &fixture.client, "Create notes.md.").await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    let notes = fixture.workspace.join("notes.md");
    assert_eq!(
        fs::read_to_string(&notes).expect("the file is written"),
        "hello"
    );
    let id = app.selected.clone();
    let log = log_text(&fixture.client, &id).await;
    assert!(
        log.contains("permission"),
        "serve still records the permission: {log}"
    );
    assert_eq!(decisions(&log), vec!["allow_once".to_string()]);
    assert!(draw(&app).contains("yolo"), "{}", draw(&app));
}

#[tokio::test]
async fn toggling_yolo_off_leaves_the_next_write_waiting() {
    let fixture = Fixture::new("yolo-toggle").await;
    let mut app = fixture.app();
    app.start_yolo = true;
    start_session(&mut app, &fixture.client, "Create notes.md.").await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"proof")
    })
    .await;

    drive(&mut app, &fixture.client, ctrl('y')).await;
    assert!(!app.yolo);
    assert!(!draw(&app).contains("yolo"));

    fixture._fake.set_replies(write_file_then_finish(
        "other.md",
        "two",
        "Created other.md.",
    ));
    type_text(&mut app, &fixture.client, "Create other.md.").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session().is_some_and(|row| {
            row.status == Status::Waiting && row.waiting == Some(Wait::Permission)
        })
    })
    .await;
    assert!(!fixture.workspace.join("other.md").exists());
    drive(&mut app, &fixture.client, press(KeyCode::Char('a'))).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("other.md")).expect("the second file is written"),
        "two"
    );
}

#[tokio::test]
async fn slash_yolo_on_unblocks_the_waiting_write() {
    let fixture = Fixture::new("slash-yolo-on").await;
    let mut app = fixture.app();
    start_session(&mut app, &fixture.client, "Create notes.md.").await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session().is_some_and(|row| {
            row.status == Status::Waiting && row.waiting == Some(Wait::Permission)
        })
    })
    .await;
    assert!(!fixture.workspace.join("notes.md").exists());
    type_text(&mut app, &fixture.client, "/yolo on").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    assert!(app.yolo);
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("notes.md")).expect("the file is written"),
        "hello"
    );
    let mut again = fixture.app();
    poll(&mut again, &fixture.client)
        .await
        .expect("the second attach polls");
    assert!(again.yolo);
    assert!(!again.start_yolo);
}

#[tokio::test]
async fn slash_yolo_now_is_an_ask() {
    let fixture = Fixture::with_replies("slash-yolo-now", finish_only("done")).await;
    let mut app = idle_session(&fixture).await;
    type_text(&mut app, &fixture.client, "/yolo now").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    assert!(!app.yolo);
    assert!(kinds(&app).contains(&"ask"));
}

#[tokio::test]
async fn a_question_still_waits_in_yolo() {
    let fixture = Fixture::with_replies(
        "yolo-question",
        ask_then_finish("Which way?", &["continue", "stop"]),
    )
    .await;
    let mut app = fixture.app();
    app.start_yolo = true;
    start_session(&mut app, &fixture.client, "Name the binary.").await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Waiting && row.waiting == Some(Wait::Question))
    })
    .await;
    assert_eq!(kyotoagent::tui::mode(&app), Mode::Question { choices: 2 });
    assert_eq!(
        app.selected_session().map(|row| row.waiting),
        Some(Some(Wait::Question))
    );
    match screen_model(&app).overlay {
        Some(Overlay::Question { text, .. }) => {
            assert!(text.contains("Which way?"), "{text}");
        }
        other => panic!("expected a question overlay under yolo, got {other:?}"),
    }
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
}

#[tokio::test]
async fn enter_opens_the_overlay_and_esc_leaves_the_permission_waiting() {
    let fixture = Fixture::new("overlay-esc").await;
    let mut app = fixture.app();
    start_session(&mut app, &fixture.client, "Create notes.md.").await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session().is_some_and(|row| {
            row.status == Status::Waiting && row.waiting == Some(Wait::Permission)
        })
    })
    .await;
    assert!(!app.overlay);
    assert!(screen_model(&app).overlay.is_none());
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(app.overlay);
    match screen_model(&app).overlay {
        Some(Overlay::Permission { action, diff, .. }) => {
            assert!(action.contains("notes.md"), "{action}");
            assert!(diff.iter().any(|line| line.contains("hello")), "{diff:?}");
        }
        other => panic!("expected a permission overlay, got {other:?}"),
    }
    let pane = draw(&app);
    assert!(pane.contains("hello"), "{pane}");
    assert!(pane.contains("a once"), "{pane}");
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    assert!(!app.overlay);
    assert_eq!(kyotoagent::tui::mode(&app), Mode::Permission);
    assert!(!fixture.workspace.join("notes.md").exists());
}

#[tokio::test]
async fn overlay_deny_stops_a_command() {
    let fixture = Fixture::with_replies("overlay-deny", run_then_finish()).await;
    let mut app = fixture.app();
    start_session(&mut app, &fixture.client, "Touch started.").await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session().is_some_and(|row| {
            row.status == Status::Waiting && row.waiting == Some(Wait::Permission)
        })
    })
    .await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    let pane = draw(&app);
    assert!(pane.contains("touch"), "{pane}");
    assert!(pane.contains("started"), "{pane}");
    drive(&mut app, &fixture.client, press(KeyCode::Char('d'))).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
    })
    .await;
    assert!(!fixture.workspace.join("started").exists());
    assert!(!app.overlay);
}

#[test]
fn a_click_on_a_waiting_card_opens_the_overlay() {
    let model = kyotoagent::mock::waiting();
    let area = Rect::new(0, 0, 76, 24);
    let mut found = None;
    for y in 0..24u16 {
        for x in 0..76u16 {
            if kyotoagent::screen::waiting_card_at(&model, area, x, y) {
                found = Some((x, y));
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    let (column, row) = found.expect("the waiting permission is on screen");
    assert_eq!(
        released_click(&model, area, column, row),
        Some(Effect::OpenOverlay)
    );
    let idle = kyotoagent::mock::idle();
    assert_eq!(released_click(&idle, area, column, row), None);
}

#[test]
fn yolo_off_and_overlay_closed_keep_the_quiet_frames() {
    let waiting = kyotoagent::mock::waiting();
    assert!(!waiting.yolo);
    assert!(waiting.overlay.is_none());
    let idle = kyotoagent::mock::idle();
    assert!(!idle.yolo);
    assert!(idle.overlay.is_none());
    let working = kyotoagent::mock::working();
    assert!(!working.yolo);
    assert!(working.overlay.is_none());
}

fn idle_proof_app() -> App {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions.push(kyotoagent::screen::SessionRow {
        id: "91bc7a1d".into(),
        workspace: PathBuf::from("/w"),
        title: None,
        status: Status::Idle,
        waiting: None,
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
    });
    app.cards.push(Card::Proof {
        text: "cargo test passed on the readme heading.".into(),
        items: vec![
            ItemRun {
                id: "test".into(),
                kind: ItemKind::Command,
                outcome: Outcome::Passed,
                argv: Vec::new(),
                exit: None,
                tail: String::new(),
            },
            ItemRun {
                id: "lint".into(),
                kind: ItemKind::Command,
                outcome: Outcome::Failed,
                argv: vec!["cargo".into(), "clippy".into()],
                exit: Some(1),
                tail: "error: unused".into(),
            },
        ],
    });
    app
}

#[tokio::test]
async fn enter_on_a_proof_opens_the_overlay_and_esc_closes_it() {
    let mut app = idle_proof_app();
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    drive(&mut app, &client, press(KeyCode::Enter)).await;
    assert!(app.overlay);
    match screen_model(&app).overlay {
        Some(Overlay::Proof { items, .. }) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].id, "test");
            assert_eq!(items[0].outcome, Outcome::Passed);
            assert_eq!(items[1].id, "lint");
            assert_eq!(items[1].outcome, Outcome::Failed);
            assert_eq!(items[1].argv, vec!["cargo", "clippy"]);
            assert_eq!(items[1].tail, "error: unused");
        }
        other => panic!("expected a proof overlay, got {other:?}"),
    }
    let pane = draw(&app);
    assert!(pane.contains("cargo test passed on the"), "{pane}");
    assert!(pane.contains("readme heading."), "{pane}");
    assert!(!pane.contains("error: unused"), "{pane}");
    assert!(!pane.contains("cargo clippy"), "{pane}");
    drive(&mut app, &client, press(KeyCode::Esc)).await;
    assert!(!app.overlay);
    assert!(screen_model(&app).overlay.is_none());
    let quiet = draw(&app);
    assert!(quiet.contains("cargo test passed on the"), "{quiet}");
    assert!(quiet.contains("readme heading."), "{quiet}");
    assert!(!quiet.contains("error: unused"), "{quiet}");
    assert!(!quiet.contains("M README.md"), "{quiet}");
    assert!(!quiet.contains("README.md | 1 +"), "{quiet}");
}

#[tokio::test]
async fn enter_on_a_proof_sentence_opens_it_and_a_blank_proof_stays_closed() {
    let mut app = idle_proof_app();
    app.cards = vec![Card::proof("cargo test passed.", &[])];
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    drive(&mut app, &client, press(KeyCode::Enter)).await;
    assert!(app.overlay);
    match screen_model(&app).overlay {
        Some(Overlay::Proof { text, items }) => {
            assert_eq!(text, "cargo test passed.");
            assert!(items.is_empty());
        }
        other => panic!("expected the proof sentence, got {other:?}"),
    }
    let pane = draw(&app);
    assert!(pane.contains("cargo test passed."), "{pane}");
    assert!(!pane.contains("cargo clippy"), "{pane}");
    app.overlay = false;
    app.cards = vec![Card::proof(
        "",
        &[("test", ItemKind::Command, Outcome::Passed)],
    )];
    drive(&mut app, &client, press(KeyCode::Enter)).await;
    assert!(!app.overlay);
    assert!(screen_model(&app).overlay.is_none());
}

#[test]
fn a_click_on_a_proof_card_opens_the_overlay() {
    let model = kyotoagent::mock::idle();
    let area = Rect::new(0, 0, 76, 24);
    let mut found = None;
    for y in 0..24u16 {
        for x in 0..76u16 {
            if kyotoagent::screen::overlay_card_at(&model, area, x, y) {
                found = Some((x, y));
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    let (column, row) = found.expect("the proof card is on screen");
    assert_eq!(
        released_click(&model, area, column, row),
        Some(Effect::OpenOverlay)
    );
    let mut text_only = kyotoagent::mock::idle();
    text_only.cards = vec![Card::proof("cargo test passed.", &[])];
    let mut text_hit = None;
    for y in 0..24u16 {
        for x in 0..76u16 {
            if kyotoagent::screen::overlay_card_at(&text_only, area, x, y) {
                text_hit = Some((x, y));
                break;
            }
        }
        if text_hit.is_some() {
            break;
        }
    }
    let (column, row) = text_hit.expect("a proof sentence is clickable");
    assert_eq!(
        released_click(&text_only, area, column, row),
        Some(Effect::OpenOverlay)
    );
    let mut blank = kyotoagent::mock::idle();
    blank.cards = vec![Card::proof(
        "",
        &[("test", ItemKind::Command, Outcome::Passed)],
    )];
    for y in 0..24u16 {
        for x in 0..76u16 {
            assert!(
                !kyotoagent::screen::overlay_card_at(&blank, area, x, y),
                "a proof with no text stays on the pane"
            );
        }
    }
}

fn sample_pull() -> String {
    format!(
        "{}/pmdroid/kyotoagent/pull/14",
        kyotoagent::session::github_origin()
    )
}

#[tokio::test]
async fn enter_on_a_session_with_a_pull_opens_the_overlay() {
    let mut app = idle_proof_app();
    app.left_width = 20;
    app.cards = vec![Card::proof("cargo test passed.", &[])];
    let url = sample_pull();
    app.sessions[0].pull_url = Some(url.clone());
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    drive(&mut app, &client, press(KeyCode::Enter)).await;
    match screen_model(&app).overlay {
        Some(Overlay::Pull { url: shown }) => assert_eq!(shown, url),
        other => panic!("expected a pull overlay, got {other:?}"),
    }
    let pane = draw(&app);
    assert!(pane.contains(&url), "{pane}");
    assert!(pane.contains("pr 14"), "{pane}");
    drive(&mut app, &client, press(KeyCode::Esc)).await;
    assert!(!app.overlay);
    assert!(screen_model(&app).overlay.is_none());
    let quiet = draw(&app);
    assert!(quiet.contains("pr 14"), "{quiet}");
    assert!(quiet.contains("Opened"), "{quiet}");
}

#[test]
fn a_click_on_the_pull_title_opens_the_overlay() {
    let mut model = kyotoagent::mock::idle();
    let url = sample_pull();
    model.sessions[0].pull_url = Some(url);
    let area = Rect::new(0, 0, 76, 24);
    let mut found = None;
    for y in 0..24u16 {
        for x in 0..76u16 {
            if kyotoagent::screen::pull_target_at(&model, area, x, y) {
                found = Some((x, y));
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    let (column, row) = found.expect("the title or opened line is on screen");
    let event = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(mouse(event, &model, area), Some(Effect::OpenPull));
    let idle = kyotoagent::mock::idle();
    assert_eq!(mouse(event, &idle, area), None);
}

fn list_click(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn list_inner(model: &kyotoagent::screen::ScreenModel, area: Rect) -> Rect {
    let list = kyotoagent::screen::split_of(model, area).list;
    Rect {
        x: list.x.saturating_add(1),
        y: list.y.saturating_add(1),
        width: list.width.saturating_sub(2),
        height: list.height.saturating_sub(2),
    }
}

#[test]
fn session_at_hits_each_three_row_block_on_the_mock_list() {
    let model = kyotoagent::mock::idle();
    let area = Rect::new(0, 0, 76, 24);
    let inner = list_inner(&model, area);
    let x = inner.x + 2;
    let ids = ["91bc7a1d", "3f2ae04c", "ab1029f6"];
    for (index, id) in ids.iter().enumerate() {
        let top = inner.y + (index as u16) * 3;
        for row in [top, top + 1, top + 2] {
            assert_eq!(
                kyotoagent::screen::session_at(&model, area, x, row).as_deref(),
                Some(*id),
                "row {row} should select {id}"
            );
            assert_eq!(
                mouse(list_click(x, row), &model, area),
                Some(Effect::SelectSession(id.to_string())),
                "click on row {row} selects {id}"
            );
        }
    }
    assert_eq!(
        mouse(list_click(x, inner.y), &model, area),
        Some(Effect::SelectSession("91bc7a1d".into()))
    );
    let empty_y = inner.y + 9;
    assert_eq!(
        kyotoagent::screen::session_at(&model, area, x, empty_y),
        None
    );
    assert_eq!(mouse(list_click(x, empty_y), &model, area), None);
    let hint_y = inner.y + inner.height - 1;
    assert_eq!(
        kyotoagent::screen::session_at(&model, area, x, hint_y),
        None
    );
    assert_eq!(mouse(list_click(x, hint_y), &model, area), None);
}

fn right_click(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn a_right_click_on_a_session_opens_close_and_a_worktree_row_adds_remove() {
    let model = kyotoagent::mock::idle();
    let area = Rect::new(0, 0, 76, 24);
    let inner = list_inner(&model, area);
    let x = inner.x + 2;
    let y = inner.y;
    assert_eq!(
        mouse(right_click(x, y), &model, area),
        Some(Effect::OpenMenu {
            id: "91bc7a1d".into(),
            column: x,
            row: y,
        })
    );
    let session = kyotoagent::screen::split_of(&model, area).session;
    assert_eq!(
        mouse(right_click(session.x + 2, session.y + 2), &model, area),
        None
    );
    let mut with_todos = model.clone();
    with_todos.right_open = true;
    with_todos
        .right_panes
        .insert(kyotoagent::screen::RightPane::Todos);
    with_todos.todos.push(kyotoagent::events::TodoItem {
        id: "t1".into(),
        title: "Ship".into(),
        status: kyotoagent::events::TodoStatus::Pending,
        description: None,
        files: Vec::new(),
        links: Vec::new(),
    });
    let todos = kyotoagent::screen::split_of(&with_todos, area).todos;
    assert!(todos.width > 1, "the right stack is open");
    assert_eq!(
        mouse(right_click(todos.x + 1, todos.y + 1), &with_todos, area),
        None
    );

    let mut app = App::new(
        PathBuf::from("/work"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions = model.sessions.clone();
    app.area = area;
    apply_pane(
        &mut app,
        Effect::OpenMenu {
            id: "91bc7a1d".into(),
            column: x,
            row: y,
        },
    );
    match screen_model(&app).overlay {
        Some(Overlay::Menu { items, id, .. }) => {
            assert_eq!(id, "91bc7a1d");
            assert_eq!(items, vec!["Delete session".to_string()]);
        }
        other => panic!("expected a close menu, got {other:?}"),
    }
    app.sessions[0].worktree = true;
    apply_pane(
        &mut app,
        Effect::OpenMenu {
            id: "91bc7a1d".into(),
            column: x,
            row: y,
        },
    );
    match screen_model(&app).overlay {
        Some(Overlay::Menu { items, .. }) => {
            assert_eq!(items, vec!["Delete session".to_string()]);
        }
        other => panic!("expected both menu items, got {other:?}"),
    }
    let menu = screen_model(&app);
    let item_x = x.saturating_add(1);
    let item_y = y.saturating_add(1);
    let hit = kyotoagent::screen::menu_item_at(&menu, area, item_x, item_y);
    assert_eq!(hit, Some(0));
    assert_eq!(
        mouse(list_click(item_x, item_y), &menu, area),
        Some(Effect::MenuItem(0))
    );
    assert_eq!(
        mouse(list_click(x, y), &model, area),
        Some(Effect::SelectSession("91bc7a1d".into()))
    );
}

#[tokio::test]
async fn closing_the_selected_session_selects_the_next_row() {
    let fixture = Fixture::new("close-next").await;
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    let other = fixture.root.join("other");
    fs::create_dir_all(&other).expect("the other workspace exists");
    let create =
        |workspace: &std::path::Path| serde_json::json!({ "workspace": workspace }).to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&create(&fixture.workspace)))
        .await
        .expect("the first session is posted");
    assert_eq!(status, 201, "{response}");
    let first: Value = serde_json::from_str(&response).expect("json");
    let first = first["id"].as_str().expect("id").to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&create(&other)))
        .await
        .expect("the second session is posted");
    assert_eq!(status, 201, "{response}");
    let second: Value = serde_json::from_str(&response).expect("json");
    let second = second["id"].as_str().expect("id").to_string();
    poll(&mut app, &fixture.client)
        .await
        .expect("both sessions poll");
    assert_eq!(app.sessions[0].id, second);
    assert_eq!(app.sessions[1].id, first);
    app.selected = second.clone();
    apply(
        &mut app,
        &fixture.client,
        Effect::OpenMenu {
            id: second.clone(),
            column: 2,
            row: 2,
        },
    )
    .await
    .expect("the menu opens");
    apply(&mut app, &fixture.client, Effect::MenuItem(0))
        .await
        .expect("the confirm opens");
    assert!(app.sessions.iter().any(|row| row.id == second));
    assert!(fixture.root.join("sessions").join(&second).exists());
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(app.sessions.iter().all(|row| row.id != second));
    assert_eq!(app.selected, first);
    assert!(!fixture.root.join("sessions").join(&second).exists());

    drive(&mut app, &fixture.client, ctrl('k')).await;
    type_text(&mut app, &fixture.client, "delete session").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(app.sessions.iter().any(|row| row.id == first));
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(app.sessions.is_empty());
    assert!(app.selected.is_empty());
    assert!(!fixture.root.join("sessions").join(&first).exists());
}

#[tokio::test]
async fn ctrl_w_asks_before_delete_and_names_a_worktree() {
    let fixture = Fixture::new("delete-confirm").await;
    let mut app = fixture.app();
    let dir = fixture.root.join("notes");
    fs::create_dir_all(&dir).expect("the workspace exists");
    git_init(&dir);
    let body = serde_json::json!({ "workspace": dir, "worktree": true }).to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&body))
        .await
        .expect("the session is posted");
    assert_eq!(status, 201, "{response}");
    let created: Value = serde_json::from_str(&response).expect("json");
    let id = created["id"].as_str().expect("id").to_string();
    poll(&mut app, &fixture.client)
        .await
        .expect("the session polls");
    app.selected = id.clone();
    let workspace = app.sessions[0].workspace.clone();
    assert_eq!(
        key(ctrl('w'), Mode::Idle, false),
        Some(Effect::ConfirmDelete)
    );
    assert_eq!(key(ctrl('w'), Mode::Idle, true), None);
    drive(&mut app, &fixture.client, ctrl('w')).await;
    match screen_model(&app).overlay {
        Some(Overlay::Delete {
            path,
            highlight,
            remove_workspace,
            ..
        }) => {
            assert_eq!(path.as_deref(), workspace.to_str());
            assert_eq!(highlight, 1);
            assert!(!remove_workspace);
        }
        other => panic!("expected the delete confirm, got {other:?}"),
    }
    let model = screen_model(&app);
    let backend = TestBackend::new(76, 24);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| kyotoagent::screen::render(&model, frame.area(), frame))
        .expect("the confirm draws");
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
    }
    assert!(text.contains("Delete this session?"), "{text}");
    assert!(text.contains("Also delete workspace"), "{text}");
    assert!(!text.contains("That directory will be removed."), "{text}");
    assert!(fixture.root.join("sessions").join(&id).exists());
    drive(&mut app, &fixture.client, ctrl('k')).await;
    assert!(keystroke(&app, ctrl('w')).is_none());
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    drive(&mut app, &fixture.client, ctrl('w')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    assert!(screen_model(&app).overlay.is_none());
    assert!(fixture.root.join("sessions").join(&id).exists());
    drive(&mut app, &fixture.client, ctrl('w')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Down)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(fixture.root.join("sessions").join(&id).exists());
    drive(&mut app, &fixture.client, ctrl('w')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(!fixture.root.join("sessions").join(&id).exists());
    assert!(workspace.exists());
}

#[test]
fn a_click_on_the_list_border_still_starts_a_drag() {
    let model = kyotoagent::mock::idle();
    let area = Rect::new(0, 0, 76, 24);
    let list = kyotoagent::screen::split_of(&model, area).list;
    let border_x = list.x + list.width - 1;
    let row = list.y + 2;
    assert_eq!(
        kyotoagent::screen::session_at(&model, area, border_x, row),
        None
    );
    assert_eq!(
        mouse(list_click(border_x, row), &model, area),
        Some(Effect::DragLeft(border_x))
    );
}

#[test]
fn a_click_on_the_collapsed_rail_opens_the_list() {
    let mut model = kyotoagent::mock::idle();
    model.left_open = false;
    let area = Rect::new(0, 0, 76, 24);
    let list = kyotoagent::screen::split_of(&model, area).list;
    let column = list.x;
    let row = list.y + 2;
    assert_eq!(
        kyotoagent::screen::session_at(&model, area, column, row),
        None
    );
    assert_eq!(
        mouse(list_click(column, row), &model, area),
        Some(Effect::ToggleLeft)
    );
}

fn todos_inner(model: &kyotoagent::screen::ScreenModel, area: Rect) -> Rect {
    let todos = kyotoagent::screen::split_of(model, area).todos;
    Rect {
        x: todos.x.saturating_add(1),
        y: todos.y.saturating_add(1),
        width: todos.width.saturating_sub(2),
        height: todos.height.saturating_sub(2),
    }
}

#[test]
fn todo_at_hits_each_inner_row_on_the_open_pane() {
    let model = kyotoagent::mock::todos();
    let area = Rect::new(0, 0, 76, 24);
    let inner = todos_inner(&model, area);
    let x = inner.x + 2;
    assert_eq!(kyotoagent::screen::todo_at(&model, area, x, inner.y), None);
    assert_eq!(mouse(list_click(x, inner.y), &model, area), None);
    let ids = ["read", "write", "draw"];
    for (index, id) in ids.iter().enumerate() {
        let row = inner.y + 1 + index as u16;
        assert_eq!(
            kyotoagent::screen::todo_at(&model, area, x, row).as_deref(),
            Some(*id),
            "row {row} is {id}"
        );
        assert_eq!(
            mouse(list_click(x, row), &model, area),
            Some(Effect::OpenTodo((*id).into())),
            "click on row {row} opens {id}"
        );
        assert_eq!(
            mouse(list_click(x, row), &model, area),
            Some(Effect::OpenTodo((*id).into())),
            "a second click routes to the same todo"
        );
    }
    assert_eq!(
        kyotoagent::screen::todo_at(&model, area, x, inner.y + 4),
        None
    );
    assert_eq!(mouse(list_click(x, inner.y + 4), &model, area), None);
    let todos = kyotoagent::screen::split_of(&model, area).todos;
    assert_eq!(kyotoagent::screen::todo_at(&model, area, x, todos.y), None);
    assert_eq!(
        kyotoagent::screen::todo_at(&model, area, todos.x, inner.y + 1),
        None
    );
    assert_eq!(
        mouse(list_click(todos.x, inner.y + 1), &model, area),
        Some(Effect::DragRight(todos.x))
    );
}

fn plant_skill(workspace: &std::path::Path, name: &str, description: &str, body: &str) {
    let dir = workspace.join(kyotoagent::skills::AGENTS_SKILLS).join(name);
    fs::create_dir_all(&dir).expect("the skill directory exists");
    fs::write(
        dir.join(kyotoagent::skills::SKILL_FILE),
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    )
    .expect("the skill is written");
}

#[tokio::test]
async fn the_slash_picker_reads_skills_from_the_view() {
    let fixture = Fixture::new("view-skill-picker").await;
    plant_skill(
        &fixture.workspace,
        "workspace-only-skill",
        "From the session workspace",
        "BODY",
    );
    let mut app = fixture.app();
    app.home = PathBuf::from("/tmp/kyotoagent-empty-tui-home-no-skills");
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    type_text(&mut app, &fixture.client, "/workspace-only").await;
    let pane = draw(&app);
    assert!(
        pane.contains("workspace-only-skill"),
        "the picker shows a workspace skill with an empty TUI home: {pane}"
    );
}

#[tokio::test]
async fn typing_a_slash_prefix_lists_skills_and_enter_fills_the_name() {
    let fixture = Fixture::new("slash-picker").await;
    plant_skill(
        &fixture.workspace,
        "preflight",
        "Ship checks",
        "PREFLIGHT BODY UNIQUE",
    );
    plant_skill(
        &fixture.workspace,
        "preview",
        "Preview a change",
        "PREVIEW BODY",
    );
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    type_text(&mut app, &fixture.client, "/pre").await;
    let pane = draw(&app);
    assert!(pane.contains("preflight"), "{pane}");
    assert!(pane.contains("preview"), "{pane}");
    assert!(pane.contains("Ship checks"), "{pane}");
    assert!(pane.contains("Preview a change"), "{pane}");
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert_eq!(app.ask, "/preflight ");
}

#[tokio::test]
async fn down_then_enter_fills_the_highlighted_skill() {
    let fixture = Fixture::new("slash-down").await;
    plant_skill(
        &fixture.workspace,
        "preflight",
        "Ship checks",
        "PREFLIGHT BODY UNIQUE",
    );
    plant_skill(
        &fixture.workspace,
        "preview",
        "Preview a change",
        "PREVIEW BODY",
    );
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    type_text(&mut app, &fixture.client, "/pre").await;
    drive(&mut app, &fixture.client, press(KeyCode::Down)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert_eq!(app.ask, "/preview ");
}

#[tokio::test]
async fn esc_closes_the_skill_list_and_keeps_the_typed_text() {
    let fixture = Fixture::new("slash-esc").await;
    plant_skill(
        &fixture.workspace,
        "preflight",
        "Ship checks",
        "PREFLIGHT BODY UNIQUE",
    );
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    type_text(&mut app, &fixture.client, "/pre").await;
    assert!(screen_model(&app).skill_picker.is_some());
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    assert_eq!(app.ask, "/pre");
    assert!(screen_model(&app).skill_picker.is_none());
    let pane = draw(&app);
    assert!(!pane.contains("Ship checks"), "{pane}");
}

#[tokio::test]
async fn submitting_a_slash_skill_posts_the_text_after_the_name() {
    let fixture = Fixture::new("slash-submit").await;
    plant_skill(
        &fixture.workspace,
        "preflight",
        "Ship checks",
        "PREFLIGHT BODY UNIQUE",
    );
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    type_text(&mut app, &fixture.client, "/preflight ship this").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| kinds(app).contains(&"ask")).await;
    match app.cards.first() {
        Some(Card::Ask { text, .. }) => assert_eq!(text, "ship this"),
        other => panic!("expected an ask card, got {other:?}"),
    }
}

fn catalog_two_models() -> String {
    serde_json::json!({
        "data": [
            { "id": "test/model", "context_length": 128000 },
            { "id": "grok-4.6", "context_length": 256000, "reasoning_efforts": ["low", "medium", "high", "xhigh"] }
        ]
    })
    .to_string()
}

async fn idle_session(fixture: &Fixture) -> App {
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    app
}

fn palette_rows(app: &App) -> (Vec<String>, usize) {
    match screen_model(app).overlay {
        Some(Overlay::Palette {
            rows, highlight, ..
        }) => (rows.into_iter().map(|row| row.name).collect(), highlight),
        other => panic!("expected a command palette, got {other:?}"),
    }
}

#[tokio::test]
async fn ctrl_k_filters_to_model_and_enter_opens_it() {
    let fixture = Fixture::new("palette-model").await;
    plant_skill(
        &fixture.workspace,
        "preflight",
        "Ship checks",
        "PREFLIGHT BODY",
    );
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('k')).await;
    let (rows, _) = palette_rows(&app);
    assert!(rows.iter().any(|name| name == "New session"), "{rows:?}");
    assert!(rows.iter().any(|name| name == "/compact"), "{rows:?}");
    assert!(rows.iter().all(|name| name != "preflight"), "{rows:?}");
    assert!(app.ask.is_empty());
    type_text(&mut app, &fixture.client, "mod").await;
    let (rows, highlight) = palette_rows(&app);
    assert_eq!(rows, vec!["Open model".to_string(), "/model".to_string()]);
    assert_eq!(highlight, 0);
    assert!(app.ask.is_empty());
    assert_eq!(screen_model(&app).bottom, "mod");
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(
        matches!(screen_model(&app).overlay, Some(Overlay::Model { .. })),
        "enter on /model opens the model list: {:?}",
        screen_model(&app).overlay
    );
}

#[tokio::test]
async fn question_mark_opens_help_only_on_an_empty_prompt() {
    let fixture = Fixture::new("palette-help").await;
    plant_skill(
        &fixture.workspace,
        "preflight",
        "Ship checks",
        "PREFLIGHT BODY",
    );
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('?'))).await;
    match screen_model(&app).overlay {
        Some(Overlay::Help { rows, .. }) => {
            assert!(rows
                .iter()
                .any(|row| row.name == "New session" && row.keys == "Ctrl-T"));
            assert!(rows
                .iter()
                .any(|row| row.name == "Sessions" && row.keys == "Ctrl-B"));
            assert!(rows
                .iter()
                .any(|row| row.name == "Panes" && row.keys == "Ctrl-G"));
            assert!(rows.iter().any(|row| row.name == "/compact"));
            assert!(rows.iter().any(|row| row.name == "/todos"));
            assert!(rows.iter().any(|row| row.name == "/tasks"));
            assert!(rows.iter().any(|row| row.name == "/schedules"));
            assert!(rows.iter().any(|row| row.name == "/closeout"));
            assert!(rows.iter().all(|row| row.name != "preflight"));
        }
        other => panic!("expected help, got {other:?}"),
    }
    assert!(app.ask.is_empty());
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    assert!(screen_model(&app).overlay.is_none());
    type_text(&mut app, &fixture.client, "hi").await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('?'))).await;
    assert_eq!(app.ask, "hi?");
    assert!(screen_model(&app).overlay.is_none());
}

#[tokio::test]
async fn typing_pre_in_the_palette_leaves_the_skill_off_the_list() {
    let fixture = Fixture::new("palette-skill").await;
    plant_skill(
        &fixture.workspace,
        "preflight",
        "Ship checks",
        "PREFLIGHT BODY",
    );
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('k')).await;
    type_text(&mut app, &fixture.client, "pre").await;
    match screen_model(&app).overlay {
        Some(Overlay::Palette { rows, query, .. }) => {
            assert!(rows.is_empty(), "{rows:?}");
            assert_eq!(query, "pre");
        }
        other => panic!("the palette stays open, got {other:?}"),
    }
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(app.ask.is_empty());
    assert!(matches!(
        screen_model(&app).overlay,
        Some(Overlay::Palette { .. })
    ));
}

#[tokio::test]
async fn slash_and_palette_toggle_panes_on_their_own() {
    let fixture = Fixture::new("pane-toggles").await;
    let mut app = idle_session(&fixture).await;
    type_text(&mut app, &fixture.client, "/todos").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(app.ask.is_empty());
    assert!(app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Todos));
    assert!(draw(&app).contains(" todos "));
    type_text(&mut app, &fixture.client, "/tasks").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Todos));
    assert!(app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Tasks));
    assert!(draw(&app).contains(" tasks "));
    type_text(&mut app, &fixture.client, "/schedules").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    type_text(&mut app, &fixture.client, "/closeout").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Schedules));
    assert!(app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Closeout));
    assert!(app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Todos));
    type_text(&mut app, &fixture.client, "/todos").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(!app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Todos));
    assert!(app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Tasks));
    drive(&mut app, &fixture.client, ctrl('k')).await;
    type_text(&mut app, &fixture.client, "/todos").await;
    let (rows, highlight) = palette_rows(&app);
    assert_eq!(rows, vec!["/todos".to_string()]);
    assert_eq!(highlight, 0);
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(screen_model(&app).overlay.is_none());
    assert!(app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Todos));
    assert!(app.ask.is_empty());
    drive(&mut app, &fixture.client, ctrl('k')).await;
    type_text(&mut app, &fixture.client, "Todos").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(!app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Todos));
}

#[tokio::test]
async fn slash_todos_extra_is_an_ask() {
    let fixture = Fixture::with_replies("slash-todos-extra", finish_only("done")).await;
    let mut app = idle_session(&fixture).await;
    type_text(&mut app, &fixture.client, "/todos extra").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    assert!(kinds(&app).contains(&"ask"));
    assert!(!app
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Todos));
}

#[tokio::test]
async fn ctrl_m_picks_a_model_for_the_next_complete() {
    let fixture = Fixture::with_replies("pick-model", finish_only("done")).await;
    fixture._fake.set_models(&catalog_two_models());
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('m')).await;
    match screen_model(&app).overlay {
        Some(Overlay::Model { rows, highlight }) => {
            assert!(rows.iter().any(|row| row == "test/model"), "{rows:?}");
            assert!(rows.iter().any(|row| row == "grok-4.6"), "{rows:?}");
            assert_eq!(rows[highlight], "test/model");
        }
        other => panic!("expected a model overlay, got {other:?}"),
    }
    drive(&mut app, &fixture.client, press(KeyCode::Down)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    match screen_model(&app).overlay {
        Some(Overlay::Effort { rows, .. }) => {
            assert_eq!(rows, vec!["low", "medium", "high", "xhigh"]);
        }
        other => panic!("expected an effort overlay, got {other:?}"),
    }
    drive(&mut app, &fixture.client, press(KeyCode::Down)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Down)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(screen_model(&app).overlay.is_none());
    assert_eq!(app.model, "grok-4.6");
    assert_eq!(app.effort.as_deref(), Some("high"));
    let config = Config::load(&fixture.home.join("config.toml")).expect("config loads");
    assert_eq!(config.model, "grok-4.6");
    assert_eq!(config.effort.as_deref(), Some("high"));
    let meta = kyotoagent::session::Session::at(
        &fixture
            .home
            .join(kyotoagent::server::SESSIONS_DIR)
            .join(&app.selected),
    )
    .meta()
    .expect("meta loads");
    assert_eq!(meta.model, "grok-4.6");
    assert_eq!(meta.effort.as_deref(), Some("high"));

    type_text(&mut app, &fixture.client, "Hello.").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    let posts = fixture._fake.posts();
    let last = posts.last().expect("a completion was posted");
    assert_eq!(last["model"], Value::from("grok-4.6"));
    assert_eq!(last["reasoning_effort"], Value::from("high"));
}

#[tokio::test]
async fn a_local_model_pick_omits_reasoning_effort() {
    let fixture = Fixture::with_replies("pick-local", finish_only("done")).await;
    fixture._fake.set_models(
        &serde_json::json!({
            "data": [
                { "id": "test/model" },
                { "id": "local-model" }
            ]
        })
        .to_string(),
    );
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('m')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Down)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(screen_model(&app).overlay.is_none());
    assert_eq!(app.model, "local-model");
    assert_eq!(app.effort, None);
    type_text(&mut app, &fixture.client, "Hello.").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    let last = fixture
        ._fake
        .posts()
        .pop()
        .expect("a completion was posted");
    assert_eq!(last["model"], Value::from("local-model"));
    assert!(
        last.get("reasoning_effort").is_none(),
        "local omits the key: {last}"
    );
}

#[tokio::test]
async fn esc_leaves_the_stored_model_and_effort() {
    let fixture = Fixture::with_replies("pick-esc", finish_only("done")).await;
    fs::write(
        fixture.home.join("config.toml"),
        format!(
            "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\neffort = \"medium\"\n",
            fixture._fake.base_url()
        ),
    )
    .expect("the config writes");
    fixture._fake.set_models(&catalog_two_models());
    let mut app = idle_session(&fixture).await;
    poll(&mut app, &fixture.client).await.expect("polls");
    assert_eq!(app.model, "test/model");
    drive(&mut app, &fixture.client, ctrl('m')).await;
    assert!(matches!(
        screen_model(&app).overlay,
        Some(Overlay::Model { .. })
    ));
    drive(&mut app, &fixture.client, press(KeyCode::Down)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    assert!(screen_model(&app).overlay.is_none());
    let config = Config::load(&fixture.home.join("config.toml")).expect("config loads");
    assert_eq!(config.model, "test/model");
    assert_eq!(config.effort.as_deref(), Some("medium"));
}

#[tokio::test]
async fn slash_model_and_effort_write_the_pair() {
    let fixture = Fixture::with_replies("slash-model", finish_only("done")).await;
    let mut app = idle_session(&fixture).await;
    type_text(&mut app, &fixture.client, "/model grok-4.6").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert_eq!(app.model, "grok-4.6");
    type_text(&mut app, &fixture.client, "/effort high").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert_eq!(app.effort.as_deref(), Some("high"));
    let config = Config::load(&fixture.home.join("config.toml")).expect("config loads");
    assert_eq!(config.model, "grok-4.6");
    assert_eq!(config.effort.as_deref(), Some("high"));
}

fn catalog_filter_models() -> String {
    serde_json::json!({
        "data": [
            { "id": "test/model", "context_length": 128000 },
            { "id": "grok-4.6", "context_length": 256000, "aliases": ["grok"], "reasoning_efforts": ["low", "medium", "high", "xhigh"] },
            { "id": "other", "aliases": ["green"] }
        ]
    })
    .to_string()
}

fn overlay_model_rows(app: &App) -> (Vec<String>, usize) {
    match screen_model(app).overlay {
        Some(Overlay::Model { rows, highlight }) => (rows, highlight),
        other => panic!("expected a model overlay, got {other:?}"),
    }
}

#[tokio::test]
async fn typing_prefix_filters_the_model_list() {
    let fixture = Fixture::new("filter-model").await;
    fixture._fake.set_models(&catalog_filter_models());
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('m')).await;
    type_text(&mut app, &fixture.client, "gR").await;
    let (rows, highlight) = overlay_model_rows(&app);
    assert_eq!(rows, vec!["grok-4.6", "other"]);
    assert_eq!(highlight, 0);
    assert_eq!(rows[highlight], "grok-4.6");
    assert!(app.ask.is_empty());
    assert_eq!(screen_model(&app).bottom, "gR");
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    assert!(screen_model(&app).overlay.is_none());
    assert_eq!(app.model, "test/model");
    assert!(app.ask.is_empty());
}

#[tokio::test]
async fn typing_a_version_and_a_subsequence_lists_grok_4_6() {
    let fixture = Fixture::new("filter-fuzzy").await;
    fixture._fake.set_models(&catalog_filter_models());
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('m')).await;
    type_text(&mut app, &fixture.client, "4.6").await;
    let (rows, highlight) = overlay_model_rows(&app);
    assert_eq!(rows, vec!["grok-4.6"]);
    assert_eq!(rows[highlight], "grok-4.6");
    assert_eq!(screen_model(&app).bottom, "4.6");
    assert!(app.ask.is_empty());
    for _ in 0..3 {
        drive(&mut app, &fixture.client, press(KeyCode::Backspace)).await;
    }
    type_text(&mut app, &fixture.client, "g46").await;
    let (rows, highlight) = overlay_model_rows(&app);
    assert_eq!(rows, vec!["grok-4.6"]);
    assert_eq!(rows[highlight], "grok-4.6");
    assert_eq!(screen_model(&app).bottom, "g46");
    assert!(app.ask.is_empty());
    assert_eq!(app.model, "test/model");
}

#[tokio::test]
async fn backspace_restores_the_full_model_catalog() {
    let fixture = Fixture::new("filter-backspace").await;
    fixture._fake.set_models(&catalog_filter_models());
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('m')).await;
    type_text(&mut app, &fixture.client, "gr").await;
    drive(&mut app, &fixture.client, press(KeyCode::Backspace)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Backspace)).await;
    let (rows, _) = overlay_model_rows(&app);
    assert_eq!(rows, vec!["test/model", "grok-4.6", "other"]);
    assert!(app.ask.is_empty());
    assert!(screen_model(&app).bottom.is_empty());
}

#[tokio::test]
async fn enter_on_an_empty_model_filter_keeps_the_overlay() {
    let fixture = Fixture::new("filter-empty").await;
    fixture._fake.set_models(&catalog_filter_models());
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('m')).await;
    type_text(&mut app, &fixture.client, "zzz").await;
    let (rows, _) = overlay_model_rows(&app);
    assert!(rows.is_empty(), "{rows:?}");
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    let (rows, _) = overlay_model_rows(&app);
    assert!(rows.is_empty(), "{rows:?}");
    assert!(app.ask.is_empty());
    assert_eq!(app.model, "test/model");
}

#[tokio::test]
async fn enter_on_a_filtered_model_row_writes_that_model() {
    let fixture = Fixture::with_replies("filter-apply", finish_only("done")).await;
    fixture._fake.set_models(&catalog_two_models());
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('m')).await;
    type_text(&mut app, &fixture.client, "gr").await;
    let (rows, highlight) = overlay_model_rows(&app);
    assert_eq!(rows, vec!["grok-4.6"]);
    assert_eq!(rows[highlight], "grok-4.6");
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    match screen_model(&app).overlay {
        Some(Overlay::Effort { rows, .. }) => {
            assert_eq!(rows, vec!["low", "medium", "high", "xhigh"]);
        }
        other => panic!("expected an effort overlay, got {other:?}"),
    }
    assert_eq!(app.model, "grok-4.6");
}

#[tokio::test]
async fn typing_on_the_effort_overlay_leaves_the_rows_and_ask() {
    let fixture = Fixture::new("filter-effort").await;
    fixture._fake.set_models(&catalog_two_models());
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('m')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Down)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    match screen_model(&app).overlay {
        Some(Overlay::Effort { rows, .. }) => {
            assert_eq!(rows, vec!["low", "medium", "high", "xhigh"]);
        }
        other => panic!("expected an effort overlay, got {other:?}"),
    }
    type_text(&mut app, &fixture.client, "zz").await;
    match screen_model(&app).overlay {
        Some(Overlay::Effort { rows, .. }) => {
            assert_eq!(rows, vec!["low", "medium", "high", "xhigh"]);
        }
        other => panic!("expected an effort overlay, got {other:?}"),
    }
    assert!(app.ask.is_empty());
}

fn tall_session() -> App {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions.push(SessionRow {
        id: "91bc7a1d".into(),
        workspace: PathBuf::from("/w"),
        title: None,
        status: Status::Idle,
        waiting: None,
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
    });
    app.cards = (0..20)
        .map(|index| Card::ask(&format!("card {index}")))
        .collect();
    app
}

fn pane_text(app: &App) -> String {
    let area = app.area;
    let backend = TestBackend::new(area.width, area.height);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    let model = screen_model(app);
    terminal
        .draw(|frame| kyotoagent::screen::render(&model, frame.area(), frame))
        .expect("the screen draws");
    let mut out = String::new();
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn pane_line(app: &App, offset: u16) -> String {
    let area = app.area;
    let backend = TestBackend::new(area.width, area.height);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    let model = screen_model(app);
    terminal
        .draw(|frame| kyotoagent::screen::render(&model, frame.area(), frame))
        .expect("the screen draws");
    let inner = kyotoagent::screen::session_inner_of(&model, area);
    let y = inner.y.saturating_add(offset);
    let buffer = terminal.backend().buffer();
    let mut line = String::new();
    for x in 0..inner.width {
        line.push_str(buffer[(inner.x.saturating_add(x), y)].symbol());
    }
    line
}

#[test]
fn follow_keeps_the_last_card_when_the_list_is_taller_than_the_pane() {
    let app = tall_session();
    assert!(app.follow);
    let text = pane_text(&app);
    assert!(text.contains("card 19"), "{text}");
    assert!(!text.contains("card 0"), "{text}");
}

#[tokio::test]
async fn scroll_up_leaves_follow_and_scroll_down_at_the_end_resumes_it() {
    let mut app = tall_session();
    let client = Client::at(PathBuf::from("/nope"));
    apply(&mut app, &client, Effect::ScrollUp)
        .await
        .expect("scroll up");
    assert!(!app.follow);
    apply(&mut app, &client, Effect::ScrollDown)
        .await
        .expect("scroll down");
    assert!(app.follow);
    let text = pane_text(&app);
    assert!(text.contains("card 19"), "{text}");
}

fn wheel_at(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn session_wheel_point(app: &App) -> (u16, u16) {
    let inner = kyotoagent::screen::session_inner_of(&screen_model(app), app.area);
    (inner.x, inner.y)
}

fn three_line_text(app: &App) -> String {
    let inner = kyotoagent::screen::session_inner_of(&screen_model(app), app.area);
    let text_width = usize::from(inner.width).saturating_sub(2).max(1);
    format!(
        "{}{}{}",
        "a".repeat(text_width),
        "b".repeat(text_width),
        "c".repeat(text_width)
    )
}

fn three_line_result(app: &App) -> Card {
    Card::result(&three_line_text(app))
}

#[tokio::test]
async fn the_wheel_moves_one_wrapped_line_and_follow_returns_at_the_tail() {
    let mut app = tall_session();
    app.area = Rect::new(0, 0, 76, 6);
    app.cards = vec![three_line_result(&app)];
    let client = Client::at(PathBuf::from("/nope"));
    let area = app.area;
    let inner = kyotoagent::screen::session_inner_of(&screen_model(&app), area);
    let text_width = usize::from(inner.width).saturating_sub(2);
    assert_eq!(
        kyotoagent::screen::wrap(&three_line_text(&app), text_width).len(),
        3
    );
    assert!(app.follow);
    let first = pane_line(&app, 0);
    let (column, row) = session_wheel_point(&app);
    let up = mouse(
        wheel_at(MouseEventKind::ScrollUp, column, row),
        &screen_model(&app),
        area,
    );
    assert_eq!(up, Some(Effect::ScrollUp));
    apply(&mut app, &client, up.expect("wheel up"))
        .await
        .expect("scroll up");
    assert!(!app.follow);
    assert_ne!(pane_line(&app, 0), first);
    assert_eq!(pane_line(&app, 1), first);

    let (column, row) = session_wheel_point(&app);
    let down = mouse(
        wheel_at(MouseEventKind::ScrollDown, column, row),
        &screen_model(&app),
        area,
    );
    assert_eq!(down, Some(Effect::ScrollDown));
    apply(&mut app, &client, down.expect("wheel down"))
        .await
        .expect("scroll down");
    assert!(app.follow);
    assert_eq!(pane_line(&app, 0), first);

    let scroll = app.scroll;
    for index in 0..8 {
        let mut extra = app.sessions[0].clone();
        extra.id = format!("extra-{index}");
        extra.title = Some(format!("extra {index}"));
        app.sessions.push(extra);
    }
    let list = list_inner(&screen_model(&app), area);
    let over_list = mouse(
        wheel_at(MouseEventKind::ScrollDown, list.x + 1, list.y),
        &screen_model(&app),
        area,
    );
    assert_eq!(over_list, Some(Effect::ScrollList { up: false }));
    apply(&mut app, &client, over_list.expect("list wheel"))
        .await
        .expect("list scroll");
    assert_eq!(app.scroll, scroll);
    assert!(app.follow);
    assert!(app.list_scroll > 0);
}

#[tokio::test]
async fn page_down_from_the_top_moves_by_the_inner_height() {
    let mut app = tall_session();
    app.area = Rect::new(0, 0, 76, 14);
    let inner = kyotoagent::screen::session_inner_of(&screen_model(&app), app.area);
    assert_eq!(inner.height, 8);
    let text_width = usize::from(inner.width).saturating_sub(2);
    app.cards = vec![Card::result(&"y".repeat(text_width.saturating_mul(40)))];
    app.follow = false;
    app.scroll = 0;
    let client = Client::at(PathBuf::from("/nope"));
    apply(&mut app, &client, Effect::PageDown)
        .await
        .expect("page down");
    assert_eq!(app.scroll, usize::from(inner.height));
    assert!(!app.follow);
}

#[tokio::test]
async fn selecting_another_session_turns_follow_on() {
    let mut app = tall_session();
    let client = Client::at(PathBuf::from("/nope"));
    apply(&mut app, &client, Effect::ScrollUp)
        .await
        .expect("scroll up");
    assert!(!app.follow);
    app.sessions.push(SessionRow {
        id: "3f2ae04c".into(),
        workspace: PathBuf::from("/w2"),
        title: None,
        status: Status::Idle,
        waiting: None,
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
    });
    apply(&mut app, &client, Effect::SelectNext)
        .await
        .expect("selection does not need the server");
    assert!(app.follow);
    assert_eq!(app.selected, "3f2ae04c");
    assert!(app.cards.is_empty());
    assert_eq!(app.scroll, 0);
}

#[tokio::test]
async fn clicking_a_session_selects_it_and_turns_follow_on() {
    let mut app = tall_session();
    let client = Client::at(PathBuf::from("/nope"));
    apply(&mut app, &client, Effect::ScrollUp)
        .await
        .expect("scroll up");
    assert!(!app.follow);
    app.sessions.push(SessionRow {
        id: "3f2ae04c".into(),
        workspace: PathBuf::from("/w2"),
        title: None,
        status: Status::Idle,
        waiting: None,
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
    });
    apply(&mut app, &client, Effect::SelectSession("3f2ae04c".into()))
        .await
        .expect("selection does not need the server");
    assert!(app.follow);
    assert_eq!(app.selected, "3f2ae04c");
    assert!(app.cards.is_empty());
    assert_eq!(app.scroll, 0);
    apply(&mut app, &client, Effect::SelectSession("3f2ae04c".into()))
        .await
        .expect("selecting the current session does not need the server");
    assert!(app.follow);
    assert_eq!(app.selected, "3f2ae04c");
}

fn git_init(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "kyotoagent@test"]);
    run(&["config", "user.name", "kyotoagent"]);
    run(&["commit", "--allow-empty", "-m", "init"]);
}

fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test]
async fn ctrl_t_opens_the_overlay_and_posts_nothing() {
    let fixture = Fixture::new("ctrl-t-overlay").await;
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    drive(&mut app, &fixture.client, ctrl('t')).await;
    assert!(app.sessions.is_empty());
    match screen_model(&app).overlay {
        Some(Overlay::Question { text, choices, .. }) => {
            assert_eq!(text, kyotoagent::tui::WORKSPACE_QUESTION);
            assert_eq!(choices[0].label, kyotoagent::tui::WORKSPACE_HERE);
            assert_eq!(choices[1].label, kyotoagent::tui::WORKSPACE_WORKTREE);
        }
        other => panic!("expected a workspace question, got {other:?}"),
    }
    let (status, body) = fixture
        .client
        .request("GET", "/v1/sessions", None)
        .await
        .expect("the list loads");
    assert_eq!(status, 200, "{body}");
    let rows: Vec<Value> = serde_json::from_str(&body).expect("the list is JSON");
    assert!(rows.is_empty(), "{rows:?}");
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    poll(&mut app, &fixture.client)
        .await
        .expect("the list still polls");
    assert!(app.sessions.is_empty());
    assert!(screen_model(&app).overlay.is_none());
}

fn write_profiles(fixture: &Fixture) {
    let path = fixture.home.join("config.toml");
    let mut text = fs::read_to_string(&path).expect("the config reads");
    text.push_str(
        "\n[profiles.review]\ntools = [\"read_file\", \"grep\", \"list_dir\", \"web_fetch\", \"ask\", \"finish\"]\n\n[profiles.planning]\ntools = [\"read_file\", \"ask\", \"finish\"]\n",
    );
    fs::write(&path, text).expect("the config writes");
}

fn session_meta(fixture: &Fixture, id: &str) -> kyotoagent::session::SessionMeta {
    kyotoagent::session::Session::at(&fixture.home.join(kyotoagent::server::SESSIONS_DIR).join(id))
        .meta()
        .expect("meta loads")
}

fn profile_choices(app: &App) -> (String, Vec<String>) {
    match screen_model(app).overlay {
        Some(Overlay::Question { text, choices, .. }) => (
            text,
            choices.into_iter().map(|choice| choice.label).collect(),
        ),
        other => panic!("expected the profile question, got {other:?}"),
    }
}

#[tokio::test]
async fn everything_on_a_new_session_stores_no_profile() {
    let fixture = Fixture::new("profile-everything").await;
    write_profiles(&fixture);
    let path = fixture.home.join("config.toml");
    let text = fs::read_to_string(&path).expect("the config reads");
    fs::write(&path, format!("profile = \"review\"\n{text}")).expect("the config writes");
    let before = fs::read(fixture.home.join("config.toml")).expect("config");
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    drive(&mut app, &fixture.client, ctrl('t')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    let (text, choices) = profile_choices(&app);
    assert_eq!(text, kyotoagent::tui::PROFILE_QUESTION);
    assert_eq!(
        choices,
        vec![
            kyotoagent::tui::PROFILE_EVERYTHING.to_string(),
            "review".to_string(),
            "planning".to_string()
        ]
    );
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    assert_eq!(app.sessions.len(), 1);
    assert!(screen_model(&app).overlay.is_none());
    let meta = session_meta(&fixture, &app.sessions[0].id);
    assert_eq!(meta.profile, None);
    let painted = draw(&app);
    assert!(painted.contains(" Kyoto Agent "), "{painted}");
    assert!(!painted.contains("review"), "{painted}");
    assert_eq!(
        fs::read(fixture.home.join("config.toml")).expect("config"),
        before
    );
}

#[tokio::test]
async fn review_on_a_new_session_stores_review() {
    let fixture = Fixture::new("profile-review").await;
    write_profiles(&fixture);
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    drive(&mut app, &fixture.client, ctrl('t')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('2'))).await;
    assert_eq!(app.sessions.len(), 1);
    let meta = session_meta(&fixture, &app.sessions[0].id);
    assert_eq!(meta.profile.as_deref(), Some("review"));
    let painted = draw(&app);
    assert!(painted.contains(" Kyoto Agent "), "{painted}");
    assert!(painted.contains("review"), "{painted}");
}

#[tokio::test]
async fn the_palette_profile_row_changes_the_live_session() {
    let fixture = Fixture::new("profile-palette").await;
    write_profiles(&fixture);
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    drive(&mut app, &fixture.client, ctrl('t')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    assert_eq!(app.sessions.len(), 1);
    let before = fs::read(fixture.home.join("config.toml")).expect("config");
    drive(&mut app, &fixture.client, ctrl('k')).await;
    type_text(&mut app, &fixture.client, "Profile").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    let (text, choices) = profile_choices(&app);
    assert_eq!(text, kyotoagent::tui::PROFILE_QUESTION);
    assert_eq!(choices[0], kyotoagent::tui::PROFILE_EVERYTHING);
    drive(&mut app, &fixture.client, press(KeyCode::Char('2'))).await;
    let meta = session_meta(&fixture, &app.selected);
    assert_eq!(meta.profile.as_deref(), Some("review"));
    assert!(draw(&app).contains("review \u{00b7}"), "{}", draw(&app));
    assert_eq!(
        fs::read(fixture.home.join("config.toml")).expect("config"),
        before
    );
    drive(&mut app, &fixture.client, ctrl('k')).await;
    type_text(&mut app, &fixture.client, "Profile").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    let meta = session_meta(&fixture, &app.selected);
    assert_eq!(meta.profile, None);
    assert!(!draw(&app).contains("review"), "{}", draw(&app));
    assert_eq!(
        fs::read(fixture.home.join("config.toml")).expect("config"),
        before
    );
}

#[tokio::test]
async fn choosing_this_directory_creates_a_session_for_the_workspace() {
    let fixture = Fixture::new("ctrl-t-here").await;
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    assert_eq!(app.sessions.len(), 1);
    assert_eq!(app.sessions[0].workspace, fixture.workspace);
    assert!(screen_model(&app).overlay.is_none());
}

#[tokio::test]
async fn choosing_a_worktree_in_a_plain_directory_shows_the_error() {
    let fixture = Fixture::new("ctrl-t-plain").await;
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    drive(&mut app, &fixture.client, ctrl('t')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('2'))).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    assert!(app.sessions.is_empty());
    let notice = app.notice.as_deref().unwrap_or("");
    assert!(
        notice.contains("the workspace is not a git repository"),
        "{notice}"
    );
}

#[tokio::test]
async fn choosing_a_worktree_in_a_git_repo_creates_the_linked_worktree() {
    let fixture = Fixture::new("ctrl-t-git").await;
    git_init(&fixture.workspace);
    let marker = fixture.workspace.join("kept.txt");
    fs::write(&marker, "stay").expect("the original file writes");
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    drive(&mut app, &fixture.client, ctrl('t')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('2'))).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    assert_eq!(app.sessions.len(), 1);
    let id = app.sessions[0].id.clone();
    let dest = fixture.root.join("worktrees").join(format!("work-{id}"));
    assert_eq!(app.sessions[0].workspace, dest);
    assert!(dest.is_dir());
    assert_eq!(
        git_out(&dest, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        format!("kyotoagent/{id}")
    );
    assert_eq!(
        git_out(&fixture.workspace, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main"
    );
    assert_eq!(
        fs::read_to_string(&marker).expect("the original file stays"),
        "stay"
    );
}

#[tokio::test]
async fn picking_a_project_then_a_worktree_uses_that_folder() {
    let fixture = Fixture::new("ctrl-t-project").await;
    let kyotoagent = fixture.root.join("kyotoagent");
    fs::create_dir_all(&kyotoagent).expect("the project exists");
    git_init(&kyotoagent);
    fs::write(kyotoagent.join("kept.txt"), "from-kyotoagent").expect("the marker writes");
    git_out(&kyotoagent, &["add", "kept.txt"]);
    git_out(&kyotoagent, &["commit", "-m", "keep"]);
    let kyotoagent = fs::canonicalize(&kyotoagent).unwrap_or(kyotoagent);
    let acpbot = fixture.root.join("acpbot");
    fs::create_dir_all(&acpbot).expect("the other project exists");
    let acpbot = fs::canonicalize(&acpbot).unwrap_or(acpbot);
    let config_path = fixture.home.join("config.toml");
    let mut text = fs::read_to_string(&config_path).expect("the config reads");
    text.push_str(&format!(
        "\n[projects.kyotoagent]\npath = \"{}\"\n\n[projects.acpbot]\npath = \"{}\"\n",
        kyotoagent.display(),
        acpbot.display()
    ));
    fs::write(&config_path, text).expect("the config writes");
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    drive(&mut app, &fixture.client, ctrl('t')).await;
    match screen_model(&app).overlay {
        Some(Overlay::Question { text, choices, .. }) => {
            assert_eq!(text, kyotoagent::tui::WORKSPACE_QUESTION);
            assert_eq!(choices[0].label, kyotoagent::tui::WORKSPACE_HERE);
            assert_eq!(choices[1].label, "kyotoagent");
            assert_eq!(choices[2].label, "acpbot");
        }
        other => panic!("expected the project list, got {other:?}"),
    }
    drive(&mut app, &fixture.client, press(KeyCode::Char('2'))).await;
    match screen_model(&app).overlay {
        Some(Overlay::Question { choices, .. }) => {
            assert_eq!(choices[0].label, kyotoagent::tui::WORKSPACE_HERE);
            assert_eq!(choices[1].label, kyotoagent::tui::WORKSPACE_WORKTREE);
        }
        other => panic!("expected the worktree question, got {other:?}"),
    }
    drive(&mut app, &fixture.client, press(KeyCode::Char('2'))).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('1'))).await;
    assert_eq!(app.sessions.len(), 1);
    let id = app.sessions[0].id.clone();
    let dest = fixture
        .root
        .join("worktrees")
        .join(format!("kyotoagent-{id}"));
    assert_eq!(app.sessions[0].workspace, dest);
    assert_ne!(app.sessions[0].workspace, fixture.workspace);
    assert_eq!(
        git_out(&dest, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        format!("kyotoagent/{id}")
    );
    assert_eq!(
        fs::read_to_string(dest.join("kept.txt")).expect("the worktree has the project file"),
        "from-kyotoagent"
    );
}

#[tokio::test]
async fn yolo_does_not_pick_the_workspace_question() {
    let fixture = Fixture::new("ctrl-t-yolo").await;
    let mut app = fixture.app();
    app.yolo = true;
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    drive(&mut app, &fixture.client, ctrl('t')).await;
    poll(&mut app, &fixture.client)
        .await
        .expect("the overlay still polls");
    assert!(app.sessions.is_empty());
    match screen_model(&app).overlay {
        Some(Overlay::Question { text, .. }) => {
            assert_eq!(text, kyotoagent::tui::WORKSPACE_QUESTION);
        }
        other => panic!("expected a workspace question, got {other:?}"),
    }
}

fn docs_rs_serde() -> String {
    let mut url = String::from("https:");
    url.push('/');
    url.push('/');
    url.push_str("docs.rs/serde");
    url
}

fn hit(
    model: &kyotoagent::screen::ScreenModel,
    area: Rect,
    finder: fn(&kyotoagent::screen::ScreenModel, Rect, u16, u16) -> Option<String>,
    want: &str,
) -> (u16, u16) {
    for y in 0..area.height {
        for x in 0..area.width {
            if finder(model, area, x, y).as_deref() == Some(want) {
                return (x, y);
            }
        }
    }
    panic!("no hit for {want}");
}

fn click_at(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn todo_with_readme() -> TodoItem {
    TodoItem {
        id: "one".into(),
        title: "Read me".into(),
        status: TodoStatus::InProgress,
        description: None,
        files: vec!["README.md".into()],
        links: vec![docs_rs_serde()],
    }
}

#[tokio::test]
async fn a_click_on_a_todo_file_opens_the_file_overlay() {
    let fixture = Fixture::new("tui-file").await;
    fs::write(fixture.workspace.join("README.md"), "hello from readme").expect("readme");
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    app.todos = vec![todo_with_readme()];
    app.right_open = true;
    apply(&mut app, &fixture.client, Effect::OpenTodo("one".into()))
        .await
        .expect("the todo opens");
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let (x, y) = hit(&model, area, kyotoagent::screen::file_at, "README.md");
    let effect = mouse(click_at(x, y), &model, area).expect("file click");
    assert_eq!(effect, Effect::OpenFile("README.md".into()));
    apply(&mut app, &fixture.client, effect)
        .await
        .expect("the file opens");
    match screen_model(&app).overlay {
        Some(Overlay::File {
            path,
            text,
            truncated,
        }) => {
            assert_eq!(path, "README.md");
            assert_eq!(text, "hello from readme");
            assert!(!truncated);
        }
        other => panic!("expected a file overlay, got {other:?}"),
    }
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    let back = screen_model(&app);
    assert!(back.overlay.is_none());
    assert_eq!(back.open_todo.as_deref(), Some("one"));
    let (x, y) = hit(&back, area, kyotoagent::screen::file_at, "README.md");
    assert_eq!(
        kyotoagent::screen::file_at(&back, area, x, y).as_deref(),
        Some("README.md")
    );
}

#[tokio::test]
async fn an_outside_file_stays_on_the_expanded_todo() {
    let fixture = Fixture::new("tui-file-out").await;
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    app.todos = vec![todo_with_readme()];
    app.right_open = true;
    apply(&mut app, &fixture.client, Effect::OpenTodo("one".into()))
        .await
        .expect("the todo opens");
    apply(
        &mut app,
        &fixture.client,
        Effect::OpenFile("../secret".into()),
    )
    .await
    .expect("the refuse applies");
    assert!(app.notice.is_some(), "{:?}", app.notice);
    let stayed = screen_model(&app);
    assert!(stayed.overlay.is_none());
    assert_eq!(stayed.open_todo.as_deref(), Some("one"));
}

#[tokio::test]
async fn a_click_on_a_todo_link_spawns_the_browser() {
    let fixture = Fixture::new("tui-link").await;
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    app.todos = vec![todo_with_readme()];
    app.right_open = true;
    apply(&mut app, &fixture.client, Effect::OpenTodo("one".into()))
        .await
        .expect("the todo opens");
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let url = docs_rs_serde();
    let (x, y) = hit(&model, area, kyotoagent::screen::link_at, &url);
    let _cap = capture_open_url();
    let effect = mouse(click_at(x, y), &model, area).expect("link click");
    assert_eq!(effect, Effect::OpenLink(url.clone()));
    apply(&mut app, &fixture.client, effect)
        .await
        .expect("the link opens");
    assert_eq!(last_opened_url().as_deref(), Some(url.as_str()));
    let stayed = screen_model(&app);
    assert!(stayed.overlay.is_none());
    assert_eq!(stayed.open_todo.as_deref(), Some("one"));
}

#[tokio::test]
async fn a_click_on_a_pull_overlay_url_spawns_the_browser() {
    let fixture = Fixture::new("tui-pull-link").await;
    let mut app = fixture.app();
    poll(&mut app, &fixture.client)
        .await
        .expect("an empty list polls");
    new_here(&mut app, &fixture.client).await;
    let url = sample_pull();
    app.sessions[0].pull_url = Some(url.clone());
    apply(&mut app, &fixture.client, Effect::OpenPull)
        .await
        .expect("the pull opens");
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let (x, y) = hit(&model, area, kyotoagent::screen::link_at, &url);
    let _cap = capture_open_url();
    let effect = released_click(&model, area, x, y).expect("url click");
    assert_eq!(effect, Effect::OpenLink(url.clone()));
    apply(&mut app, &fixture.client, effect)
        .await
        .expect("the url opens");
    assert_eq!(last_opened_url().as_deref(), Some(url.as_str()));
    match screen_model(&app).overlay {
        Some(Overlay::Pull { url: shown }) => assert_eq!(shown, url),
        other => panic!("expected the pull overlay, got {other:?}"),
    }
}

fn phrase_at(model: &kyotoagent::screen::ScreenModel, phrase: &str) -> (u16, u16) {
    let backend = TestBackend::new(76, 24);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| kyotoagent::screen::render(model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let mut acc = String::new();
        let mut starts = Vec::new();
        for x in 0..buffer.area.width {
            starts.push(acc.len());
            acc.push_str(buffer[(x, y)].symbol());
        }
        if let Some(at) = acc.find(phrase) {
            let x = starts
                .iter()
                .position(|start| *start == at)
                .expect("phrase starts on a cell");
            return (x as u16, y);
        }
    }
    panic!("missing {phrase}");
}

#[tokio::test]
async fn a_click_on_a_result_link_opens_the_browser() {
    let mut app = idle_proof_app();
    app.cards = vec![
        Card::result("See [docs](https://example.com) now."),
        Card::proof(
            "plain proof line",
            &[("test", ItemKind::Command, Outcome::Passed)],
        ),
    ];
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let url = "https://example.com".to_string();
    let (x, y) = phrase_at(&model, "docs");
    let _cap = capture_open_url();
    let effect = released_click(&model, area, x, y).expect("link click");
    assert_eq!(effect, Effect::OpenLink(url.clone()));
    apply(&mut app, &client, effect)
        .await
        .expect("the link opens");
    assert_eq!(last_opened_url().as_deref(), Some(url.as_str()));
    assert!(!app.overlay);
}

#[tokio::test]
async fn a_click_on_a_proof_card_away_from_a_link_opens_the_overlay() {
    let mut app = idle_proof_app();
    app.cards = vec![Card::proof(
        "See [docs](https://example.com) later.",
        &[("test", ItemKind::Command, Outcome::Passed)],
    )];
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let (x, y) = phrase_at(&model, "later");
    assert_eq!(
        released_click(&model, area, x, y),
        Some(Effect::OpenOverlay)
    );
    let (link_x, link_y) = phrase_at(&model, "docs");
    let effect = released_click(&model, area, link_x, link_y).expect("proof link");
    assert_eq!(effect, Effect::OpenLink("https://example.com".to_string()));
    let _cap = capture_open_url();
    apply(&mut app, &client, effect)
        .await
        .expect("the proof link opens");
    assert!(!app.overlay);
}

async fn create_session(client: &Client, workspace: &std::path::Path) -> String {
    let body = serde_json::json!({ "workspace": workspace }).to_string();
    let (status, body) = client
        .request("POST", "/v1/sessions", Some(&body))
        .await
        .expect("the session is created");
    assert_eq!(status, 201, "{body}");
    let created: Value = serde_json::from_str(&body).expect("the reply is json");
    created["id"].as_str().expect("an id").to_string()
}

fn listed(root: &std::path::Path, id: &str) -> kyotoagent::session::Session {
    kyotoagent::session::Session::at(&root.join(kyotoagent::server::SESSIONS_DIR).join(id))
}

fn set_listed_status(root: &std::path::Path, id: &str, status: Status) {
    listed(root, id)
        .update(|meta| {
            meta.status = status;
            true
        })
        .expect("the status writes");
}

fn append_kind(root: &std::path::Path, id: &str, kind: kyotoagent::events::EventKind) {
    let event =
        kyotoagent::events::Event::new("e-wait", "2026-09-29T00:00:00.000Z", "t-wait", kind);
    listed(root, id).append(&event).expect("the event appends");
}

fn notice_has(text: &str, id: &str, phrase: &str) -> bool {
    let short = kyotoagent::screen::short_id(id);
    text.contains('\u{7}')
        && text.contains(&format!("\u{1b}]9;Kyoto Agent: {short} {phrase}\u{1b}\\"))
}

#[tokio::test]
async fn the_first_list_is_quiet_and_a_later_waiting_session_rings_once() {
    let _capture = capture_notices();
    let fixture = Fixture::new("notify-first").await;
    let waiting = create_session(&fixture.client, &fixture.workspace).await;
    set_listed_status(&fixture.root, &waiting, Status::Waiting);
    append_kind(
        &fixture.root,
        &waiting,
        kyotoagent::events::EventKind::Permission,
    );
    let selected = create_session(&fixture.client, &fixture.workspace).await;
    let mut app = fixture.app();
    app.selected = selected.clone();
    poll(&mut app, &fixture.client)
        .await
        .expect("the first list polls");
    assert_eq!(app.selected, selected);
    assert!(
        take_notices().is_empty(),
        "the first list after attach does not notify"
    );
    poll(&mut app, &fixture.client)
        .await
        .expect("the same list polls");
    assert!(take_notices().is_empty(), "nothing changed");

    let extra = create_session(&fixture.client, &fixture.workspace).await;
    set_listed_status(&fixture.root, &extra, Status::Waiting);
    append_kind(
        &fixture.root,
        &extra,
        kyotoagent::events::EventKind::Permission,
    );
    poll(&mut app, &fixture.client)
        .await
        .expect("the new session polls");
    assert_eq!(app.selected, selected);
    let rung = take_notices();
    assert!(notice_has(&rung, &extra, "needs a permission"), "{rung:?}");
    poll(&mut app, &fixture.client)
        .await
        .expect("the new session polls again");
    assert!(take_notices().is_empty(), "a waiting session rings once");
}

#[tokio::test]
async fn an_unselected_change_rings_and_the_selected_one_stays_quiet() {
    let _capture = capture_notices();
    let fixture = Fixture::new("notify-change").await;
    let watched = create_session(&fixture.client, &fixture.workspace).await;
    let selected = create_session(&fixture.client, &fixture.workspace).await;
    let mut app = fixture.app();
    app.selected = selected.clone();
    poll(&mut app, &fixture.client)
        .await
        .expect("the first list polls");
    assert!(take_notices().is_empty(), "the first list stays quiet");

    set_listed_status(&fixture.root, &watched, Status::Working);
    poll(&mut app, &fixture.client)
        .await
        .expect("working polls");
    assert!(take_notices().is_empty(), "working is quiet");

    set_listed_status(&fixture.root, &watched, Status::Waiting);
    append_kind(
        &fixture.root,
        &watched,
        kyotoagent::events::EventKind::Permission,
    );
    poll(&mut app, &fixture.client)
        .await
        .expect("waiting polls");
    let rung = take_notices();
    assert!(
        notice_has(&rung, &watched, "needs a permission"),
        "{rung:?}"
    );
    poll(&mut app, &fixture.client)
        .await
        .expect("still waiting");
    assert!(
        take_notices().is_empty(),
        "the same wait does not ring again"
    );

    set_listed_status(&fixture.root, &watched, Status::Idle);
    poll(&mut app, &fixture.client).await.expect("idle polls");
    let rung = take_notices();
    assert!(notice_has(&rung, &watched, "finished"), "{rung:?}");

    set_listed_status(&fixture.root, &selected, Status::Working);
    poll(&mut app, &fixture.client)
        .await
        .expect("the selected session works");
    set_listed_status(&fixture.root, &selected, Status::Waiting);
    append_kind(
        &fixture.root,
        &selected,
        kyotoagent::events::EventKind::Permission,
    );
    poll(&mut app, &fixture.client)
        .await
        .expect("the selected session waits");
    set_listed_status(&fixture.root, &selected, Status::Idle);
    poll(&mut app, &fixture.client)
        .await
        .expect("the selected session finishes");
    assert!(
        take_notices().is_empty(),
        "the session on screen stays quiet"
    );

    append_kind(
        &fixture.root,
        &watched,
        kyotoagent::events::EventKind::PermissionAnswer,
    );
    append_kind(
        &fixture.root,
        &watched,
        kyotoagent::events::EventKind::Question,
    );
    set_listed_status(&fixture.root, &watched, Status::Waiting);
    poll(&mut app, &fixture.client)
        .await
        .expect("the question polls");
    let rung = take_notices();
    assert!(notice_has(&rung, &watched, "needs a question"), "{rung:?}");
    assert_eq!(key(ctrl('c'), Mode::Permission, false), Some(Effect::Exit));
}

fn prose(text: &str) -> String {
    serde_json::json!({
        "choices": [{
            "message": { "role": "assistant", "content": text }
        }]
    })
    .to_string()
}

fn enhance_then_finish(draft: &str) -> Vec<Canned> {
    vec![
        Canned::Json(prose(draft)),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ]
}

async fn session_log(client: &Client, id: &str) -> String {
    let (status, body) = client
        .request("GET", &format!("/v1/sessions/{id}/events"), None)
        .await
        .expect("events");
    assert_eq!(status, 200, "{body}");
    body
}

fn user_asks(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| {
            let event: Value = serde_json::from_str(line).ok()?;
            if event["kind"].as_str() == Some("user_ask") {
                event["body"]["text"].as_str().map(str::to_string)
            } else {
                None
            }
        })
        .collect()
}

fn enhance_choices(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| {
            let event: Value = serde_json::from_str(line).ok()?;
            if event["kind"].as_str() == Some("enhance_answer") {
                event["body"]["choice"].as_str().map(str::to_string)
            } else {
                None
            }
        })
        .collect()
}

fn enhance_open(app: &App) -> bool {
    matches!(screen_model(app).overlay, Some(Overlay::Enhance { .. }))
}

async fn turn_enhance_on(app: &mut App, client: &Client) {
    type_text(app, client, "/enhance on").await;
    drive(app, client, press(KeyCode::Enter)).await;
    assert!(app.enhance);
    assert!(app.ask.is_empty());
    assert!(draw(app).contains("enhance"));
}

async fn send_for_card(app: &mut App, client: &Client, text: &str) {
    type_text(app, client, text).await;
    drive(app, client, press(KeyCode::Enter)).await;
    assert!(app.ask.is_empty());
    wait_until(app, client, |app| {
        enhance_open(app)
            && app
                .selected_session()
                .is_some_and(|row| row.status == Status::Waiting)
    })
    .await;
}

#[tokio::test]
async fn enhance_on_shows_the_card_and_use_posts_the_draft() {
    let fixture = Fixture::with_replies(
        "enhance-use",
        enhance_then_finish("Do the thing carefully."),
    )
    .await;
    let mut app = idle_session(&fixture).await;
    turn_enhance_on(&mut app, &fixture.client).await;
    send_for_card(&mut app, &fixture.client, "ship it").await;
    let log = session_log(&fixture.client, &app.selected).await;
    assert!(user_asks(&log).is_empty(), "{log}");
    assert!(log.contains("enhance_request"), "{log}");
    let drawn = draw(&app);
    assert!(drawn.contains("ship it"), "{drawn}");
    assert!(drawn.contains("Do the thing carefully."), "{drawn}");
    drive(&mut app, &fixture.client, press(KeyCode::Char('u'))).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    let log = session_log(&fixture.client, &app.selected).await;
    assert_eq!(enhance_choices(&log), vec!["use".to_string()], "{log}");
    assert_eq!(
        user_asks(&log),
        vec!["Do the thing carefully.".to_string()],
        "{log}"
    );
}

#[tokio::test]
async fn discard_and_esc_drop_the_enhance_card() {
    let fixture = Fixture::with_replies(
        "enhance-discard",
        vec![
            Canned::Json(prose("Leave this draft.")),
            Canned::Json(prose("Leave this draft.")),
        ],
    )
    .await;
    let mut app = idle_session(&fixture).await;
    turn_enhance_on(&mut app, &fixture.client).await;
    send_for_card(&mut app, &fixture.client, "ship it").await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('x'))).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
    })
    .await;
    assert!(!enhance_open(&app));
    let log = session_log(&fixture.client, &app.selected).await;
    assert_eq!(enhance_choices(&log), vec!["discard".to_string()], "{log}");
    assert!(user_asks(&log).is_empty(), "{log}");

    send_for_card(&mut app, &fixture.client, "ship it").await;
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
    })
    .await;
    assert!(!enhance_open(&app));
    let log = session_log(&fixture.client, &app.selected).await;
    assert_eq!(
        enhance_choices(&log),
        vec!["discard".to_string(), "discard".to_string()],
        "{log}"
    );
    assert!(user_asks(&log).is_empty(), "{log}");
}

#[tokio::test]
async fn edit_then_enter_posts_revise_and_a_blank_enter_does_not() {
    let fixture = Fixture::with_replies(
        "enhance-revise",
        enhance_then_finish("Do the thing carefully."),
    )
    .await;
    let mut app = idle_session(&fixture).await;
    turn_enhance_on(&mut app, &fixture.client).await;
    send_for_card(&mut app, &fixture.client, "ship it").await;
    drive(&mut app, &fixture.client, press(KeyCode::Char('e'))).await;
    assert_eq!(app.ask, "Do the thing carefully.");
    assert!(enhance_open(&app));
    while !app.ask.is_empty() {
        drive(&mut app, &fixture.client, press(KeyCode::Backspace)).await;
    }
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    let log = session_log(&fixture.client, &app.selected).await;
    assert!(enhance_choices(&log).is_empty(), "{log}");
    assert!(enhance_open(&app));
    type_text(&mut app, &fixture.client, "the client wording").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
            && kinds(app).contains(&"result")
    })
    .await;
    let log = session_log(&fixture.client, &app.selected).await;
    assert_eq!(enhance_choices(&log), vec!["revise".to_string()], "{log}");
    assert_eq!(
        user_asks(&log),
        vec!["the client wording".to_string()],
        "{log}"
    );
}

#[tokio::test]
async fn retry_posts_when_the_card_has_an_error() {
    let fixture = Fixture::with_replies(
        "enhance-retry",
        vec![
            Canned::Status(500, "down".into()),
            Canned::Json(prose("A better prompt.")),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
            )])),
        ],
    )
    .await;
    let mut app = idle_session(&fixture).await;
    turn_enhance_on(&mut app, &fixture.client).await;
    send_for_card(&mut app, &fixture.client, "ship it").await;
    assert!(matches!(
        screen_model(&app).overlay,
        Some(Overlay::Enhance { error: Some(_), .. })
    ));
    drive(&mut app, &fixture.client, press(KeyCode::Char('r'))).await;
    wait_until(&mut app, &fixture.client, |app| {
        matches!(
            screen_model(app).overlay,
            Some(Overlay::Enhance {
                text,
                error: None,
                ..
            }) if text == "A better prompt."
        )
    })
    .await;
    let log = session_log(&fixture.client, &app.selected).await;
    assert_eq!(enhance_choices(&log), vec!["retry".to_string()], "{log}");
    drive(&mut app, &fixture.client, press(KeyCode::Char('u'))).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.selected_session()
            .is_some_and(|row| row.status == Status::Idle)
    })
    .await;
    let log = session_log(&fixture.client, &app.selected).await;
    assert_eq!(
        user_asks(&log),
        vec!["A better prompt.".to_string()],
        "{log}"
    );
}

#[tokio::test]
async fn ctrl_x_during_the_rewrite_cancels_and_restores_the_prompt() {
    let fixture = Fixture::with_replies(
        "enhance-cancel",
        vec![Canned::Json(prose("Do the thing carefully."))],
    )
    .await;
    fixture._fake.pause_before(0);
    let mut app = idle_session(&fixture).await;
    turn_enhance_on(&mut app, &fixture.client).await;
    type_text(&mut app, &fixture.client, "ship it").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        poll(&mut app, &fixture.client)
            .await
            .expect("the rewrite polls");
        if fixture._fake.is_holding()
            && app
                .selected_session()
                .is_some_and(|row| row.status == Status::Working)
        {
            break;
        }
        assert!(Instant::now() < deadline, "the rewrite did not hold");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!enhance_open(&app));
    drive(&mut app, &fixture.client, ctrl('x')).await;
    wait_until(&mut app, &fixture.client, |app| {
        app.ask == "ship it"
            && app
                .selected_session()
                .is_some_and(|row| row.status == Status::Idle)
    })
    .await;
    assert!(!enhance_open(&app));
    let log = session_log(&fixture.client, &app.selected).await;
    assert!(log.contains("enhance_request"), "{log}");
    assert!(user_asks(&log).is_empty(), "{log}");
    fixture._fake.release();
}

#[tokio::test]
async fn goal_status_while_answering_a_question_keeps_the_question_open() {
    let (fixture, mut app) = start_question("goal-status-question", &[]).await;
    type_text(&mut app, &fixture.client, "/goal status").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert_eq!(
        app.selected_session().expect("session").status,
        Status::Waiting
    );
    let (_, body) = fixture
        .client
        .request("GET", &format!("/v1/sessions/{}/view", app.selected), None)
        .await
        .expect("view");
    let view: View = serde_json::from_str(&body).expect("view");
    assert!(view.cards.iter().any(|card| card.kind == CardKind::Result
        && card.body["text"]
            .as_str()
            .is_some_and(|text| text.contains("No goal set"))));
    assert!(view
        .cards
        .iter()
        .any(|card| card.kind == CardKind::Question && card.body["answer"].is_null()));
    assert!(!view.cards.iter().any(|card| card.kind == CardKind::Answer));
    type_text(&mut app, &fixture.client, "yes").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
}

#[tokio::test]
async fn an_image_only_drop_sends_pixels_and_opens_the_saved_preview() {
    let fixture =
        Fixture::with_replies("image-only", vec![Canned::Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":"I see a dog."},"finish_reason":"stop"}]}).to_string())]).await;
    let mut app = fixture.app();
    new_here(&mut app, &fixture.client).await;
    let path = fixture.workspace.join("dog image.png");
    fs::write(&path, kyotoagent::splash::PNG).unwrap();
    apply(
        &mut app,
        &fixture.client,
        Effect::Paste(format!("'{}'", path.display())),
    )
    .await
    .unwrap();
    assert!(app.ask.is_empty());
    assert_eq!(screen_model(&app).pending_images.len(), 1);
    let model = screen_model(&app);
    let click = (0..app.area.height)
        .flat_map(|row| (0..app.area.width).map(move |column| (column, row)))
        .find_map(|(column, row)| {
            mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
                &model,
                app.area,
            )
            .filter(|effect| matches!(effect, Effect::OpenImage(_)))
        })
        .expect("image chip is clickable");
    apply(&mut app, &fixture.client, click).await.unwrap();
    assert!(matches!(
        screen_model(&app).overlay,
        Some(Overlay::Image { .. })
    ));
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    let missing = Client::at(fixture.root.join("missing.sock"));
    assert!(apply(&mut app, &missing, Effect::Submit).await.is_err());
    assert_eq!(screen_model(&app).pending_images.len(), 1);
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(screen_model(&app).pending_images.is_empty());
    wait_until(&mut app, &fixture.client, |app| {
        app.cards
            .iter()
            .any(|card| matches!(card, Card::Result { text } if text == "I see a dog."))
    })
    .await;
    let saved = app
        .cards
        .iter()
        .find_map(|card| match card {
            Card::Ask { images, .. } => images.first(),
            _ => None,
        })
        .unwrap()
        .clone();
    assert_eq!(
        saved,
        kyotoagent::attachment::ImageAttachment::from_bytes(
            "dog image.png",
            kyotoagent::splash::PNG
        )
        .unwrap()
    );
    apply(&mut app, &fixture.client, Effect::OpenImage(saved))
        .await
        .unwrap();
    assert!(matches!(
        screen_model(&app).overlay,
        Some(Overlay::Image { .. })
    ));
    drive(&mut app, &fixture.client, press(KeyCode::Esc)).await;
    assert!(screen_model(&app).overlay.is_none());
    let bodies = fixture._fake.received.lock().unwrap();
    assert!(bodies
        .iter()
        .any(|(_, body)| body.contains("data:image/png;base64,")));
}

#[tokio::test]
async fn choosing_an_identical_model_id_keeps_the_selected_provider() {
    let fixture = Fixture::with_replies("duplicate-provider-model", finish_only("done")).await;
    fixture
        ._fake
        .set_models(&serde_json::json!({"data": [{"id": "shared-model"}]}).to_string());
    fs::write(fixture.home.join("config.toml"), format!("provider = \"first\"\ntitle_model = \"\"\n[providers.first]\nbase_url = \"{}\"\nmodel = \"shared-model\"\n[providers.second]\nbase_url = \"{}\"\nmodel = \"shared-model\"\n", fixture._fake.base_url(), fixture._fake.base_url())).unwrap();
    let mut app = idle_session(&fixture).await;
    drive(&mut app, &fixture.client, ctrl('m')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Down)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    let config = Config::load(&fixture.home.join("config.toml")).unwrap();
    assert_eq!(config.provider.as_deref(), Some("second"));
    assert_eq!(config.model, "shared-model");
}

#[tokio::test]
async fn deleting_a_dirty_workspace_requires_the_checkbox_and_a_second_decision() {
    let fixture = Fixture::new("delete-dirty-workspace").await;
    git_init(&fixture.workspace);
    let body = serde_json::json!({"workspace": fixture.workspace, "worktree": true}).to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&body))
        .await
        .unwrap();
    assert_eq!(status, 201);
    let created: Value = serde_json::from_str(&response).unwrap();
    let workspace = PathBuf::from(created["workspace"].as_str().unwrap());
    fs::write(workspace.join("unsaved.txt"), "uncommitted work").unwrap();
    let mut app = fixture.app();
    poll(&mut app, &fixture.client).await.unwrap();
    drive(&mut app, &fixture.client, ctrl('w')).await;
    assert!(
        matches!(screen_model(&app).overlay, Some(Overlay::Delete { remove_workspace: false, warning: Some(text), .. }) if text.contains("unsaved.txt"))
    );
    drive(&mut app, &fixture.client, press(KeyCode::Char(' '))).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(workspace.exists());
    assert!(matches!(
        screen_model(&app).overlay,
        Some(Overlay::Delete {
            confirm_dirty: true,
            highlight: 2,
            ..
        })
    ));
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(workspace.exists());
    assert_eq!(app.sessions.len(), 1);
    drive(&mut app, &fixture.client, ctrl('w')).await;
    drive(&mut app, &fixture.client, press(KeyCode::Char(' '))).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Up)).await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    assert!(!workspace.exists());
    assert!(fixture.workspace.exists());
    assert!(app.sessions.is_empty());
}

#[tokio::test]
async fn the_save_layout_command_persists_and_applies_to_other_clients_and_sessions() {
    let fixture = Fixture::new("saved-layout").await;
    let mut app = idle_session(&fixture).await;
    let area = Rect::new(0, 0, 76, 24);
    app.area = area;
    type_text(&mut app, &fixture.client, "/schedules").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    apply(&mut app, &fixture.client, Effect::ToggleLeft)
        .await
        .unwrap();
    type_text(&mut app, &fixture.client, "draft kept while saving").await;
    apply(&mut app, &fixture.client, Effect::OpenPalette)
        .await
        .unwrap();
    type_text(&mut app, &fixture.client, "Save layout").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    for _ in 0..100 {
        kyotoagent::tui::advance_requests(&mut app, &fixture.client).await;
        if app.notice.as_deref() == Some("Layout saved for all sessions on this server.") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        app.notice.as_deref(),
        Some("Layout saved for all sessions on this server.")
    );
    assert_eq!(screen_model(&app).bottom, "draft kept while saving");
    assert_eq!(screen_model(&app).toast, app.notice);
    type_text(&mut app, &fixture.client, "!").await;
    assert_eq!(screen_model(&app).bottom, "draft kept while saving!");
    assert!(screen_model(&app).toast.is_some());
    apply(&mut app, &fixture.client, Effect::DismissNotice)
        .await
        .unwrap();
    assert!(screen_model(&app).toast.is_none());
    assert_eq!(app.ask, "draft kept while saving!");
    app.ask.clear();
    let mut other = fixture.app();
    poll(&mut other, &fixture.client).await.unwrap();
    let model = screen_model(&other);
    assert!(!model.left_open);
    assert!(model.right_open);
    assert!(model
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Schedules));
    new_here(&mut other, &fixture.client).await;
    assert_eq!(screen_model(&other).right_panes, model.right_panes);
    apply(
        &mut other,
        &fixture.client,
        Effect::TogglePane(kyotoagent::screen::RightPane::Tasks),
    )
    .await
    .unwrap();
    poll(&mut other, &fixture.client).await.unwrap();
    assert!(screen_model(&other)
        .right_panes
        .contains(&kyotoagent::screen::RightPane::Tasks));
    type_text(&mut app, &fixture.client, "/schedules").await;
    drive(&mut app, &fixture.client, press(KeyCode::Enter)).await;
    apply(&mut app, &fixture.client, Effect::SaveLayout)
        .await
        .unwrap();
    for _ in 0..100 {
        kyotoagent::tui::advance_requests(&mut app, &fixture.client).await;
        if app.notice.as_deref() == Some("Layout saved for all sessions on this server.") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    poll(&mut other, &fixture.client).await.unwrap();
    assert!(!screen_model(&other).right_open);
    assert!(screen_model(&other).right_panes.is_empty());
}
