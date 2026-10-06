//! The server on a temp socket, driven by a fake chat server.
//!
//! Everything here goes through the HTTP routes: a [`Server`] is started on a
//! socket in a temporary root, the fake server answers each completion with a
//! canned reply, and a test scripts a whole turn over the socket the way a
//! second terminal would.
//!
//! What these tests are really about is the contract of the routes: the socket
//! is private, a second serve does not replace a live one, a busy session is
//! 409 while its sibling stays reachable, a settled card refuses a second
//! answer, a waiting row says what it waits on, and the quiet view hides what
//! the raw log keeps.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kyotoagent::config::Config;
use kyotoagent::server::{Server, ServerError, SOCKET_FILE};
use kyotoagent::session::github_origin;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn image_uploads_above_two_mib_are_accepted_and_upload_limits_are_enforced() {
    let fixture = Fixture::new("large-image-upload", finish_turn("A picture.")).await;
    let id = fixture.add_session("images").await;
    let mut seed = 1u32;
    let pixels = image::RgbImage::from_fn(1024, 768, |_, _| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        image::Rgb([(seed >> 16) as u8, (seed >> 8) as u8, seed as u8])
    });
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(pixels)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    assert!(encoded.get_ref().len() > 2 * 1024 * 1024);
    let image = kyotoagent::attachment::ImageAttachment::from_bytes("large.png", encoded.get_ref())
        .unwrap();
    let (status, body) = fixture
        .client
        .message_body(
            &id,
            serde_json::json!({"text":"describe", "images":[image.clone()]}),
        )
        .await;
    assert_eq!(status, 202, "{body}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = fixture.client.view(&id).await;
        if view["cards"]
            .as_array()
            .is_some_and(|cards| !cards.is_empty())
        {
            assert_eq!(view["cards"][0]["body"]["images"][0]["data"], image.data);
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fixture.client.wait_for_status(&id, "idle").await;
    let oversized = kyotoagent::attachment::ImageAttachment {
        data: "A".repeat(kyotoagent::attachment::MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4),
        ..image
    };
    let (status, _) = fixture
        .client
        .message_body(&id, serde_json::json!({"text":"", "images":[oversized]}))
        .await;
    assert_eq!(status, 400);
    let (status, _) = fixture
        .client
        .message_body(
            &id,
            serde_json::json!({"text":"x".repeat(30 * 1024 * 1024)}),
        )
        .await;
    assert_eq!(status, 413);
}

/// What the fake server answers with.
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

/// A local HTTP server that answers each completion with a canned reply from a
/// queue. The last reply repeats, so a turn that asks more questions than the
/// test scripted still gets an answer.
struct FakeServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    connection: Arc<Mutex<Option<TcpStream>>>,
    handle: Option<JoinHandle<()>>,
    models: Arc<Mutex<Option<String>>>,
    bodies: Arc<Mutex<Vec<String>>>,
    title_model: Arc<Mutex<Option<String>>>,
    title_reply: Arc<Mutex<Option<Canned>>>,
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
        let models = Arc::new(Mutex::new(None));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let title_model = Arc::new(Mutex::new(None));
        let title_reply = Arc::new(Mutex::new(None));
        let chat_hold = Arc::new(ChatHold {
            pause_at: Mutex::new(None),
            served: AtomicUsize::new(0),
            go: Mutex::new(false),
            cv: Condvar::new(),
            holding: AtomicBool::new(false),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let connection = Arc::new(Mutex::new(None));

        let handle = {
            let replies = Arc::clone(&replies);
            let models = Arc::clone(&models);
            let bodies = Arc::clone(&bodies);
            let title_model = Arc::clone(&title_model);
            let title_reply = Arc::clone(&title_reply);
            let chat_hold = Arc::clone(&chat_hold);
            let stop = Arc::clone(&stop);
            let connection = Arc::clone(&connection);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let mut current = connection.lock().expect("the active connection");
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            *current = Some(stream.try_clone().expect("the connection clones"));
                            drop(current);
                            let _ = stream.set_nonblocking(false);
                            serve_one(
                                stream,
                                &replies,
                                &models,
                                &bodies,
                                &title_model,
                                &title_reply,
                                &chat_hold,
                            );
                            *connection.lock().expect("the active connection") = None;
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
            connection,
            handle: Some(handle),
            models,
            bodies,
            title_model,
            title_reply,
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

    fn set_title(&self, model: &str, reply: Canned) {
        *self.title_model.lock().expect("the title model") = Some(model.to_string());
        *self.title_reply.lock().expect("the title reply") = Some(reply);
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().expect("the bodies").clone()
    }

    fn set_models(&self, body: &str) {
        *self.models.lock().expect("the models slot is not poisoned") = Some(body.to_string());
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.release();
        if let Some(connection) = self
            .connection
            .lock()
            .expect("the active connection")
            .take()
        {
            let _ = connection.shutdown(std::net::Shutdown::Both);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn fake_server_shutdown_does_not_wait_for_a_partial_request() {
    let server = FakeServer::start(Vec::new());
    let mut stream = TcpStream::connect(server.addr).expect("the client connects");
    stream
        .write_all(b"POST /chat HTTP/1.1\r\n")
        .expect("the partial header sends");
    let deadline = Instant::now() + Duration::from_secs(2);
    while server.connection.lock().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "the server did not accept the client"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let (done, finished) = std::sync::mpsc::channel();
    let dropping = std::thread::spawn(move || {
        drop(server);
        done.send(()).unwrap();
    });
    let completion = finished.recv_timeout(Duration::from_secs(1));
    drop(stream);
    dropping.join().unwrap();
    assert!(
        completion.is_ok(),
        "shutdown waited for the client to finish its request"
    );
}

fn serve_one(
    mut stream: TcpStream,
    replies: &Arc<Mutex<Vec<Canned>>>,
    models: &Arc<Mutex<Option<String>>>,
    bodies: &Arc<Mutex<Vec<String>>>,
    title_model: &Arc<Mutex<Option<String>>>,
    title_reply: &Arc<Mutex<Option<Canned>>>,
    chat_hold: &Arc<ChatHold>,
) {
    let Some((path, request_body)) = read_request(&mut stream) else {
        return;
    };
    if path.contains("/models") {
        let catalog = models
            .lock()
            .expect("the models slot is not poisoned")
            .clone()
            .unwrap_or_else(|| {
                serde_json::json!({
                    "data": [{ "id": "test/model", "context_length": 200000 }]
                })
                .to_string()
            });
        respond(&mut stream, 200, &catalog);
        return;
    }
    bodies
        .lock()
        .expect("the bodies")
        .push(request_body.clone());
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
    let scripted = title_model.lock().expect("the title model").clone();
    if scripted
        .as_deref()
        .is_some_and(|model| request_model(&request_body).as_deref() == Some(model))
    {
        if let Some(reply) = title_reply.lock().expect("the title reply").clone() {
            match reply {
                Canned::Json(body) => {
                    kyotoagent::chat::answer_completion(&mut stream, 200, &body, &request_body)
                }
                Canned::Status(status, body) => respond(&mut stream, status, &body),
            }
            return;
        }
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

fn request_model(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body).ok().and_then(|value| {
        value
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
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

/// A tool call reply: the model wants tools run. The arguments are the JSON
/// string the server sends, so a value here is the arguments object.
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

/// A write the model wants to do, then a finish.
fn write_then_finish() -> Vec<Canned> {
    vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "notes.md", "contents": "hello" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Created notes.md.", "proof": "cargo test passed." }),
        )])),
    ]
}

/// A client for the server's socket, speaking just enough HTTP.
struct Client {
    socket: PathBuf,
}

impl Client {
    fn new(socket: PathBuf) -> Client {
        Client { socket }
    }

    /// Wait until the server is answering on the socket. A stale socket file
    /// left by a dead server is there before the new one binds, so this waits
    /// for a connection that actually succeeds.
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

    /// One request, and the status and body of the answer.
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
                let head = String::from_utf8_lossy(&raw[..at]);
                let content_length = content_length(&head);
                if raw.len() >= at + 4 + content_length {
                    break;
                }
            }
            let n = stream.read(&mut chunk).await.expect("the answer reads");
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
        }
        let text = String::from_utf8_lossy(&raw);
        split_response(&text)
    }

    /// Create a session for a workspace, and its id.
    async fn create_session(&self, workspace: &str) -> String {
        let body = serde_json::json!({ "workspace": workspace }).to_string();
        let (status, response) = self.request("POST", "/v1/sessions", Some(&body)).await;
        assert_eq!(status, 201, "the session is created: {response}");
        let json: Value = serde_json::from_str(&response).expect("the reply is JSON");
        json["id"].as_str().expect("an id").to_string()
    }

    /// The sessions as the list route sees them.
    async fn list(&self) -> Vec<Value> {
        let (status, response) = self.request("GET", "/v1/sessions", None).await;
        assert_eq!(status, 200, "the sessions list: {response}");
        serde_json::from_str(&response).expect("the list is JSON")
    }

    /// The view of one session.
    async fn view(&self, id: &str) -> Value {
        let (status, response) = self
            .request("GET", &format!("/v1/sessions/{id}/view"), None)
            .await;
        assert_eq!(status, 200, "the view reads: {response}");
        serde_json::from_str(&response).expect("the view is JSON")
    }

    /// The raw log of one session.
    async fn events(&self, id: &str) -> String {
        let (status, response) = self
            .request("GET", &format!("/v1/sessions/{id}/events"), None)
            .await;
        assert_eq!(status, 200, "the log reads: {response}");
        response
    }

    /// Start a turn on a session.
    async fn message(&self, id: &str, text: &str) -> (u16, String) {
        self.message_body(id, serde_json::json!({ "text": text }))
            .await
    }

    async fn message_body(&self, id: &str, body: Value) -> (u16, String) {
        let body = body.to_string();
        self.request("POST", &format!("/v1/sessions/{id}/messages"), Some(&body))
            .await
    }

    /// Answer the open card of a session.
    async fn answer(&self, id: &str, event_id: &str, choice: &str) -> (u16, String) {
        let body = serde_json::json!({ "id": event_id, "choice": choice }).to_string();
        self.request("POST", &format!("/v1/sessions/{id}/answers"), Some(&body))
            .await
    }

    async fn answer_text(
        &self,
        id: &str,
        event_id: &str,
        choice: &str,
        text: &str,
    ) -> (u16, String) {
        let body =
            serde_json::json!({ "id": event_id, "choice": choice, "text": text }).to_string();
        self.request("POST", &format!("/v1/sessions/{id}/answers"), Some(&body))
            .await
    }

    /// Cancel the turn of a session.
    async fn cancel(&self, id: &str) -> (u16, String) {
        self.request("POST", &format!("/v1/sessions/{id}/cancel"), None)
            .await
    }

    /// Wait until the session's view reads `status`.
    async fn wait_for_status(&self, id: &str, status: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let view = self.view(id).await;
            if view["status"].as_str() == Some(status) {
                return view;
            }
            assert!(
                Instant::now() < deadline,
                "the session did not reach {status}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The event id of the open permission card, from the raw log.
    async fn open_permission_id(&self, id: &str) -> String {
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
        open.expect("an open permission")
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

/// A server on a temp socket, a fake chat server behind it, and a workspace.
/// The temporary directories go when the fixture does, so a failed test leaves
/// nothing behind for the next run to trip over.
struct Fixture {
    _handle: tokio::task::JoinHandle<()>,
    _fake: FakeServer,
    root: PathBuf,
    client: Client,
    runner: Arc<kyotoagent::turn::Runner>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A turn the test left waiting on a card is blocked on the gate. Answer
        // it, so the turn finishes and the runtime can shut down.
        self.runner.release_all();
        self._handle.abort();
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    async fn new(name: &str, replies: Vec<Canned>) -> Fixture {
        let root = test_temp_dir().join(format!("kyotoagent-server-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the root exists");
        let fake = FakeServer::start(replies);
        let config_text = format!(
            "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
            fake.base_url()
        );
        Fixture::boot(root, fake, config_text).await
    }

    async fn titled(name: &str, replies: Vec<Canned>, title_model: &str) -> Fixture {
        let root = test_temp_dir().join(format!("kyotoagent-server-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the root exists");
        let fake = FakeServer::start(replies);
        let config_text = format!(
            "provider = \"local\"\ntitle_model = \"{title_model}\"\neffort = \"high\"\n\n[providers.local]\nbase_url = \"{}\"\nmodel = \"test/model\"\n",
            fake.base_url()
        );
        Fixture::boot(root, fake, config_text).await
    }

    async fn omitted_title(name: &str, replies: Vec<Canned>) -> Fixture {
        let root = test_temp_dir().join(format!("kyotoagent-server-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the root exists");
        let fake = FakeServer::start(replies);
        let config_text = format!(
            "provider = \"local\"\n\n[providers.local]\nbase_url = \"{}\"\nmodel = \"test/model\"\n",
            fake.base_url()
        );
        Fixture::boot(root, fake, config_text).await
    }

    async fn boot(root: PathBuf, fake: FakeServer, config_text: String) -> Fixture {
        fs::write(root.join("config.toml"), &config_text).expect("the config writes");
        let config = Config::from_toml(&config_text).expect("the config parses");
        let server = Server::new(&root, &config).expect("the server is built");
        let runner = Arc::clone(server.runner());
        let handle = tokio::spawn(async move {
            let _ = server.serve().await;
        });
        let client = Client::new(root.join(SOCKET_FILE));
        client.wait_for_socket().await;
        Fixture {
            _handle: handle,
            _fake: fake,
            root,
            client,
            runner,
        }
    }

    /// A workspace under the root, and a session for it. The session id is the
    /// one the server generated, which is not the workspace name.
    async fn add_session(&self, name: &str) -> String {
        let workspace = self.root.join(format!("w-{name}"));
        fs::create_dir_all(&workspace).expect("the workspace exists");
        self.client
            .create_session(workspace.to_str().expect("a path"))
            .await
    }

    /// The workspace a session was added to.
    fn workspace(&self, name: &str) -> PathBuf {
        self.root.join(format!("w-{name}"))
    }

    fn pause_before(&self, index: usize) {
        self._fake.pause_before(index);
    }

    fn release_chat(&self) {
        self._fake.release();
    }

    fn chat_is_holding(&self) -> bool {
        self._fake.is_holding()
    }
}

/// The socket is private, and a second serve exits without replacing the live
/// one.
#[tokio::test]
async fn the_socket_is_private_and_a_second_serve_exits() {
    let fixture = Fixture::new("private", write_then_finish()).await;

    // The socket is mode 0600.
    let mode = fs::metadata(fixture.root.join(SOCKET_FILE))
        .expect("the socket is there")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the socket is private");

    // A second serve on the same root exits and says so.
    let config = Config::from_toml(&format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
        fixture._fake.base_url()
    ))
    .expect("the config parses");
    let second = Server::new(&fixture.root, &config).expect("the second server is built");
    let error = second.serve().await.expect_err("the second serve exits");
    assert!(
        matches!(error, ServerError::AlreadyServing),
        "the second serve says the socket is live: {error}"
    );

    // The first server is still serving.
    let sessions = fixture.client.list().await;
    assert!(sessions.is_empty(), "the first server answers");
}

/// Two sessions can be created. A message to a busy session is 409; a message
/// to the other session is 202.
#[tokio::test]
async fn a_busy_session_is_refused_and_its_sibling_starts() {
    let fixture = Fixture::new("busy", write_then_finish()).await;
    let first = fixture.add_session("91bc").await;
    let second = fixture.add_session("3f2a").await;

    // The first session starts a turn and stops on the write permission.
    let (status, response) = fixture.client.message(&first, "Create notes.md.").await;
    assert_eq!(status, 202, "the turn starts: {response}");
    fixture.client.wait_for_status(&first, "waiting").await;

    // A second message to the busy session is 409.
    let (status, response) = fixture.client.message(&first, "Again.").await;
    assert_eq!(status, 409, "the busy session is refused: {response}");

    // The sibling session is free and starts its own turn.
    let (status, response) = fixture.client.message(&second, "Finish this.").await;
    assert_eq!(status, 202, "the sibling starts: {response}");
    fixture.client.wait_for_status(&second, "idle").await;

    // The first session is still waiting on its permission.
    assert_eq!(
        fixture.client.view(&first).await["status"].as_str(),
        Some("waiting")
    );
}

/// An answer to an already settled card is 409. The first answer wins.
#[tokio::test]
async fn an_answer_to_a_settled_card_is_refused() {
    let fixture = Fixture::new("settled", write_then_finish()).await;
    let id = fixture.add_session("91bc").await;

    fixture.client.message(&id, "Create notes.md.").await;
    fixture.client.wait_for_status(&id, "waiting").await;
    let permission = fixture.client.open_permission_id(&id).await;

    // The first answer lands.
    let (status, response) = fixture.client.answer(&id, &permission, "allow_once").await;
    assert_eq!(status, 204, "the first answer lands: {response}");

    // The turn finishes.
    fixture.client.wait_for_status(&id, "idle").await;

    // A second answer to the same card is 409.
    let (status, response) = fixture.client.answer(&id, &permission, "deny").await;
    assert_eq!(status, 409, "the settled card refuses: {response}");
}

/// A session waiting for a write says `waiting` and `permission` on the list.
#[tokio::test]
async fn a_waiting_session_says_what_it_waits_on() {
    let fixture = Fixture::new("waiting-row", write_then_finish()).await;
    let id = fixture.add_session("91bc").await;

    fixture.client.message(&id, "Create notes.md.").await;
    fixture.client.wait_for_status(&id, "waiting").await;

    let rows = fixture.client.list().await;
    assert_eq!(rows.len(), 1, "one session");
    assert_eq!(rows[0]["status"].as_str(), Some("waiting"));
    assert_eq!(rows[0]["waiting"].as_str(), Some("permission"));
}

/// After a fake-model turn is allowed to write, the view shows the result card
/// and no tool name, and the raw log still has the write tool call.
#[tokio::test]
async fn the_quiet_view_hides_what_the_raw_log_keeps() {
    let fixture = Fixture::new("quiet-view", write_then_finish()).await;
    let id = fixture.add_session("91bc").await;

    fixture.client.message(&id, "Create notes.md.").await;
    fixture.client.wait_for_status(&id, "waiting").await;
    let permission = fixture.client.open_permission_id(&id).await;
    fixture.client.answer(&id, &permission, "allow_once").await;
    let view = fixture.client.wait_for_status(&id, "idle").await;

    // The view includes the proof of the write after the result. No tool name.
    let cards = view["cards"].as_array().expect("cards");
    let kinds: Vec<&str> = cards
        .iter()
        .map(|card| card["kind"].as_str().expect("a kind"))
        .collect();
    assert_eq!(kinds, vec!["ask", "result", "proof"]);
    let json = serde_json::to_string(cards).expect("cards serialize");
    assert!(!json.contains("write_file"), "no tool name: {json}");
    assert!(!json.contains("read_file"), "no tool name: {json}");
    assert!(!json.contains("run"), "no tool name: {json}");

    // The file was written.
    assert_eq!(
        fs::read_to_string(fixture.workspace("91bc").join("notes.md")).expect("the file reads"),
        "hello"
    );

    // The raw log still has the write tool call.
    let log = fixture.client.events(&id).await;
    assert!(log.contains("write_file"), "the write is in the log: {log}");
    assert!(
        log.contains("Created notes.md."),
        "the result is in the log: {log}"
    );
}

/// Restarting serve leaves an unanswered permission waiting, and the answer
/// still lands.
#[tokio::test]
async fn restarting_leaves_an_unanswered_permission_waiting() {
    let root = test_temp_dir().join(format!("kyotoagent-server-restart-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");

    // The first server starts a turn that stops on a write permission.
    let fake = FakeServer::start(write_then_finish());
    let config_text = format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
        fake.base_url()
    );
    let mut fixture = Fixture::boot(root.clone(), fake, config_text).await;
    let runner = Arc::clone(&fixture.runner);
    let client = &fixture.client;

    let workspace = root.join("w-91bc");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    let id = client
        .create_session(workspace.to_str().expect("a path"))
        .await;
    client.message(&id, "Create notes.md.").await;
    client.wait_for_status(&id, "waiting").await;
    let permission = client.open_permission_id(&id).await;

    // The server dies. The socket file is left behind, stale. The turn it left
    // waiting stays blocked on its gate, so the session on disk is still
    // waiting.
    fixture._handle.abort();
    let _ = (&mut fixture._handle).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while tokio::net::UnixStream::connect(root.join(SOCKET_FILE))
        .await
        .is_ok()
    {
        assert!(
            Instant::now() < deadline,
            "the first server still accepts connections"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The second server starts on the same root. The stale socket is replaced.
    let fake2 = FakeServer::start(write_then_finish());
    let config2 = Config::from_toml(&format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
        fake2.base_url()
    ))
    .expect("the config parses");
    let server2 = Server::new(&root, &config2).expect("the second server is built");
    fixture._handle = tokio::spawn(async move {
        let _ = server2.serve().await;
    });
    let client2 = Client::new(root.join(SOCKET_FILE));
    client2.wait_for_socket().await;

    let rows = client2.list().await;
    assert_eq!(rows[0]["status"].as_str(), Some("waiting"));
    assert_eq!(rows[0]["waiting"].as_str(), Some("permission"));

    let (status, response) = client2.message(&id, "Again.").await;
    assert_eq!(
        status, 409,
        "a waiting session stays busy after the restart: {response}"
    );

    let (status, response) = client2.answer(&id, &permission, "allow_once").await;
    assert_eq!(
        status, 204,
        "the answer lands after the restart: {response}"
    );
    client2.wait_for_status(&id, "idle").await;

    // The first server's turn is still blocked on its gate. Answer it, so it
    // finishes and the runtime can shut down.
    runner.release_all();
    fixture._handle.abort();
    let _ = (&mut fixture._handle).await;
    let _ = fs::remove_dir_all(&root);
}

/// A question the model asks says `question` on the list, and the reply text
/// answers it.
#[tokio::test]
async fn a_question_is_answered_by_its_reply_text() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({ "text": "Which title?", "choices": ["Kyoto Agent", "Kyoto Agent CLI"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("question", replies).await;
    let id = fixture.add_session("91bc").await;

    fixture.client.message(&id, "Name the binary.").await;
    fixture.client.wait_for_status(&id, "waiting").await;

    // The row says the session waits on a question.
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["waiting"].as_str(), Some("question"));

    // The question event id answers with the reply text.
    let question = fixture
        .client
        .events(&id)
        .await
        .lines()
        .find_map(|line| {
            let event: Value = serde_json::from_str(line).ok()?;
            (event["kind"].as_str() == Some("question"))
                .then(|| event["id"].as_str().unwrap().to_string())
        })
        .expect("a question event");
    let (status, response) = fixture.client.answer(&id, &question, "Kyoto Agent").await;
    assert_eq!(status, 204, "the reply answers the question: {response}");

    let view = fixture.client.wait_for_status(&id, "idle").await;
    let cards = view["cards"].as_array().expect("cards");
    let question = cards
        .iter()
        .find(|card| card["kind"] == "question")
        .expect("the question stays");
    assert_eq!(question["body"]["text"], "Which title?");
    assert_eq!(question["body"]["answer"], "Kyoto Agent");
    assert!(question["body"]["choices"]
        .as_array()
        .expect("choices")
        .is_empty());
    let answer = cards
        .iter()
        .find(|card| card["kind"] == "answer")
        .expect("the answer card");
    assert_eq!(answer["body"]["text"], "Kyoto Agent");
    assert_ne!(answer["body"]["text"], "1");
    assert!(cards.iter().any(|card| card["kind"] == "result"));
    let log = fixture.client.events(&id).await;
    assert!(
        log.contains("\"kind\":\"question\""),
        "the log kept the question"
    );
    assert!(
        log.contains("\"answer\":\"Kyoto Agent\""),
        "the log kept the answer: {log}"
    );
}

/// Cancel stops the turn of one session and leaves the others alone.
#[tokio::test]
async fn cancel_stops_one_session_and_the_other_keeps_going() {
    let fixture = Fixture::new("cancel", write_then_finish()).await;
    let first = fixture.add_session("91bc").await;
    let second = fixture.add_session("3f2a").await;

    // The first session stops on a write permission; the second finishes.
    fixture.client.message(&first, "Create notes.md.").await;
    fixture.client.wait_for_status(&first, "waiting").await;
    fixture.client.message(&second, "Finish this.").await;
    fixture.client.wait_for_status(&second, "idle").await;

    // Cancel the waiting session's turn.
    let (status, response) = fixture.client.cancel(&first).await;
    assert_eq!(status, 204, "the cancel lands: {response}");

    // The second session is unaffected.
    assert_eq!(
        fixture.client.view(&second).await["status"].as_str(),
        Some("idle")
    );
}

fn ask_then_finish() -> Vec<Canned> {
    vec![
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({
                "text": "Which way?",
                "choices": ["continue", "stop"]
            }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ]
}

fn plant_skill(workspace: &std::path::Path, name: &str, description: &str) {
    let dir = workspace.join(".agents/skills").join(name);
    fs::create_dir_all(&dir).expect("the skill directory exists");
    fs::write(
        dir.join(kyotoagent::skills::SKILL_FILE),
        format!("---\nname: {name}\ndescription: {description}\n---\n\nbody\n"),
    )
    .expect("the skill is written");
}

#[tokio::test]
async fn yolo_allows_a_write_with_no_tui() {
    let fixture = Fixture::new("yolo-no-tui", write_then_finish()).await;
    let id = fixture.add_session("91bc").await;
    let body = serde_json::json!({ "yolo": true }).to_string();
    let (status, response) = fixture
        .client
        .request("POST", &format!("/v1/sessions/{id}/yolo"), Some(&body))
        .await;
    assert_eq!(status, 204, "yolo is stored: {response}");
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["yolo"], serde_json::json!(true));

    let (status, response) = fixture.client.message(&id, "Create notes.md.").await;
    assert_eq!(status, 202, "the turn starts: {response}");
    fixture.client.wait_for_status(&id, "idle").await;
    assert_eq!(
        fs::read_to_string(fixture.workspace("91bc").join("notes.md")).expect("the file reads"),
        "hello"
    );
    let log = fixture.client.events(&id).await;
    assert!(
        log.contains("permission"),
        "the permission is in the log: {log}"
    );
    assert!(log.contains("allow_once"), "yolo answers once: {log}");
}

#[tokio::test]
async fn yolo_still_waits_on_a_question_with_no_tui() {
    let fixture = Fixture::new("yolo-question-no-tui", ask_then_finish()).await;
    let id = fixture.add_session("91bc").await;
    let body = serde_json::json!({ "yolo": true }).to_string();
    let (status, _) = fixture
        .client
        .request("POST", &format!("/v1/sessions/{id}/yolo"), Some(&body))
        .await;
    assert_eq!(status, 204);
    fixture.client.message(&id, "Name the binary.").await;
    fixture.client.wait_for_status(&id, "waiting").await;
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["waiting"].as_str(), Some("question"));
    assert_eq!(rows[0]["yolo"], serde_json::json!(true));
}

fn session_meta(root: &std::path::Path, id: &str) -> kyotoagent::session::SessionMeta {
    kyotoagent::session::Session::at(&root.join(kyotoagent::server::SESSIONS_DIR).join(id))
        .meta()
        .expect("meta loads")
}

fn three_writes() -> Vec<Canned> {
    vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "one.md", "contents": "one" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "two.md", "contents": "two" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "three.md", "contents": "three" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Wrote three.", "proof": "three files." }),
        )])),
    ]
}

async fn post_yolo(client: &Client, id: &str, yolo: bool) {
    let body = serde_json::json!({ "yolo": yolo }).to_string();
    let (status, response) = client
        .request("POST", &format!("/v1/sessions/{id}/yolo"), Some(&body))
        .await;
    assert_eq!(status, 204, "yolo is stored: {response}");
}

#[tokio::test]
async fn config_yolo_starts_the_session_and_a_write_proceeds() {
    let root = test_temp_dir().join(format!(
        "kyotoagent-server-{}-config-yolo",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let fake = FakeServer::start(write_then_finish());
    let config = format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\nyolo = true\n",
        fake.base_url()
    );
    let fixture = Fixture::boot(root, fake, config).await;
    let id = fixture.add_session("91bc").await;
    assert!(session_meta(&fixture.root, &id).yolo);
    let (status, response) = fixture.client.message(&id, "Create notes.md.").await;
    assert_eq!(status, 202, "the turn starts: {response}");
    fixture.client.wait_for_status(&id, "idle").await;
    assert_eq!(
        fs::read_to_string(fixture.workspace("91bc").join("notes.md")).expect("the file reads"),
        "hello"
    );
}

#[tokio::test]
async fn project_yolo_matches_that_folder_only() {
    let root = test_temp_dir().join(format!(
        "kyotoagent-server-{}-project-yolo",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let fake = FakeServer::start(write_then_finish());
    let kyotoagent = root.join("w-kyotoagent");
    let other = root.join("w-other");
    fs::create_dir_all(&kyotoagent).expect("the project dir exists");
    fs::create_dir_all(&other).expect("the other dir exists");
    let config = format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n\n[projects.kyotoagent]\npath = \"{}\"\nyolo = true\n\n[projects.other]\npath = \"{}\"\n",
        fake.base_url(),
        kyotoagent.display(),
        other.display()
    );
    let fixture = Fixture::boot(root, fake, config).await;
    let on = fixture
        .client
        .create_session(kyotoagent.to_str().expect("a path"))
        .await;
    let off = fixture
        .client
        .create_session(other.to_str().expect("a path"))
        .await;
    assert!(session_meta(&fixture.root, &on).yolo);
    assert!(!session_meta(&fixture.root, &off).yolo);
    let (status, body) = fixture.client.request("GET", "/v1/projects", None).await;
    assert_eq!(status, 200, "{body}");
    let projects: Vec<Value> = serde_json::from_str(&body).expect("projects");
    let kyotoagent_row = projects
        .iter()
        .find(|row| row["id"] == "kyotoagent")
        .expect("kyotoagent");
    let other_row = projects
        .iter()
        .find(|row| row["id"] == "other")
        .expect("other");
    assert_eq!(kyotoagent_row["yolo"], serde_json::json!(true));
    assert!(other_row.get("yolo").is_none(), "{other_row}");
    fixture.client.message(&on, "Create notes.md.").await;
    fixture.client.wait_for_status(&on, "idle").await;
    assert_eq!(
        fs::read_to_string(kyotoagent.join("notes.md")).expect("the file reads"),
        "hello"
    );
}

#[tokio::test]
async fn an_omitted_project_yolo_uses_the_file_default() {
    let root = test_temp_dir().join(format!(
        "kyotoagent-server-{}-yolo-fallthrough",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let fake = FakeServer::start(Vec::new());
    let kyotoagent = root.join("w-kyotoagent");
    let notes = root.join("w-notes");
    fs::create_dir_all(&kyotoagent).expect("the project dir exists");
    fs::create_dir_all(&notes).expect("the notes dir exists");
    let config = format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\nyolo = true\n\n[projects.kyotoagent]\npath = \"{}\"\n\n[projects.notes]\npath = \"{}\"\nyolo = false\n",
        fake.base_url(),
        kyotoagent.display(),
        notes.display()
    );
    let fixture = Fixture::boot(root, fake, config).await;
    let inherited = fixture
        .client
        .create_session(kyotoagent.to_str().expect("a path"))
        .await;
    let overridden = fixture
        .client
        .create_session(notes.to_str().expect("a path"))
        .await;
    assert!(session_meta(&fixture.root, &inherited).yolo);
    assert!(!session_meta(&fixture.root, &overridden).yolo);
}

fn finish_turn(text: &str) -> Vec<Canned> {
    vec![Canned::Json(tool_call_reply(vec![(
        "finish",
        serde_json::json!({ "text": text, "proof": "the turn finished." }),
    )]))]
}

#[tokio::test]
async fn enhance_off_starts_a_turn_and_writes_no_enhance_event() {
    let fixture = Fixture::new("enhance-off", finish_turn("Done.")).await;
    let id = fixture.add_session("91bc").await;
    assert!(!session_meta(&fixture.root, &id).enhance);
    let (status, response) = fixture.client.message(&id, "Say done.").await;
    assert_eq!(status, 202, "the turn starts: {response}");
    fixture.client.wait_for_status(&id, "idle").await;
    let log = fixture.client.events(&id).await;
    assert!(
        log.contains("\"kind\":\"user_ask\""),
        "the turn was written: {log}"
    );
    assert!(
        !log.contains("\"kind\":\"enhance\""),
        "enhance off writes no enhance event: {log}"
    );
}

#[tokio::test]
async fn post_enhance_flips_the_live_session_and_leaves_config() {
    let fixture = Fixture::new("enhance-post", finish_turn("Done.")).await;
    let id = fixture.add_session("91bc").await;
    let path = fixture.root.join("config.toml");
    let before = fs::read(&path).expect("the config reads");
    let body = serde_json::json!({ "enhance": true }).to_string();
    let (missing, _) = fixture
        .client
        .request("POST", "/v1/sessions/missing/enhance", Some(&body))
        .await;
    assert_eq!(missing, 404);
    let (status, response) = fixture
        .client
        .request("POST", &format!("/v1/sessions/{id}/enhance"), Some(&body))
        .await;
    assert_eq!(status, 204, "enhance is stored: {response}");
    assert!(session_meta(&fixture.root, &id).enhance);
    assert_eq!(fs::read(&path).expect("the config reads"), before);
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["enhance"], serde_json::json!(true));
    let off = serde_json::json!({ "enhance": false }).to_string();
    let (status, response) = fixture
        .client
        .request("POST", &format!("/v1/sessions/{id}/enhance"), Some(&off))
        .await;
    assert_eq!(status, 204, "enhance turns off: {response}");
    assert!(!session_meta(&fixture.root, &id).enhance);
    assert_eq!(fs::read(&path).expect("the config reads"), before);
}

#[tokio::test]
async fn project_enhance_is_copied_onto_the_session() {
    let root = test_temp_dir().join(format!(
        "kyotoagent-server-{}-project-enhance",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let fake = FakeServer::start(Vec::new());
    let kyotoagent = root.join("w-kyotoagent");
    let notes = root.join("w-notes");
    let other = root.join("w-other");
    fs::create_dir_all(&kyotoagent).expect("the project dir exists");
    fs::create_dir_all(&notes).expect("the notes dir exists");
    fs::create_dir_all(&other).expect("the other dir exists");
    let config = format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\nenhance = true\n\n[projects.kyotoagent]\npath = \"{}\"\n\n[projects.notes]\npath = \"{}\"\nenhance = false\n",
        fake.base_url(),
        kyotoagent.display(),
        notes.display()
    );
    let fixture = Fixture::boot(root, fake, config).await;
    let inherited = fixture
        .client
        .create_session(kyotoagent.to_str().expect("a path"))
        .await;
    let overridden = fixture
        .client
        .create_session(notes.to_str().expect("a path"))
        .await;
    let plain = fixture
        .client
        .create_session(other.to_str().expect("a path"))
        .await;
    assert!(session_meta(&fixture.root, &inherited).enhance);
    assert!(!session_meta(&fixture.root, &overridden).enhance);
    assert!(session_meta(&fixture.root, &plain).enhance);
    let rows = fixture.client.list().await;
    let inherited_row = rows
        .iter()
        .find(|row| row["id"] == inherited)
        .expect("the inherited session");
    assert_eq!(inherited_row["enhance"], serde_json::json!(true));
    assert_eq!(inherited_row["yolo"], serde_json::json!(false));
}

async fn turn_enhance_on(fixture: &Fixture, id: &str) {
    let body = serde_json::json!({ "enhance": true }).to_string();
    let (status, response) = fixture
        .client
        .request("POST", &format!("/v1/sessions/{id}/enhance"), Some(&body))
        .await;
    assert_eq!(status, 204, "enhance turns on: {response}");
}

fn event_kinds(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            value.get("kind")?.as_str().map(str::to_string)
        })
        .collect()
}

fn user_ask_texts(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            if value.get("kind")?.as_str() != Some("user_ask") {
                return None;
            }
            value.get("body")?.get("text")?.as_str().map(str::to_string)
        })
        .collect()
}

fn enhance_cards(view: &Value) -> Vec<&Value> {
    view["cards"]
        .as_array()
        .expect("cards")
        .iter()
        .filter(|card| card["kind"] == "enhance")
        .collect()
}

async fn wait_holding(fixture: &Fixture) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.chat_is_holding() {
        assert!(Instant::now() < deadline, "the rewrite was not held");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn quiet_bodies(bodies: &[String]) -> Vec<Value> {
    bodies
        .iter()
        .filter_map(|body| {
            let value: Value = serde_json::from_str(body).ok()?;
            (value.get("stream") == Some(&Value::Bool(false))).then_some(value)
        })
        .collect()
}

#[tokio::test]
async fn enhance_on_writes_a_card_and_keeps_the_draft_out_of_the_transcript() {
    let fixture = Fixture::titled(
        "enhance-gate",
        vec![
            Canned::Json(prose("Do the thing carefully.")),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "text": "Done.", "proof": "the turn finished." }),
            )])),
        ],
        "title-fast",
    )
    .await;
    let id = fixture.add_session("91bc").await;
    turn_enhance_on(&fixture, &id).await;
    let workspace = fixture.workspace("91bc");
    fs::write(workspace.join("AGENTS.md"), "No comments.\n").expect("agents");
    let long = "n".repeat(600);
    let seeded = [
        serde_json::json!({
            "id": "e1", "at": "2026-09-29T00:00:00.000Z", "turnId": "t1",
            "kind": "user_ask", "body": { "text": "one" }
        }),
        serde_json::json!({
            "id": "e2", "at": "2026-09-29T00:00:00.000Z", "turnId": "t1",
            "kind": "user_ask", "body": { "text": "two" }
        }),
        serde_json::json!({
            "id": "e3", "at": "2026-09-29T00:00:00.000Z", "turnId": "t1",
            "kind": "user_ask", "body": { "text": "three" }
        }),
        serde_json::json!({
            "id": "e4", "at": "2026-09-29T00:00:00.000Z", "turnId": "t1",
            "kind": "user_ask", "body": { "text": long }
        }),
        serde_json::json!({
            "id": "e5", "at": "2026-09-29T00:00:00.000Z", "turnId": "t1",
            "kind": "tool_result",
            "body": { "tool": "read_file", "output": "SECRET_TOOL_OUTPUT" }
        }),
    ];
    let log_path = fixture
        .root
        .join(kyotoagent::server::SESSIONS_DIR)
        .join(&id)
        .join("events.jsonl");
    let lines = seeded
        .iter()
        .map(|event| event.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&log_path, format!("{lines}\n")).expect("seed");
    let (status, response) = fixture.client.message(&id, "ship it").await;
    assert_eq!(status, 200, "the gate answers: {response}");
    let started: Value = serde_json::from_str(&response).expect("json");
    assert_eq!(started["state"], "enhancing");
    let request_id = started["id"].as_str().expect("request id").to_string();
    let view = fixture.client.wait_for_status(&id, "waiting").await;
    let rows = fixture.client.list().await;
    let row = rows.iter().find(|row| row["id"] == id).expect("row");
    assert_eq!(row["waiting"], "enhance");
    let cards = enhance_cards(&view);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["body"]["text"], "Do the thing carefully.");
    assert_eq!(cards[0]["body"]["source"], "ship it");
    assert_eq!(cards[0]["body"]["model"], "title-fast");
    assert!(cards[0]["body"].get("error").is_none());
    let card_id = cards[0]["body"]["eventId"].as_str().expect("event id");
    assert_ne!(card_id, request_id);
    let log = fixture.client.events(&id).await;
    let kinds = event_kinds(&log);
    assert!(kinds.contains(&"enhance_request".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"enhance".to_string()), "{kinds:?}");
    assert_eq!(user_ask_texts(&log).len(), 4, "{log}");
    let projected = kyotoagent::compact::projected_messages(
        "sys",
        &kyotoagent::session::Session::at(
            &fixture
                .root
                .join(kyotoagent::server::SESSIONS_DIR)
                .join(&id),
        )
        .events()
        .expect("events"),
        workspace.to_str().expect("path"),
    );
    let blob = serde_json::to_string(&projected).expect("projected");
    assert!(!blob.contains("ship it"), "{blob}");
    assert!(!blob.contains("Do the thing carefully."), "{blob}");
    let quiet = quiet_bodies(&fixture._fake.bodies());
    assert_eq!(quiet.len(), 1, "{quiet:?}");
    let quiet_blob = quiet[0].to_string();
    assert_eq!(quiet[0]["model"], "title-fast");
    assert!(quiet[0].get("tools").is_none(), "{quiet_blob}");
    assert!(quiet_blob.contains("Workspace: "), "{quiet_blob}");
    assert!(
        quiet_blob.contains(&workspace.to_string_lossy().to_string()),
        "{quiet_blob}"
    );
    assert!(quiet_blob.contains("No comments."), "{quiet_blob}");
    assert!(
        quiet_blob.contains("Rewrite the request into the prompt"),
        "{quiet_blob}"
    );
    assert!(quiet_blob.contains("two"), "{quiet_blob}");
    assert!(!quiet_blob.contains("\"one\""), "{quiet_blob}");
    assert!(!quiet_blob.contains("SECRET_TOOL_OUTPUT"), "{quiet_blob}");
    assert!(quiet_blob.matches('n').count() >= 500, "{quiet_blob}");
    let (status, response) = fixture.client.message(&id, "again").await;
    assert_eq!(status, 409, "a second message is refused: {response}");
    let (status, response) = fixture.client.answer(&id, card_id, "allow_once").await;
    assert_eq!(
        status, 400,
        "an enhance card is not a permission: {response}"
    );
    let (status, response) = fixture.client.answer_text(&id, card_id, "use", "").await;
    assert_eq!(status, 204, "use accepts the draft: {response}");
    fixture.client.wait_for_status(&id, "idle").await;
    let log = fixture.client.events(&id).await;
    assert_eq!(
        user_ask_texts(&log).last().map(String::as_str),
        Some("Do the thing carefully."),
        "{log}"
    );
    let projected = kyotoagent::compact::projected_messages(
        "sys",
        &kyotoagent::session::Session::at(
            &fixture
                .root
                .join(kyotoagent::server::SESSIONS_DIR)
                .join(&id),
        )
        .events()
        .expect("events"),
        workspace.to_str().expect("path"),
    );
    let blob = serde_json::to_string(&projected).expect("projected");
    assert_eq!(blob.matches("Do the thing carefully.").count(), 1, "{blob}");
    assert!(!blob.contains("ship it"), "{blob}");
}

#[tokio::test]
async fn enhance_discard_cancel_and_a_second_message_do_not_start_a_turn() {
    let fixture = Fixture::new(
        "enhance-discard",
        vec![Canned::Json(prose("Leave this draft."))],
    )
    .await;
    let id = fixture.add_session("91bc").await;
    turn_enhance_on(&fixture, &id).await;
    fixture.pause_before(0);
    let (status, _) = fixture.client.message(&id, "ship it").await;
    assert_eq!(status, 200);
    wait_holding(&fixture).await;
    let (status, _) = fixture.client.cancel(&id).await;
    assert_eq!(status, 204);
    fixture.release_chat();
    fixture.client.wait_for_status(&id, "idle").await;
    let log = fixture.client.events(&id).await;
    assert_eq!(
        event_kinds(&log),
        vec!["enhance_request".to_string()],
        "{log}"
    );
    assert!(user_ask_texts(&log).is_empty(), "{log}");

    let (status, _) = fixture.client.message(&id, "ship it").await;
    assert_eq!(status, 200);
    let view = fixture.client.wait_for_status(&id, "waiting").await;
    let card_id = enhance_cards(&view)[0]["body"]["eventId"]
        .as_str()
        .expect("event id")
        .to_string();
    let (again, _) = fixture.client.message(&id, "nope").await;
    assert_eq!(again, 409);
    let (status, response) = fixture.client.answer(&id, &card_id, "discard").await;
    assert_eq!(status, 204, "discard settles: {response}");
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(200) {
        let view = fixture.client.view(&id).await;
        assert_eq!(view["status"], "idle");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let log = fixture.client.events(&id).await;
    assert!(user_ask_texts(&log).is_empty(), "{log}");
    let bodies = fixture._fake.bodies();
    assert_eq!(quiet_bodies(&bodies).len(), 2, "{bodies:?}");
    assert_eq!(bodies.len(), 2, "{bodies:?}");
}

#[tokio::test]
async fn enhance_revise_rejects_empty_text_and_retry_replaces_a_failed_card() {
    let fixture = Fixture::new(
        "enhance-retry",
        vec![
            Canned::Status(500, "down".into()),
            Canned::Json(prose("   ")),
            Canned::Json(prose("A better prompt.")),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "text": "Done.", "proof": "the turn finished." }),
            )])),
        ],
    )
    .await;
    let id = fixture.add_session("91bc").await;
    turn_enhance_on(&fixture, &id).await;
    let (status, _) = fixture.client.message(&id, "ship it").await;
    assert_eq!(status, 200);
    let view = fixture.client.wait_for_status(&id, "waiting").await;
    let card_id = enhance_cards(&view)[0]["body"]["eventId"]
        .as_str()
        .expect("event id")
        .to_string();
    assert!(enhance_cards(&view)[0]["body"]["error"]
        .as_str()
        .unwrap_or("")
        .contains("500"));
    let (status, response) = fixture.client.answer(&id, &card_id, "use").await;
    assert_eq!(status, 400, "use needs a draft: {response}");
    assert!(response.contains("use needs a draft"), "{response}");
    let (status, response) = fixture
        .client
        .answer_text(&id, &card_id, "revise", "   ")
        .await;
    assert_eq!(status, 400, "revise needs text: {response}");
    assert!(response.contains("revise needs text"), "{response}");
    assert_eq!(fixture.client.view(&id).await["status"], "waiting");
    let (status, _) = fixture.client.answer(&id, &card_id, "retry").await;
    assert_eq!(status, 204);
    let view = wait_for_error(&fixture, &id, "the rewrite was empty").await;
    assert_eq!(enhance_cards(&view).len(), 1);
    let card_id = enhance_cards(&view)[0]["body"]["eventId"]
        .as_str()
        .expect("event id")
        .to_string();
    let (status, _) = fixture.client.answer(&id, &card_id, "retry").await;
    assert_eq!(status, 204);
    let deadline = Instant::now() + Duration::from_secs(10);
    let card_id = loop {
        let view = fixture.client.view(&id).await;
        let cards = enhance_cards(&view);
        assert_eq!(cards.len(), 1);
        if cards[0]["body"]["text"] == "A better prompt." {
            assert!(cards[0]["body"].get("error").is_none());
            break cards[0]["body"]["eventId"]
                .as_str()
                .expect("event id")
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "the retry did not replace the card"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let log = fixture.client.events(&id).await;
    assert!(user_ask_texts(&log).is_empty(), "{log}");
    assert!(
        event_kinds(&log)
            .iter()
            .filter(|kind| *kind == "enhance_request")
            .count()
            == 1,
        "{log}"
    );
    let (status, response) = fixture
        .client
        .answer_text(&id, &card_id, "revise", "the client wording")
        .await;
    assert_eq!(status, 204, "revise starts the worker: {response}");
    fixture.client.wait_for_status(&id, "idle").await;
    let log = fixture.client.events(&id).await;
    assert_eq!(
        user_ask_texts(&log),
        vec!["the client wording".to_string()],
        "{log}"
    );
    assert_eq!(quiet_bodies(&fixture._fake.bodies()).len(), 3);
}

#[tokio::test]
async fn enhance_use_while_a_retry_is_running_starts_the_open_draft() {
    let fixture = Fixture::new(
        "enhance-use-retry",
        vec![
            Canned::Json(prose("Use this draft.")),
            Canned::Json(prose("A later draft that should not land.")),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "text": "Done.", "proof": "the turn finished." }),
            )])),
        ],
    )
    .await;
    let id = fixture.add_session("91bc").await;
    turn_enhance_on(&fixture, &id).await;
    let (status, _) = fixture.client.message(&id, "ship it").await;
    assert_eq!(status, 200);
    let view = fixture.client.wait_for_status(&id, "waiting").await;
    let card_id = enhance_cards(&view)[0]["body"]["eventId"]
        .as_str()
        .expect("event id")
        .to_string();
    fixture.pause_before(1);
    let (status, _) = fixture.client.answer(&id, &card_id, "retry").await;
    assert_eq!(status, 204);
    wait_holding(&fixture).await;
    let (status, response) = fixture.client.answer(&id, &card_id, "use").await;
    assert_eq!(status, 204, "use starts the worker: {response}");
    let log = fixture.client.events(&id).await;
    assert_eq!(
        user_ask_texts(&log),
        vec!["Use this draft.".to_string()],
        "{log}"
    );
    assert!(enhance_cards(&fixture.client.view(&id).await).is_empty());
    fixture.release_chat();
    fixture.client.wait_for_status(&id, "idle").await;
    let log = fixture.client.events(&id).await;
    assert_eq!(
        user_ask_texts(&log),
        vec!["Use this draft.".to_string()],
        "{log}"
    );
    assert!(
        !log.contains("A later draft that should not land."),
        "{log}"
    );
}

async fn wait_for_error(fixture: &Fixture, id: &str, error: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = fixture.client.view(id).await;
        if enhance_cards(&view)
            .first()
            .and_then(|card| card["body"]["error"].as_str())
            .is_some_and(|text| text.contains(error))
        {
            return view;
        }
        assert!(Instant::now() < deadline, "the card did not show {error}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn enhance_skill_and_an_explicit_off_skip_the_gate() {
    let fixture = Fixture::new(
        "enhance-skip",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "text": "Done.", "proof": "the turn finished." }),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "text": "Done.", "proof": "the turn finished." }),
            )])),
        ],
    )
    .await;
    let id = fixture.add_session("91bc").await;
    let skill = fixture
        .workspace("91bc")
        .join(".agents")
        .join("skills")
        .join("ship");
    fs::create_dir_all(&skill).expect("skill dir");
    fs::write(skill.join("SKILL.md"), "Ship the change.\n").expect("skill");
    turn_enhance_on(&fixture, &id).await;
    let (status, response) = fixture.client.message(&id, "/ship the notes").await;
    assert_eq!(status, 202, "a skill skips the gate: {response}");
    fixture.client.wait_for_status(&id, "idle").await;
    let log = fixture.client.events(&id).await;
    assert!(
        user_ask_texts(&log)
            .iter()
            .any(|text| text.contains("the notes")
                || text == "/ship the notes"
                || text == "the notes"),
        "{log}"
    );
    assert!(
        !event_kinds(&log)
            .iter()
            .any(|kind| kind == "enhance_request"),
        "{log}"
    );
    let (status, response) = fixture
        .client
        .message_body(
            &id,
            serde_json::json!({ "text": "plain", "enhance": false }),
        )
        .await;
    assert_eq!(status, 202, "enhance false skips the gate: {response}");
    fixture.client.wait_for_status(&id, "idle").await;
    let log = fixture.client.events(&id).await;
    assert_eq!(
        event_kinds(&log)
            .iter()
            .filter(|kind| *kind == "enhance_request")
            .count(),
        0,
        "{log}"
    );
    assert!(
        user_ask_texts(&log).iter().any(|text| text == "plain"),
        "{log}"
    );
}

#[tokio::test]
async fn editing_config_leaves_an_existing_session() {
    let fixture = Fixture::new("yolo-edit", Vec::new()).await;
    let old = fixture.add_session("old").await;
    assert!(!session_meta(&fixture.root, &old).yolo);
    let path = fixture.root.join("config.toml");
    let text = fs::read_to_string(&path).expect("the config reads");
    fs::write(&path, format!("{text}yolo = true\n")).expect("the config writes");
    assert!(!session_meta(&fixture.root, &old).yolo);
    let fresh = fixture.add_session("fresh").await;
    assert!(session_meta(&fixture.root, &fresh).yolo);
    assert!(!session_meta(&fixture.root, &old).yolo);
}

#[tokio::test]
async fn closeout_off_hides_the_session_and_leaves_the_file() {
    let fixture = Fixture::new("closeout-off", Vec::new()).await;
    let id = fixture.add_session("91bc").await;
    assert!(session_meta(&fixture.root, &id).show_closeout);
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["showCloseout"], serde_json::json!(true));
    let workspace = fixture.workspace("91bc");
    fs::create_dir_all(workspace.join(".kyotoagent")).expect("the closeout directory exists");
    let yaml = workspace.join(".kyotoagent").join("closeout.yaml");
    let yaml_text = "version: 1\nitems:\n  - id: test\n    kind: command\n    run: echo out\n    hint: Fix the failing test\n";
    fs::write(&yaml, yaml_text).expect("the file writes");
    let body = serde_json::json!({ "show": false }).to_string();
    let (status, response) = fixture
        .client
        .request("POST", &format!("/v1/sessions/{id}/closeout"), Some(&body))
        .await;
    assert_eq!(status, 204, "{response}");
    assert!(!session_meta(&fixture.root, &id).show_closeout);
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["showCloseout"], serde_json::json!(false));
    assert_eq!(
        fs::read_to_string(&yaml).expect("the file stays"),
        yaml_text
    );
    let path = fixture.root.join("config.toml");
    let text = fs::read_to_string(&path).expect("the config reads");
    fs::write(&path, format!("{text}show_closeout = true\n")).expect("the config writes");
    assert!(!session_meta(&fixture.root, &id).show_closeout);
    let view = fixture.client.view(&id).await;
    assert!(view
        .get("closeout")
        .and_then(|value| value.as_array())
        .is_none_or(|rows| rows.is_empty()));
}

#[tokio::test]
async fn project_show_closeout_is_copied_onto_a_new_session() {
    let root = test_temp_dir().join(format!(
        "kyotoagent-server-{}-show-closeout",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let fake = FakeServer::start(Vec::new());
    let hidden = root.join("w-hidden");
    let shown = root.join("w-shown");
    fs::create_dir_all(&hidden).expect("the hidden dir exists");
    fs::create_dir_all(&shown).expect("the shown dir exists");
    let config = format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\nshow_closeout = false\n\n[projects.shown]\npath = \"{}\"\nshow_closeout = true\n\n[projects.hidden]\npath = \"{}\"\n",
        fake.base_url(),
        shown.display(),
        hidden.display()
    );
    let fixture = Fixture::boot(root, fake, config).await;
    let off = fixture
        .client
        .create_session(hidden.to_str().expect("a path"))
        .await;
    let on = fixture
        .client
        .create_session(shown.to_str().expect("a path"))
        .await;
    assert!(!session_meta(&fixture.root, &off).show_closeout);
    assert!(session_meta(&fixture.root, &on).show_closeout);
}

#[tokio::test]
async fn yolo_on_mid_wait_allows_the_next_write_and_off_stops_the_third() {
    let fixture = Fixture::new("yolo-mid", three_writes()).await;
    fixture.pause_before(2);
    let id = fixture.add_session("91bc").await;
    fixture.client.message(&id, "Write three files.").await;
    fixture.client.wait_for_status(&id, "waiting").await;
    assert!(!fixture.workspace("91bc").join("one.md").exists());
    post_yolo(&fixture.client, &id, true).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.chat_is_holding() {
        assert!(
            Instant::now() < deadline,
            "the third completion did not pause"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        fs::read_to_string(fixture.workspace("91bc").join("one.md")).expect("one reads"),
        "one"
    );
    assert_eq!(
        fs::read_to_string(fixture.workspace("91bc").join("two.md")).expect("two reads"),
        "two"
    );
    post_yolo(&fixture.client, &id, false).await;
    let remembered = session_meta(&fixture.root, &id).allow.write_paths.clone();
    assert!(remembered.is_empty(), "{remembered:?}");
    fixture.release_chat();
    fixture.client.wait_for_status(&id, "waiting").await;
    assert!(!fixture.workspace("91bc").join("three.md").exists());
    assert!(!session_meta(&fixture.root, &id).yolo);
    let log = fixture.client.events(&id).await;
    assert_eq!(log.matches("allow_once").count(), 2, "{log}");
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["yolo"], serde_json::json!(false));
}

#[tokio::test]
async fn turning_yolo_off_keeps_the_remembered_path() {
    let fixture = Fixture::new("yolo-allow", write_then_finish()).await;
    let id = fixture.add_session("91bc").await;
    fixture.client.message(&id, "Create notes.md.").await;
    fixture.client.wait_for_status(&id, "waiting").await;
    let card = fixture.client.open_permission_id(&id).await;
    let (status, response) = fixture.client.answer(&id, &card, "allow_session").await;
    assert_eq!(status, 204, "{response}");
    fixture.client.wait_for_status(&id, "idle").await;
    let before = session_meta(&fixture.root, &id).allow.write_paths.clone();
    assert_eq!(before.len(), 1, "{before:?}");
    post_yolo(&fixture.client, &id, true).await;
    post_yolo(&fixture.client, &id, false).await;
    let after = session_meta(&fixture.root, &id);
    assert_eq!(after.allow.write_paths, before);
    assert!(!after.yolo);
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["allow"]["writePaths"], serde_json::json!(before));
    let view = fixture.client.view(&id).await;
    assert_eq!(view["allow"]["writePaths"], serde_json::json!(before));
}

#[tokio::test]
async fn models_come_from_serve_and_a_pick_writes_the_server_root() {
    let fixture = Fixture::new("models-post", write_then_finish()).await;
    fixture._fake.set_models(
        &serde_json::json!({
            "data": [
                { "id": "test/model", "context_length": 128000 },
                {
                    "id": "grok-4.6",
                    "context_length": 256000,
                    "aliases": ["grok"],
                    "reasoning_efforts": ["low", "medium", "high", "xhigh"]
                }
            ]
        })
        .to_string(),
    );
    let (status, body) = fixture.client.request("GET", "/v1/models", None).await;
    assert_eq!(status, 200, "the catalog reads: {body}");
    let rows: Value = serde_json::from_str(&body).expect("the catalog is JSON");
    let ids: Vec<&str> = rows
        .as_array()
        .expect("an array")
        .iter()
        .map(|row| row["id"].as_str().expect("an id"))
        .collect();
    assert!(ids.contains(&"test/model"), "{ids:?}");
    assert!(ids.contains(&"grok-4.6"), "{ids:?}");
    let grok = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "grok-4.6")
        .expect("grok is listed");
    assert_eq!(grok["context_length"], 256000);
    assert!(!grok["reasoning_efforts"].as_array().unwrap().is_empty());

    let id = fixture.add_session("91bc").await;
    let body = serde_json::json!({ "model": "grok-4.6", "effort": "high" }).to_string();
    let (status, response) = fixture
        .client
        .request("POST", &format!("/v1/sessions/{id}/model"), Some(&body))
        .await;
    assert_eq!(status, 204, "the pick lands: {response}");
    let config = Config::load(&fixture.root.join("config.toml")).expect("config loads");
    assert_eq!(config.model, "grok-4.6");
    assert_eq!(config.effort.as_deref(), Some("high"));
    let meta = kyotoagent::session::Session::at(
        &fixture
            .root
            .join(kyotoagent::server::SESSIONS_DIR)
            .join(&id),
    )
    .meta()
    .expect("meta loads");
    assert_eq!(meta.model, "grok-4.6");
    assert_eq!(meta.effort.as_deref(), Some("high"));
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["model"].as_str(), Some("grok-4.6"));
    assert_eq!(rows[0]["effort"].as_str(), Some("high"));
}

#[tokio::test]
async fn a_session_model_pick_leaves_defaults_and_siblings_unchanged() {
    let fixture = Fixture::new("session-model", finish_turn("Done.")).await;
    fixture
        ._fake
        .set_models(r#"{"data":[{"id":"test/model"},{"id":"session/model"}]}"#);
    let first = fixture.add_session("91bc").await;
    let second = fixture.add_session("3f2a").await;
    let path = fixture.root.join("config.toml");
    let before = fs::read(&path).unwrap();
    let body = serde_json::json!({"model":"session/model","effort":"high"}).to_string();
    let (status, response) = fixture
        .client
        .request(
            "POST",
            &format!("/v1/sessions/{first}/model/session"),
            Some(&body),
        )
        .await;
    assert_eq!(status, 204, "{response}");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fixture.client.message(&first, "Finish.").await.0, 202);
    fixture.client.wait_for_status(&first, "idle").await;
    assert_eq!(fixture.client.message(&second, "Finish.").await.0, 202);
    fixture.client.wait_for_status(&second, "idle").await;
    let models: Vec<_> = fixture
        ._fake
        .bodies()
        .iter()
        .filter_map(|body| request_model(body))
        .collect();
    assert_eq!(models, vec!["session/model", "test/model"]);
    let rows = fixture.client.list().await;
    assert_eq!(
        rows.iter().find(|row| row["id"] == first).unwrap()["model"],
        "session/model"
    );
    assert_eq!(
        rows.iter().find(|row| row["id"] == second).unwrap()["model"],
        "test/model"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let body = serde_json::json!({"model":"test/model","effort":null}).to_string();
    let (status, response) = fixture
        .client
        .request("POST", &format!("/v1/sessions/{first}/model"), Some(&body))
        .await;
    assert_eq!(status, 204, "{response}");
    let session = kyotoagent::session::Session::at(
        &fixture
            .root
            .join(kyotoagent::server::SESSIONS_DIR)
            .join(&first),
    );
    assert!(session.meta().unwrap().model_override.is_none());
    Config::set_model(&path, "later/default").unwrap();
    assert_eq!(fixture.client.message(&first, "Finish again.").await.0, 202);
    fixture.client.wait_for_status(&first, "idle").await;
    assert_eq!(
        request_model(fixture._fake.bodies().last().unwrap()).as_deref(),
        Some("later/default")
    );
}

#[tokio::test]
async fn the_view_lists_workspace_skills() {
    let fixture = Fixture::new("view-skills", write_then_finish()).await;
    let id = fixture.add_session("91bc").await;
    plant_skill(
        &fixture.workspace("91bc"),
        "workspace-only-skill",
        "A skill that lives in the session workspace",
    );
    let view = fixture.client.view(&id).await;
    let skills = view["skills"].as_array().expect("skills");
    let names: Vec<&str> = skills
        .iter()
        .map(|skill| skill["name"].as_str().expect("a name"))
        .collect();
    assert!(
        names.contains(&"workspace-only-skill"),
        "the view lists the workspace skill: {names:?}"
    );
}

#[tokio::test]
async fn an_idle_session_fills_its_pull_url_from_gh_on_serve() {
    let fixture = Fixture::new("pull-fill", write_then_finish()).await;
    let id = fixture.add_session("91bc").await;
    let workspace = fixture.workspace("91bc");
    fs::create_dir_all(workspace.join(".git")).expect("the workspace is a git repo");
    let bin = fixture.root.join("bin");
    fs::create_dir_all(&bin).expect("the bin exists");
    let gh = bin.join("gh");
    let url = format!("{}/pmdroid/kyotoagent/pull/42", github_origin());
    fs::write(
        &gh,
        format!("#!/bin/sh\necho '{{\"url\":\"{url}\",\"baseRefName\":\"release\"}}'\n"),
    )
    .expect("gh is written");
    let mut perm = fs::metadata(&gh).expect("gh metadata").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&gh, perm).expect("gh is executable");
    fixture.runner.set_gh_program(gh);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rows = fixture.client.list().await;
        if rows[0]["pullUrl"].as_str() == Some(url.as_str()) {
            assert_eq!(
                session_meta(&fixture.root, &id).base_ref_name.as_deref(),
                Some("release")
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "serve did not fill the pull url: {}",
            rows[0]
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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

fn session_count(root: &std::path::Path) -> usize {
    fs::read_dir(root.join("sessions"))
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

#[tokio::test]
async fn a_worktree_session_is_a_linked_git_worktree() {
    let fixture = Fixture::new("worktree-git", Vec::new()).await;
    let repo = fixture.root.join("repo");
    fs::create_dir_all(&repo).expect("the repo exists");
    git_init(&repo);
    let marker = repo.join("kept.txt");
    fs::write(&marker, "stay").expect("the original file writes");
    let head = git_out(&repo, &["rev-parse", "HEAD"]);
    let body = serde_json::json!({
        "workspace": repo.to_str().expect("a path"),
        "worktree": true
    })
    .to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&body))
        .await;
    assert_eq!(status, 201, "{response}");
    let json: Value = serde_json::from_str(&response).expect("the reply is JSON");
    let id = json["id"].as_str().expect("an id");
    let dest = fixture.root.join("worktrees").join(format!("repo-{id}"));
    assert_eq!(json["workspace"].as_str(), dest.to_str());
    assert!(dest.is_dir(), "the worktree exists");
    assert_eq!(
        git_out(&dest, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        format!("kyotoagent/{id}")
    );
    assert_eq!(git_out(&dest, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git_out(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main"
    );
    assert_eq!(
        fs::read_to_string(&marker).expect("the original file stays"),
        "stay"
    );
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["workspace"].as_str(), dest.to_str());
    assert_eq!(rows[0]["worktree"], true);
    assert_eq!(rows[0]["isolation"], "worktree");
    let meta: Value = serde_json::from_str(&meta_text(&fixture.root, id)).expect("meta");
    assert_eq!(meta["isolation"], "worktree");
    assert_eq!(meta["requestedWorkspace"], repo.to_str().expect("a path"));
}

#[tokio::test]
async fn deleting_a_user_worktree_runs_git_in_the_requested_workspace() {
    let fixture = Fixture::new("worktree-delete", Vec::new()).await;
    let repo = fixture.root.join("repo");
    fs::create_dir_all(&repo).expect("the repo exists");
    git_init(&repo);
    let body = serde_json::json!({
        "workspace": repo.to_str().expect("a path"),
        "worktree": true
    })
    .to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&body))
        .await;
    assert_eq!(status, 201, "{response}");
    let json: Value = serde_json::from_str(&response).expect("the reply is JSON");
    let id = json["id"].as_str().expect("an id").to_string();
    let dest = fixture.root.join("worktrees").join(format!("repo-{id}"));
    assert!(dest.is_dir());
    assert!(
        git_out(&repo, &["worktree", "list"]).contains(dest.to_str().expect("a path")),
        "the repo lists the new worktree"
    );
    let (status, response) = fixture
        .client
        .request(
            "DELETE",
            &format!("/v1/sessions/{id}?delete_workspace=true"),
            None,
        )
        .await;
    assert_eq!(status, 204, "{response}");
    assert!(!dest.exists());
    assert!(!fixture.root.join("sessions").join(&id).exists());
    let listed = git_out(&repo, &["worktree", "list"]);
    assert!(
        !listed.contains(dest.to_str().expect("a path")),
        "git worktree remove left the checkout registered: {listed}"
    );
}

#[tokio::test]
async fn deleting_an_old_workspace_session_keeps_both_directories() {
    let fixture = Fixture::new("worktree-old", Vec::new()).await;
    let plain = fixture.root.join("plain");
    fs::create_dir_all(&plain).expect("the plain workspace exists");
    let kept_id = fixture
        .client
        .create_session(plain.to_str().expect("a path"))
        .await;
    let old_id = fixture
        .client
        .create_session(plain.to_str().expect("a path"))
        .await;
    let dest = fixture.root.join("worktrees").join(format!("old-{old_id}"));
    let kept = fixture.root.join("worktrees").join("kept");
    fs::create_dir_all(&dest).expect("the old worktree exists");
    fs::create_dir_all(&kept).expect("the other worktree exists");
    fs::write(dest.join("note"), "gone").expect("a file in the old worktree");
    fs::write(kept.join("note"), "stay").expect("a file in the other worktree");
    kyotoagent::session::Session::at(&fixture.root.join("sessions").join(&old_id))
        .update(|meta| {
            meta.workspace = dest.display().to_string();
            meta.isolation = None;
            true
        })
        .expect("meta");
    let rows = fixture.client.list().await;
    let old = rows
        .iter()
        .find(|row| row["id"] == old_id)
        .expect("the old session is listed");
    assert_eq!(old["worktree"], true);
    assert!(old.get("isolation").is_none());
    let kept_row = rows
        .iter()
        .find(|row| row["id"] == kept_id)
        .expect("the plain session is listed");
    assert!(kept_row.get("worktree").is_none());
    let (status, response) = fixture
        .client
        .request("DELETE", &format!("/v1/sessions/{old_id}"), None)
        .await;
    assert_eq!(status, 204, "{response}");
    assert!(dest.exists());
    assert_eq!(fs::read_to_string(kept.join("note")).expect("kept"), "stay");
    let (status, response) = fixture
        .client
        .request("DELETE", &format!("/v1/sessions/{kept_id}"), None)
        .await;
    assert_eq!(status, 204, "{response}");
    assert_eq!(fs::read_to_string(kept.join("note")).expect("kept"), "stay");
}

#[tokio::test]
async fn removing_a_user_worktree_keeps_the_session() {
    let fixture = Fixture::new("worktree-keep", Vec::new()).await;
    let repo = fixture.root.join("repo");
    fs::create_dir_all(&repo).expect("the repo exists");
    git_init(&repo);
    let body = serde_json::json!({
        "workspace": repo.to_str().expect("a path"),
        "worktree": true
    })
    .to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&body))
        .await;
    assert_eq!(status, 201, "{response}");
    let json: Value = serde_json::from_str(&response).expect("the reply is JSON");
    let id = json["id"].as_str().expect("an id");
    let dest = fixture.root.join("worktrees").join(format!("repo-{id}"));
    let (status, response) = fixture
        .client
        .request("DELETE", &format!("/v1/sessions/{id}/worktree"), None)
        .await;
    assert_eq!(status, 204, "{response}");
    assert!(!dest.exists());
    let rows = fixture.client.list().await;
    let row = rows
        .iter()
        .find(|row| row["id"] == id)
        .expect("the session stays listed");
    assert!(row.get("isolation").is_none());
    assert!(row.get("worktree").is_none());
    assert_eq!(row["workspace"], repo.to_str().expect("a path"));
    let listed = git_out(&repo, &["worktree", "list"]);
    assert!(!listed.contains(dest.to_str().expect("a path")), "{listed}");
}

#[tokio::test]
async fn a_worktree_session_needs_a_git_repository() {
    let fixture = Fixture::new("worktree-plain", Vec::new()).await;
    let dir = fixture.root.join("plain");
    fs::create_dir_all(&dir).expect("the directory exists");
    let body = serde_json::json!({
        "workspace": dir.to_str().expect("a path"),
        "worktree": true
    })
    .to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&body))
        .await;
    assert_eq!(status, 400, "{response}");
    assert!(
        response.contains("the workspace is not a git repository"),
        "{response}"
    );
    assert!(fixture.client.list().await.is_empty());
    assert_eq!(session_count(&fixture.root), 0);
}

#[tokio::test]
async fn a_session_without_a_worktree_uses_the_directory() {
    let fixture = Fixture::new("worktree-false", Vec::new()).await;
    let dir = fixture.root.join("here");
    fs::create_dir_all(&dir).expect("the directory exists");
    let path = dir.to_str().expect("a path");
    let body = serde_json::json!({
        "workspace": path,
        "worktree": false
    })
    .to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&body))
        .await;
    assert_eq!(status, 201, "{response}");
    let json: Value = serde_json::from_str(&response).expect("the reply is JSON");
    assert_eq!(json["workspace"].as_str(), Some(path));
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["workspace"].as_str(), Some(path));
}

#[tokio::test]
async fn a_failed_worktree_add_creates_no_session() {
    let fixture = Fixture::new("worktree-empty", Vec::new()).await;
    let repo = fixture.root.join("empty");
    fs::create_dir_all(&repo).expect("the repo exists");
    let output = std::process::Command::new("git")
        .current_dir(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["init", "-b", "main"])
        .output()
        .expect("git init runs");
    assert!(output.status.success(), "git init succeeds");
    let body = serde_json::json!({
        "workspace": repo.to_str().expect("a path"),
        "worktree": true
    })
    .to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&body))
        .await;
    assert_eq!(status, 400, "{response}");
    assert!(
        !response.contains("the workspace is not a git repository"),
        "{response}"
    );
    assert!(fixture.client.list().await.is_empty());
    assert_eq!(session_count(&fixture.root), 0);
}

fn append_project(root: &std::path::Path, id: &str, path: &std::path::Path) {
    let file = root.join("config.toml");
    let mut text = fs::read_to_string(&file).expect("the config reads");
    text.push_str(&format!(
        "\n[projects.{id}]\npath = \"{}\"\n",
        path.display()
    ));
    fs::write(file, text).expect("the config writes");
}

#[tokio::test]
async fn sessions_under_a_project_path_name_that_project() {
    let fixture = Fixture::new("project-groups", Vec::new()).await;
    let kyotoagent = fixture.root.join("work").join("kyotoagent");
    let nested = kyotoagent.join("src");
    let notes = fixture.root.join("notes");
    fs::create_dir_all(&nested).expect("the project tree exists");
    fs::create_dir_all(&notes).expect("notes exists");
    append_project(&fixture.root, "kyotoagent", &kyotoagent);
    fixture
        .client
        .create_session(notes.to_str().expect("a path"))
        .await;
    fixture
        .client
        .create_session(kyotoagent.to_str().expect("a path"))
        .await;
    fixture
        .client
        .create_session(nested.to_str().expect("a path"))
        .await;
    let rows = fixture.client.list().await;
    assert_eq!(rows.len(), 3, "{rows:?}");
    let grouped = rows
        .iter()
        .filter(|row| row["project"].as_str() == Some("kyotoagent"))
        .count();
    assert_eq!(grouped, 2, "{rows:?}");
    let other = rows
        .iter()
        .find(|row| row["workspace"].as_str() == notes.to_str())
        .expect("the notes session");
    assert!(other.get("project").is_none(), "{other}");
    let (status, body) = fixture.client.request("GET", "/v1/projects", None).await;
    assert_eq!(status, 200, "{body}");
    let projects: Vec<Value> = serde_json::from_str(&body).expect("projects");
    assert_eq!(projects[0]["id"].as_str(), Some("kyotoagent"));
    assert_eq!(projects[0]["name"].as_str(), Some("kyotoagent"));
    assert_eq!(projects[0]["path"].as_str(), kyotoagent.to_str());
}

#[tokio::test]
async fn a_worktree_keeps_the_project_of_the_requested_workspace() {
    let fixture = Fixture::new("project-worktree", Vec::new()).await;
    let repo = fixture.root.join("repo");
    fs::create_dir_all(&repo).expect("the repo exists");
    git_init(&repo);
    append_project(&fixture.root, "kyotoagent", &repo);
    let body = serde_json::json!({
        "workspace": repo.to_str().expect("a path"),
        "worktree": true
    })
    .to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&body))
        .await;
    assert_eq!(status, 201, "{response}");
    let created: Value = serde_json::from_str(&response).expect("json");
    let id = created["id"].as_str().expect("an id");
    let rows = fixture.client.list().await;
    assert_eq!(rows[0]["project"].as_str(), Some("kyotoagent"), "{rows:?}");
    assert_ne!(rows[0]["workspace"].as_str(), repo.to_str());
    let meta =
        fs::read_to_string(fixture.root.join("sessions").join(id).join("meta.json")).expect("meta");
    let meta: Value = serde_json::from_str(&meta).expect("meta json");
    assert_eq!(meta["requestedWorkspace"].as_str(), repo.to_str());
}

#[tokio::test]
async fn the_project_list_follows_the_config_and_an_empty_file_is_empty() {
    let empty = Fixture::new("projects-empty", Vec::new()).await;
    let (status, body) = empty.client.request("GET", "/v1/projects", None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, "[]");

    let fixture = Fixture::new("projects-list", Vec::new()).await;
    let path = fixture.root.join("config.toml");
    let mut text = fs::read_to_string(&path).expect("the config reads");
    text.push_str(
        "\n[projects.kyotoagent]\npath = \"/work/kyotoagent\"\n\n[projects.acpbot]\npath = \"/work/acpbot\"\n",
    );
    fs::write(&path, text).expect("the config writes");
    let (status, body) = fixture.client.request("GET", "/v1/projects", None).await;
    assert_eq!(status, 200, "{body}");
    let rows: Vec<Value> = serde_json::from_str(&body).expect("the list is JSON");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "kyotoagent");
    assert_eq!(rows[0]["name"], "kyotoagent");
    assert_eq!(rows[0]["path"], "/work/kyotoagent");
    assert_eq!(rows[1]["id"], "acpbot");
    assert_eq!(rows[1]["name"], "acpbot");
    assert_eq!(rows[1]["path"], "/work/acpbot");
}

#[tokio::test]
async fn a_session_file_reads_inside_the_workspace() {
    let fixture = Fixture::new("file-inside", Vec::new()).await;
    let id = fixture.add_session("in").await;
    let workspace = fixture.root.join("w-in");
    fs::write(workspace.join("README.md"), "hello from readme").expect("readme");
    let (status, body) = fixture
        .client
        .request(
            "GET",
            &format!("/v1/sessions/{id}/file?path=README.md"),
            None,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let json: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["path"], "README.md");
    assert_eq!(json["text"], "hello from readme");
    assert_eq!(json["truncated"], false);
}

#[tokio::test]
async fn a_session_file_outside_the_workspace_is_400() {
    let fixture = Fixture::new("file-out", Vec::new()).await;
    let id = fixture.add_session("out").await;
    let (status, body) = fixture
        .client
        .request(
            "GET",
            &format!("/v1/sessions/{id}/file?path=../secret"),
            None,
        )
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("outside"), "{body}");
}

#[tokio::test]
async fn a_missing_session_file_is_404() {
    let fixture = Fixture::new("file-missing", Vec::new()).await;
    let id = fixture.add_session("miss").await;
    let (status, body) = fixture
        .client
        .request(
            "GET",
            &format!("/v1/sessions/{id}/file?path=missing.txt"),
            None,
        )
        .await;
    assert_eq!(status, 404, "{body}");
}

#[tokio::test]
async fn a_binary_session_file_is_not_text() {
    let fixture = Fixture::new("file-bin", Vec::new()).await;
    let id = fixture.add_session("bin").await;
    let workspace = fixture.root.join("w-bin");
    fs::write(workspace.join("blob.bin"), [0xff, 0xfe, 0x00]).expect("blob");
    let (status, body) = fixture
        .client
        .request(
            "GET",
            &format!("/v1/sessions/{id}/file?path=blob.bin"),
            None,
        )
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("not text"), "{body}");
}

#[tokio::test]
async fn a_long_session_file_is_truncated() {
    let fixture = Fixture::new("file-trunc", Vec::new()).await;
    let id = fixture.add_session("trunc").await;
    let workspace = fixture.root.join("w-trunc");
    let big = "x".repeat(kyotoagent::tools::READ_LIMIT + 50);
    fs::write(workspace.join("big.txt"), &big).expect("big");
    let (status, body) = fixture
        .client
        .request("GET", &format!("/v1/sessions/{id}/file?path=big.txt"), None)
        .await;
    assert_eq!(status, 200, "{body}");
    let json: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["truncated"], true);
    assert_eq!(
        json["text"].as_str().unwrap().len(),
        kyotoagent::tools::READ_LIMIT
    );
}

fn prose(text: &str) -> String {
    serde_json::json!({
        "choices": [{
            "message": { "role": "assistant", "content": text }
        }]
    })
    .to_string()
}

fn title_bodies(bodies: &[String]) -> Vec<Value> {
    bodies
        .iter()
        .filter_map(|body| {
            let value: Value = serde_json::from_str(body).ok()?;
            (value.get("model").and_then(Value::as_str) == Some("title-fast")).then_some(value)
        })
        .collect()
}

fn meta_text(root: &std::path::Path, id: &str) -> String {
    fs::read_to_string(
        root.join(kyotoagent::server::SESSIONS_DIR)
            .join(id)
            .join("meta.json"),
    )
    .expect("meta.json reads")
}

async fn wait_for_title_request(fake: &FakeServer) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if title_bodies(&fake.bodies()).len() == 1 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the title request was not sent: {:?}",
            fake.bodies()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn test_temp_dir() -> std::path::PathBuf {
    if cfg!(target_os = "macos") {
        std::path::PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    }
}

const THE_ASK: &str = "Add a readme line that names the binary.";

#[tokio::test]
async fn the_first_ask_stores_a_title_and_a_second_ask_leaves_it() {
    let fixture = Fixture::titled(
        "title-once",
        vec![
            Canned::Json(prose("Done.")),
            Canned::Json(prose("Done again.")),
        ],
        "title-fast",
    )
    .await;
    fixture
        ._fake
        .set_title("title-fast", Canned::Json(prose("Name the binary")));
    let id = fixture.add_session("readme").await;
    let (status, response) = fixture.client.message(&id, THE_ASK).await;
    assert_eq!(status, 202, "{response}");
    fixture.client.wait_for_status(&id, "idle").await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if meta_text(&fixture.root, &id).contains("Name the binary") {
            break;
        }
        assert!(Instant::now() < deadline, "the title was not stored");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let row = fixture
        .client
        .list()
        .await
        .into_iter()
        .find(|row| row["id"] == id)
        .expect("the session is listed");
    assert_eq!(row["title"], "Name the binary");
    assert!(row["workspace"].as_str().unwrap().ends_with("w-readme"));

    fixture
        ._fake
        .set_title("title-fast", Canned::Json(prose("A different title")));
    let (status, response) = fixture.client.message(&id, "Change the title.").await;
    assert_eq!(status, 202, "{response}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = fixture.client.view(&id).await;
        let results = view["cards"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|card| card["kind"] == "result")
            .count();
        if view["status"] == "idle" && results >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "the second ask did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let titles = title_bodies(&fixture._fake.bodies());
    assert_eq!(titles.len(), 1, "{titles:?}");
    assert_eq!(titles[0]["stream"], false);
    assert!(titles[0].get("tools").is_none(), "{}", titles[0]);
    assert!(titles[0].get("reasoning_effort").is_none(), "{}", titles[0]);
    assert_eq!(
        titles[0]["messages"][0]["content"],
        kyotoagent::chat::TITLE_SYSTEM
    );
    assert_eq!(titles[0]["messages"][1]["content"], THE_ASK);
    let meta = meta_text(&fixture.root, &id);
    assert!(meta.contains("Name the binary"), "{meta}");
    assert!(!meta.contains("A different title"), "{meta}");
    let row = fixture
        .client
        .list()
        .await
        .into_iter()
        .find(|row| row["id"] == id)
        .expect("the session is listed");
    assert_eq!(row["title"], "Name the binary");
}

#[tokio::test]
async fn a_title_model_failure_leaves_the_directory_name() {
    let fixture = Fixture::titled(
        "title-fail",
        vec![Canned::Json(prose("Done."))],
        "title-fast",
    )
    .await;
    fixture
        ._fake
        .set_title("title-fast", Canned::Status(500, "down".into()));
    let id = fixture.add_session("readme").await;
    fixture.client.message(&id, THE_ASK).await;
    fixture.client.wait_for_status(&id, "idle").await;
    wait_for_title_request(&fixture._fake).await;
    let meta = meta_text(&fixture.root, &id);
    assert!(!meta.contains("\"title\""), "{meta}");
    let row = fixture
        .client
        .list()
        .await
        .into_iter()
        .find(|row| row["id"] == id)
        .expect("the session is listed");
    assert!(
        row.get("title").is_none() || row["title"].is_null(),
        "{row}"
    );
    assert!(row["workspace"].as_str().unwrap().ends_with("w-readme"));
}

#[tokio::test]
async fn an_empty_title_reply_leaves_the_directory_name() {
    let fixture = Fixture::titled(
        "title-empty",
        vec![Canned::Json(prose("Done."))],
        "title-fast",
    )
    .await;
    fixture
        ._fake
        .set_title("title-fast", Canned::Json(prose("")));
    let id = fixture.add_session("readme").await;
    fixture.client.message(&id, THE_ASK).await;
    fixture.client.wait_for_status(&id, "idle").await;
    wait_for_title_request(&fixture._fake).await;
    let meta = meta_text(&fixture.root, &id);
    assert!(!meta.contains("\"title\""), "{meta}");
}

#[tokio::test]
async fn an_empty_title_model_skips_the_call() {
    let fixture = Fixture::titled("title-skip", vec![Canned::Json(prose("Done."))], "").await;
    let id = fixture.add_session("readme").await;
    fixture.client.message(&id, THE_ASK).await;
    fixture.client.wait_for_status(&id, "idle").await;
    let titles = title_bodies(&fixture._fake.bodies());
    assert!(titles.is_empty(), "{titles:?}");
    let meta = meta_text(&fixture.root, &id);
    assert!(!meta.contains("\"title\""), "{meta}");
    let row = fixture
        .client
        .list()
        .await
        .into_iter()
        .find(|row| row["id"] == id)
        .expect("the session is listed");
    assert!(
        row.get("title").is_none() || row["title"].is_null(),
        "{row}"
    );
    assert!(row["workspace"].as_str().unwrap().ends_with("w-readme"));
}

#[tokio::test]
async fn the_first_ask_titles_with_the_default_model() {
    let fixture = Fixture::omitted_title("title-default", vec![Canned::Json(prose("Done."))]).await;
    fixture._fake.set_title(
        "google/gemini-3.8-flash",
        Canned::Json(prose("Name the binary")),
    );
    let id = fixture.add_session("readme").await;
    fixture.client.message(&id, THE_ASK).await;
    fixture.client.wait_for_status(&id, "idle").await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let bodies = fixture._fake.bodies();
        let titled = bodies
            .iter()
            .any(|body| body.contains("\"google/gemini-3.8-flash\""));
        if titled && meta_text(&fixture.root, &id).contains("Name the binary") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the default title was not stored: {bodies:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let row = fixture
        .client
        .list()
        .await
        .into_iter()
        .find(|row| row["id"] == id)
        .expect("the session is listed");
    assert_eq!(row["title"], "Name the binary");
}

#[tokio::test]
async fn archive_hides_the_session_and_unarchive_restores_it() {
    let fixture = Fixture::new("archive-session", write_then_finish()).await;
    let id = fixture.add_session("notes").await;
    let dir = fixture.root.join("sessions").join(&id);
    let (status, _) = fixture.client.message(&id, "Create notes.md.").await;
    assert_eq!(status, 202);
    fixture.client.wait_for_status(&id, "waiting").await;

    let (status, response) = fixture
        .client
        .request("POST", &format!("/v1/sessions/{id}/archive"), None)
        .await;
    assert_eq!(status, 204, "{response}");
    assert!(dir.join("meta.json").is_file(), "the log stays");
    let row = fixture
        .client
        .list()
        .await
        .into_iter()
        .find(|row| row["id"] == id)
        .expect("the session stays listed");
    assert_eq!(row["archived"], true);
    assert!(row["archivedAt"].as_str().is_some());

    let (status, response) = fixture.client.message(&id, "Again.").await;
    assert_eq!(
        status, 409,
        "an archived session refuses a new ask: {response}"
    );

    let (status, response) = fixture
        .client
        .request(
            "POST",
            &format!("/v1/sessions/{id}/archive"),
            Some(r#"{"archived":false}"#),
        )
        .await;
    assert_eq!(status, 204, "{response}");
    let row = fixture
        .client
        .list()
        .await
        .into_iter()
        .find(|row| row["id"] == id)
        .expect("the session is listed");
    assert_eq!(row["archived"], serde_json::Value::Null);
    let (status, response) = fixture.client.message(&id, "Again.").await;
    assert_eq!(
        status, 202,
        "a restored session takes a new ask: {response}"
    );

    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions/missing/archive", None)
        .await;
    assert_eq!(status, 404, "{response}");
}

#[tokio::test]
async fn archive_removes_subagent_sessions() {
    let fixture = Fixture::new("archive-children", Vec::new()).await;
    let id = fixture.add_session("notes").await;
    let sessions = fixture.root.join("sessions");
    let child_dir = sessions.join("child01");
    let workspace = fixture.workspace("notes");
    let mut child = kyotoagent::session::SessionMeta::new(
        "child01",
        &workspace,
        "test/model",
        "2026-10-06T00:00:00.000Z",
    );
    child.parent_id = Some(id.clone());
    kyotoagent::session::Session::at(&child_dir)
        .create(&child)
        .expect("child session");
    fixture
        .runner
        .add_session(&kyotoagent::session::Session::at(&child_dir))
        .expect("child loads");

    let (status, response) = fixture
        .client
        .request("POST", &format!("/v1/sessions/{id}/archive"), None)
        .await;
    assert_eq!(status, 204, "{response}");
    assert!(!child_dir.exists(), "the subagent session is removed");
    assert!(sessions.join(&id).join("meta.json").is_file());
    let listed = fixture.client.list().await;
    assert!(listed.iter().any(|row| row["id"] == id));
    assert!(!listed.iter().any(|row| row["id"] == "child01"));
}

#[tokio::test]
async fn delete_session_is_204_and_an_unknown_id_is_404() {
    let fixture = Fixture::new("delete-session", write_then_finish()).await;
    let id = fixture.add_session("notes").await;
    let dir = fixture.root.join("sessions").join(&id);
    assert!(dir.join("meta.json").is_file());
    let (status, response) = fixture
        .client
        .request("DELETE", "/v1/sessions/missing", None)
        .await;
    assert_eq!(status, 404, "{response}");
    let (status, response) = fixture
        .client
        .request("DELETE", &format!("/v1/sessions/{id}"), None)
        .await;
    assert_eq!(status, 204, "{response}");
    assert!(!dir.exists());
    assert!(fixture.client.list().await.is_empty());
}

#[tokio::test]
async fn delete_worktree_is_409_unless_isolation_is_worktree() {
    let fixture = Fixture::new("delete-worktree", write_then_finish()).await;
    let repo = fixture.root.join("repo");
    git_repo(&repo);
    let parent = fixture
        .client
        .create_session(repo.to_str().expect("a path"))
        .await;
    let child = fixture
        .client
        .create_session(repo.to_str().expect("a path"))
        .await;
    let (status, response) = fixture
        .client
        .request("DELETE", &format!("/v1/sessions/{parent}/worktree"), None)
        .await;
    assert_eq!(status, 409, "{response}");
    assert!(response.contains("isolation is not worktree"));
    let dest = kyotoagent::subagent::add_worktree(&repo, &child).expect("worktree");
    assert!(dest.is_dir());
    kyotoagent::session::Session::at(&fixture.root.join("sessions").join(&child))
        .update(|meta| {
            meta.parent_id = Some(parent.clone());
            meta.isolation = Some("worktree".into());
            meta.workspace = dest.display().to_string();
            true
        })
        .expect("meta");
    let (status, response) = fixture
        .client
        .request("DELETE", "/v1/sessions/missing/worktree", None)
        .await;
    assert_eq!(status, 404, "{response}");
    let (status, response) = fixture
        .client
        .request("DELETE", &format!("/v1/sessions/{child}/worktree"), None)
        .await;
    assert_eq!(status, 204, "{response}");
    assert!(!dest.exists());
    let rows = fixture.client.list().await;
    let row = rows
        .iter()
        .find(|row| row["id"] == child)
        .expect("the child stays listed");
    assert!(row.get("isolation").is_none());
    let parent_row = rows
        .iter()
        .find(|row| row["id"] == parent)
        .expect("the parent stays listed");
    assert_eq!(row["workspace"], parent_row["workspace"]);
}

#[tokio::test]
async fn deleting_a_shared_child_keeps_the_parent_worktree() {
    let fixture = Fixture::new("shared-child-delete", Vec::new()).await;
    let repo = fixture.root.join("repo");
    git_repo(&repo);
    let body = serde_json::json!({
        "workspace": repo.to_str().expect("a path"),
        "worktree": true
    })
    .to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/sessions", Some(&body))
        .await;
    assert_eq!(status, 201, "{response}");
    let parent: Value = serde_json::from_str(&response).expect("parent json");
    let parent_id = parent["id"].as_str().expect("parent id").to_string();
    let parent_workspace = parent["workspace"].as_str().expect("parent workspace");
    fs::write(
        std::path::Path::new(parent_workspace).join("parent-only.txt"),
        "parent work\n",
    )
    .expect("parent file");
    let child = fixture.client.create_session(parent_workspace).await;
    kyotoagent::session::Session::at(&fixture.root.join("sessions").join(&child))
        .update(|meta| {
            meta.parent_id = Some(parent_id.clone());
            meta.isolation = Some("none".into());
            true
        })
        .expect("child meta");
    let rows = fixture.client.list().await;
    let child_row = rows
        .iter()
        .find(|row| row["id"] == child)
        .expect("the child is listed");
    assert!(child_row.get("worktree").is_none(), "{child_row}");
    let (status, response) = fixture
        .client
        .request("GET", &format!("/v1/sessions/{child}/workspace"), None)
        .await;
    assert_eq!(status, 200, "{response}");
    let workspace: Value = serde_json::from_str(&response).expect("workspace json");
    assert_eq!(workspace["managed"], false, "{response}");
    let (status, response) = fixture
        .client
        .request(
            "DELETE",
            &format!("/v1/sessions/{child}?delete_workspace=true&confirm_dirty=true"),
            None,
        )
        .await;
    assert_eq!(status, 409, "{response}");
    assert!(response.contains("Only a managed worktree can be deleted."));
    assert!(std::path::Path::new(parent_workspace).exists());
    assert_eq!(
        fs::read_to_string(std::path::Path::new(parent_workspace).join("parent-only.txt"))
            .expect("parent file"),
        "parent work\n"
    );
    assert!(fixture.root.join("sessions").join(&child).exists());
    assert!(fixture.root.join("sessions").join(&parent_id).exists());
}

#[tokio::test]
async fn delete_cancels_a_running_command_and_a_worktree_child() {
    let marker = "kyotoagent-hold-delete";
    let replies = vec![Canned::Json(tool_call_reply(vec![(
        "run",
        serde_json::json!({ "argv": ["bash", "-c", format!("exec -a {marker} sleep 30")] }),
    )]))];
    let fixture = Fixture::new("delete-running", replies).await;
    let repo = fixture.root.join("repo");
    git_repo(&repo);
    let parent = fixture
        .client
        .create_session(repo.to_str().expect("a path"))
        .await;
    let child = fixture
        .client
        .create_session(repo.to_str().expect("a path"))
        .await;
    let _kill = KillMatching(marker.to_string());
    fixture.client.message(&child, "Hold.").await;
    fixture.client.wait_for_status(&child, "waiting").await;
    let permission = fixture.client.open_permission_id(&child).await;
    let (status, response) = fixture
        .client
        .answer(&child, &permission, "allow_once")
        .await;
    assert_eq!(status, 204, "{response}");
    wait_until(|| process_matches(marker), "the command starts").await;
    let dest = kyotoagent::subagent::add_worktree(&repo, &child).expect("worktree");
    kyotoagent::session::Session::at(&fixture.root.join("sessions").join(&child))
        .update(|meta| {
            meta.parent_id = Some(parent.clone());
            meta.isolation = Some("worktree".into());
            meta.workspace = dest.display().to_string();
            true
        })
        .expect("meta");
    let (status, response) = fixture
        .client
        .request(
            "DELETE",
            &format!("/v1/sessions/{child}?delete_workspace=true"),
            None,
        )
        .await;
    assert_eq!(status, 204, "{response}");
    assert!(!fixture.root.join("sessions").join(&child).exists());
    assert!(!dest.exists());
    wait_until(|| !process_matches(marker), "the command stops").await;
    assert!(fixture
        .client
        .list()
        .await
        .iter()
        .any(|row| row["id"] == parent));
}

struct KillMatching(String);

impl Drop for KillMatching {
    fn drop(&mut self) {
        let pattern = format!("[{}]{}", &self.0[..1], &self.0[1..]);
        let _ = std::process::Command::new("pkill")
            .args(["-f", &pattern])
            .status();
    }
}

fn process_matches(needle: &str) -> bool {
    let pattern = format!("[{}]{}", &needle[..1], &needle[1..]);
    std::process::Command::new("pgrep")
        .args(["-f", &pattern])
        .status()
        .is_ok_and(|status| status.success())
}

async fn wait_until(mut pred: impl FnMut() -> bool, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pred() {
        assert!(Instant::now() < deadline, "{label}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn append_config(root: &std::path::Path, extra: &str) {
    let path = root.join("config.toml");
    let text = fs::read_to_string(&path).expect("the config reads");
    fs::write(&path, format!("{text}{extra}")).expect("the config writes");
}

async fn create_profiled(client: &Client, workspace: &str, profile: Option<&str>) -> (u16, String) {
    let body = match profile {
        Some(profile) => serde_json::json!({ "workspace": workspace, "profile": profile }),
        None => serde_json::json!({ "workspace": workspace }),
    };
    client
        .request("POST", "/v1/sessions", Some(&body.to_string()))
        .await
}

#[tokio::test]
async fn an_unknown_profile_creates_nothing_and_the_list_is_the_file() {
    let fixture = Fixture::new("profiles", Vec::new()).await;
    let (status, response) = fixture.client.request("GET", "/v1/profiles", None).await;
    assert_eq!(status, 200, "{response}");
    let names: Vec<String> = serde_json::from_str(&response).expect("names");
    assert!(names.is_empty(), "{names:?}");
    append_config(
        &fixture.root,
        "\n[profiles.planning]\ntools = [\"read_file\", \"finish\"]\n\n[profiles.review]\ntools = [\"read_file\", \"grep\", \"finish\"]\n",
    );
    let (status, response) = fixture.client.request("GET", "/v1/profiles", None).await;
    assert_eq!(status, 200, "{response}");
    let names: Vec<String> = serde_json::from_str(&response).expect("names");
    assert_eq!(names, vec!["planning".to_string(), "review".to_string()]);
    let workspace = fixture.root.join("w-missing");
    fs::create_dir_all(&workspace).expect("workspace");
    let (status, response) = create_profiled(
        &fixture.client,
        workspace.to_str().expect("a path"),
        Some("missing"),
    )
    .await;
    assert_eq!(status, 400, "{response}");
    assert!(response.contains("unknown profile: missing"), "{response}");
    assert_eq!(session_count(&fixture.root), 0);
}

#[tokio::test]
async fn a_project_profile_wins_and_an_empty_create_stores_none() {
    let root = test_temp_dir().join(format!(
        "kyotoagent-server-{}-project-profile",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let fake = FakeServer::start(Vec::new());
    let kyotoagent = root.join("w-kyotoagent");
    let notes = root.join("w-notes");
    fs::create_dir_all(&kyotoagent).expect("kyotoagent");
    fs::create_dir_all(&notes).expect("notes");
    let config_text = format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\nprofile = \"planning\"\n\n[projects.kyotoagent]\npath = \"{}\"\nprofile = \"review\"\n\n[projects.notes]\npath = \"{}\"\n\n[profiles.planning]\ntools = [\"read_file\", \"finish\", \"todo\", \"use_skill\"]\nskills = [\"skill-a\"]\n\n[profiles.review]\ntools = [\"read_file\", \"grep\", \"list_dir\", \"web_fetch\", \"ask\", \"finish\"]\n",
        fake.base_url(),
        kyotoagent.display(),
        notes.display()
    );
    let fixture = Fixture::boot(root, fake, config_text).await;
    let (status, response) = create_profiled(
        &fixture.client,
        fixture.workspace("kyotoagent").to_str().expect("a path"),
        None,
    )
    .await;
    assert_eq!(status, 201, "{response}");
    let on: Value = serde_json::from_str(&response).expect("created");
    let on_id = on["id"].as_str().expect("id");
    assert_eq!(
        session_meta(&fixture.root, on_id).profile.as_deref(),
        Some("review")
    );
    let (status, response) = create_profiled(
        &fixture.client,
        fixture.workspace("notes").to_str().expect("a path"),
        None,
    )
    .await;
    assert_eq!(status, 201, "{response}");
    let inherited: Value = serde_json::from_str(&response).expect("created");
    let inherited_id = inherited["id"].as_str().expect("id");
    assert_eq!(
        session_meta(&fixture.root, inherited_id).profile.as_deref(),
        Some("planning")
    );
    let (status, response) = create_profiled(
        &fixture.client,
        fixture.workspace("notes").to_str().expect("a path"),
        Some(""),
    )
    .await;
    assert_eq!(status, 201, "{response}");
    let cleared: Value = serde_json::from_str(&response).expect("created");
    let cleared_id = cleared["id"].as_str().expect("id");
    assert!(session_meta(&fixture.root, cleared_id).profile.is_none());
    let raw = fs::read_to_string(
        fixture
            .root
            .join(kyotoagent::server::SESSIONS_DIR)
            .join(cleared_id)
            .join("meta.json"),
    )
    .expect("meta");
    assert!(!raw.contains("\"profile\""), "{raw}");
    let rows = fixture.client.list().await;
    let review = rows
        .iter()
        .find(|row| row["id"] == on_id)
        .expect("review row");
    assert_eq!(review["profile"], "review");
    let none = rows
        .iter()
        .find(|row| row["id"] == cleared_id)
        .expect("cleared row");
    assert!(none.get("profile").is_none(), "{none}");
}

#[tokio::test]
async fn an_unknown_live_profile_leaves_the_session_and_the_file() {
    let fixture = Fixture::new("live-profile", Vec::new()).await;
    append_config(
        &fixture.root,
        "\n[profiles.review]\ntools = [\"read_file\", \"finish\"]\n",
    );
    let before = fs::read(fixture.root.join("config.toml")).expect("config bytes");
    let workspace = fixture.root.join("w-live");
    fs::create_dir_all(&workspace).expect("workspace");
    let (status, response) = create_profiled(
        &fixture.client,
        workspace.to_str().expect("a path"),
        Some("review"),
    )
    .await;
    assert_eq!(status, 201, "{response}");
    let created: Value = serde_json::from_str(&response).expect("created");
    let id = created["id"].as_str().expect("id");
    let (status, response) = fixture
        .client
        .request(
            "POST",
            &format!("/v1/sessions/{id}/profile"),
            Some(&serde_json::json!({ "profile": "missing" }).to_string()),
        )
        .await;
    assert_eq!(status, 400, "{response}");
    assert!(response.contains("unknown profile: missing"), "{response}");
    assert_eq!(
        session_meta(&fixture.root, id).profile.as_deref(),
        Some("review")
    );
    assert_eq!(
        fs::read(fixture.root.join("config.toml")).expect("config bytes"),
        before
    );
    let (status, response) = fixture
        .client
        .request(
            "POST",
            &format!("/v1/sessions/{id}/profile"),
            Some(&serde_json::json!({ "profile": "  " }).to_string()),
        )
        .await;
    assert_eq!(status, 204, "{response}");
    assert!(session_meta(&fixture.root, id).profile.is_none());
    let raw = fs::read_to_string(
        fixture
            .root
            .join(kyotoagent::server::SESSIONS_DIR)
            .join(id)
            .join("meta.json"),
    )
    .expect("meta");
    assert!(!raw.contains("\"profile\""), "{raw}");
    let rows = fixture.client.list().await;
    assert!(rows[0].get("profile").is_none(), "{rows:?}");
    assert_eq!(
        fs::read(fixture.root.join("config.toml")).expect("config bytes"),
        before
    );
}

#[tokio::test]
async fn a_review_profile_refuses_write_and_run_even_with_yolo() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![
            (
                "write_file",
                serde_json::json!({ "path": "notes.md", "contents": "hello" }),
            ),
            (
                "run",
                serde_json::json!({ "argv": ["touch", "ran-marker"] }),
            ),
        ])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Stopped.", "proof": "nothing was written." }),
        )])),
    ];
    let fixture = Fixture::new("review-yolo", replies).await;
    append_config(
        &fixture.root,
        "\n[profiles.review]\ntools = [\"read_file\", \"grep\", \"list_dir\", \"web_fetch\", \"ask\", \"finish\"]\n",
    );
    let workspace = fixture.workspace("91bc");
    fs::create_dir_all(&workspace).expect("workspace");
    let (status, response) = create_profiled(
        &fixture.client,
        workspace.to_str().expect("a path"),
        Some("review"),
    )
    .await;
    assert_eq!(status, 201, "{response}");
    let created: Value = serde_json::from_str(&response).expect("created");
    let id = created["id"].as_str().expect("id").to_string();
    post_yolo(&fixture.client, &id, true).await;
    let (status, response) = fixture.client.message(&id, "Write and run.").await;
    assert_eq!(status, 202, "{response}");
    fixture.client.wait_for_status(&id, "idle").await;
    assert!(!workspace.join("notes.md").exists());
    assert!(!workspace.join("ran-marker").exists());
    let log = fixture.client.events(&id).await;
    assert!(log.contains("unknown tool: write_file"), "{log}");
    assert!(log.contains("unknown tool: run"), "{log}");
}

#[tokio::test]
async fn a_profile_that_allows_write_still_skips_the_prompt_under_yolo() {
    let fixture = Fixture::new("writer-yolo", write_then_finish()).await;
    append_config(
        &fixture.root,
        "\n[profiles.writer]\ntools = [\"write_file\", \"finish\"]\n",
    );
    let workspace = fixture.workspace("91bc");
    fs::create_dir_all(&workspace).expect("workspace");
    let (status, response) = create_profiled(
        &fixture.client,
        workspace.to_str().expect("a path"),
        Some("writer"),
    )
    .await;
    assert_eq!(status, 201, "{response}");
    let created: Value = serde_json::from_str(&response).expect("created");
    let id = created["id"].as_str().expect("id").to_string();
    post_yolo(&fixture.client, &id, true).await;
    let (status, response) = fixture.client.message(&id, "Create notes.md.").await;
    assert_eq!(status, 202, "{response}");
    fixture.client.wait_for_status(&id, "idle").await;
    assert_eq!(
        fs::read_to_string(workspace.join("notes.md")).expect("notes"),
        "hello"
    );
}

fn git_repo(path: &std::path::Path) {
    fs::create_dir_all(path).expect("repo");
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git");
        assert!(status.success(), "{args:?}");
    };
    run(&["init"]);
    run(&["config", "user.email", "t@example.com"]);
    run(&["config", "user.name", "Test"]);
    fs::write(path.join("README"), "hi\n").expect("readme");
    run(&["add", "README"]);
    run(&["commit", "-m", "init"]);
}

#[tokio::test]
async fn queued_messages_have_stable_ids_and_remove_only_the_selected_entry() {
    let fixture = Fixture::new("remove-queue", vec![Canned::Json(prose("Done."))]).await;
    let session = fixture.add_session("remove-queue").await;
    fixture.pause_before(0);
    assert_eq!(fixture.client.message(&session, "live").await.0, 202);
    wait_holding(&fixture).await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        let (status, body) = fixture.client.message(&session, "same text").await;
        assert_eq!(status, 202);
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["queued"], true);
        ids.push(body["queuedId"].as_str().unwrap().to_string());
    }
    assert_ne!(ids[0], ids[1]);
    assert_ne!(ids[1], ids[2]);
    let image =
        kyotoagent::attachment::ImageAttachment::from_bytes("dog.png", kyotoagent::splash::PNG)
            .unwrap();
    let (status, body) = fixture
        .client
        .message_body(
            &session,
            serde_json::json!({ "text": "", "images": [image] }),
        )
        .await;
    assert_eq!(status, 202);
    let image_id = serde_json::from_str::<Value>(&body).unwrap()["queuedId"]
        .as_str()
        .unwrap()
        .to_string();
    let view = fixture.client.view(&session).await;
    assert_eq!(
        view["queue"],
        serde_json::json!(["same text", "same text", "same text", ""])
    );
    assert_eq!(view["queueItems"][3]["imageCount"], 1);
    let path = format!("/v1/sessions/{session}/queue/{}", ids[1]);
    assert_eq!(fixture.client.request("DELETE", &path, None).await.0, 204);
    assert_eq!(fixture.client.request("DELETE", &path, None).await.0, 409);
    let view = fixture.client.view(&session).await;
    let remaining: Vec<_> = view["queueItems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        remaining,
        [ids[0].as_str(), ids[2].as_str(), image_id.as_str()]
    );
    assert_eq!(view["status"], "working");
    for id in [&ids[2], &image_id] {
        assert_eq!(
            fixture
                .client
                .request(
                    "DELETE",
                    &format!("/v1/sessions/{session}/queue/{id}"),
                    None
                )
                .await
                .0,
            204
        );
    }
    fixture.release_chat();
    fixture.client.wait_for_status(&session, "idle").await;
    assert_eq!(
        fixture
            .client
            .request(
                "DELETE",
                &format!("/v1/sessions/{session}/queue/{}", ids[0]),
                None
            )
            .await
            .0,
        409
    );
    let view = fixture.client.view(&session).await;
    assert!(view.get("queueItems").is_none());
    let events = fixture.client.events(&session).await;
    assert!(!events.contains("Stopped."));
    assert_eq!(
        fixture
            .client
            .request("DELETE", "/v1/sessions/missing/queue/missing", None)
            .await
            .0,
        404
    );
}

#[tokio::test]
async fn projects_validate_paths_and_preserve_configuration_on_update() {
    let fixture = Fixture::new("project-config", write_then_finish()).await;
    let path = fixture.root.join("config.toml");
    fs::write(
        &path,
        format!(
            "base_url = \"{}\"\nmodel = \"test/model\"\nyolo = true\n",
            fixture._fake.base_url()
        ),
    )
    .unwrap();
    let project =
        serde_json::json!({"id":"notes", "name":"Notes", "path":fixture.root}).to_string();
    let (status, response) = fixture
        .client
        .request("POST", "/v1/projects", Some(&project))
        .await;
    assert_eq!(status, 201, "{response}");
    let original = fs::read_to_string(&path).unwrap();
    let (status, _) = fixture
        .client
        .request("POST", "/v1/projects", Some(&project))
        .await;
    assert_eq!(status, 409);
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    let bad = serde_json::json!({"id":"missing", "name":"Missing", "path":fixture.root.join("does-not-exist")}).to_string();
    assert_eq!(
        fixture
            .client
            .request("POST", "/v1/projects", Some(&bad))
            .await
            .0,
        400
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    let edited =
        serde_json::json!({"id":"notes", "name":"Notebook", "path":fixture.root}).to_string();
    assert_eq!(
        fixture
            .client
            .request("PUT", "/v1/projects/notes", Some(&edited))
            .await
            .0,
        204
    );
    let config = Config::load(&path).unwrap();
    assert!(config.yolo);
    assert_eq!(config.base_url, fixture._fake.base_url());
    assert_eq!(config.projects[0].name, "Notebook");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn session_deletion_keeps_a_worktree_and_workspace_deletion_requires_dirty_confirmation() {
    let fixture = Fixture::new("delete-workspace-options", write_then_finish()).await;
    let repo = fixture.root.join("repo");
    git_repo(&repo);
    for remove in [false, true] {
        let body = serde_json::json!({"workspace":repo, "worktree":true}).to_string();
        let (status, response) = fixture
            .client
            .request("POST", "/v1/sessions", Some(&body))
            .await;
        assert_eq!(status, 201, "{response}");
        let created: Value = serde_json::from_str(&response).unwrap();
        let id = created["id"].as_str().unwrap();
        let workspace = PathBuf::from(created["workspace"].as_str().unwrap());
        fs::write(
            workspace.join("untracked.txt"),
            "keep this unless explicitly confirmed",
        )
        .unwrap();
        let (status, response) = fixture
            .client
            .request("GET", &format!("/v1/sessions/{id}/workspace"), None)
            .await;
        assert_eq!(status, 200);
        let details: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(details["managed"], true);
        assert!(details["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("untracked.txt")));
        assert_eq!(
            fixture
                .client
                .request(
                    "DELETE",
                    &format!("/v1/sessions/{id}?delete_workspace=true"),
                    None
                )
                .await
                .0,
            409
        );
        assert!(workspace.exists());
        assert!(fixture.root.join("sessions").join(id).exists());
        let path = format!("/v1/sessions/{id}?delete_workspace={remove}&confirm_dirty={remove}");
        assert_eq!(fixture.client.request("DELETE", &path, None).await.0, 204);
        assert_eq!(workspace.exists(), !remove);
        assert!(repo.exists());
    }
}

#[tokio::test]
async fn saved_layout_is_global_and_preserves_other_server_configuration() {
    let fixture = Fixture::new("global-layout", vec![]).await;
    let (status, body) = fixture.client.request("GET", "/v1/layout", None).await;
    assert_eq!(status, 200);
    assert_eq!(body.trim(), "null");
    let path = fixture.root.join("config.toml");
    let before = fs::read_to_string(&path).unwrap();
    let body = serde_json::json!({
        "left_open": false, "left_width": 38,
        "right_open": true, "right_width": 31,
        "right_panes": ["todos", "schedules"]
    })
    .to_string();
    let (status, response) = fixture
        .client
        .request("PUT", "/v1/layout", Some(&body))
        .await;
    assert_eq!(status, 204, "{response}");
    let persisted = Config::load(&path).unwrap();
    assert_eq!(persisted.model, Config::from_toml(&before).unwrap().model);
    assert_eq!(persisted.layout.as_ref().unwrap().right_width, 31);
    fixture.add_session("layout-existing").await;
    fixture.add_session("layout-new").await;
    let (status, response) = fixture.client.request("GET", "/v1/layout", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&response).unwrap(),
        serde_json::from_str::<Value>(&body).unwrap()
    );
    let bytes = fs::read(&path).unwrap();
    for value in [
        serde_json::json!({"left_open": false, "left_width": 0, "right_open": true, "right_width": 31, "right_panes": ["todos"]}),
        serde_json::json!({"left_open": false, "left_width": 38, "right_open": true, "right_width": 31, "right_panes": ["invented"]}),
    ] {
        let (status, _) = fixture
            .client
            .request("PUT", "/v1/layout", Some(&value.to_string()))
            .await;
        assert!((400..500).contains(&status));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}
