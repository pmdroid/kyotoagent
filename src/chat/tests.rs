use super::*;

#[test]
fn image_messages_use_chat_and_responses_image_parts() {
    let image =
        crate::attachment::ImageAttachment::from_bytes("dog.png", crate::splash::PNG).unwrap();
    let message = Message::User {
        content: UserContent::with_images("Describe the dog".into(), std::slice::from_ref(&image)),
    };
    let chat = serde_json::to_value(&message).unwrap();
    assert_eq!(chat["role"], "user");
    assert_eq!(
        chat["content"][0],
        serde_json::json!({"type":"text", "text":"Describe the dog"})
    );
    assert_eq!(chat["content"][1]["type"], "image_url");
    assert_eq!(chat["content"][1]["image_url"]["url"], image.data_url());
    assert_eq!(serde_json::from_value::<Message>(chat).unwrap(), message);
    let responses = responses_body("vision", None, &[message], &[], false);
    assert_eq!(responses["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(responses["input"][0]["content"][1]["type"], "input_image");
    assert_eq!(
        responses["input"][0]["content"][1]["image_url"],
        image.data_url()
    );
    let plain = Message::User {
        content: "hello".into(),
    };
    assert_eq!(serde_json::to_value(plain).unwrap()["content"], "hello");
}
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// One request the fake server read, kept so a test can say what was sent.
#[derive(Clone, Debug)]
struct Received {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn body_json(&self) -> Value {
        serde_json::from_str(&self.body).expect("the request body is JSON")
    }
}

/// What the fake server answers with. `HangUp` closes the connection
/// without a reply, which is what a transport failure looks like from the
/// other side.
#[derive(Clone)]
enum Canned {
    Json(String),
    Raw(String),
    Status(u16, String),
    HangUp,
    Silent,
    Pace { gap: Duration, parts: Vec<String> },
}

/// A local HTTP server that records what it was sent and answers from a
/// queue of canned replies. One request per connection, so the recorded
/// request is always the one under test.
struct FakeServer {
    addr: SocketAddr,
    received: Arc<Mutex<Vec<Received>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
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
        let received = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(replies));
        let stop = Arc::new(AtomicBool::new(false));

        let handle = {
            let received = Arc::clone(&received);
            let replies = Arc::clone(&replies);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            serve_one(stream, &received, &replies, &stop);
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
            received,
            stop,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The requests read so far. One per test, so the first is the one.
    fn received(&self) -> Vec<Received> {
        self.received
            .lock()
            .expect("the log is not poisoned")
            .clone()
    }

    fn one(&self) -> Received {
        let all = self.received();
        assert_eq!(all.len(), 1, "the fake server saw one request");
        all.into_iter().next().expect("that request")
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
    received: &Arc<Mutex<Vec<Received>>>,
    replies: &Arc<Mutex<Vec<Canned>>>,
    stop: &AtomicBool,
) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let request_body = request.body.clone();
    received
        .lock()
        .expect("the log is not poisoned")
        .push(request);
    let reply = {
        let mut queue = replies.lock().expect("the queue is not poisoned");
        if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue.first().cloned().unwrap_or(Canned::HangUp)
        }
    };
    match reply {
        Canned::HangUp => {
            let _ = stream.shutdown(Shutdown::Both);
        }
        Canned::Json(body) => answer_completion(&mut stream, 200, &body, &request_body),
        Canned::Raw(body) => respond(&mut stream, 200, &body),
        Canned::Status(status, body) => respond(&mut stream, status, &body),
        Canned::Silent => hold_silent(&mut stream, stop),
        Canned::Pace { gap, parts } => pace(&mut stream, gap, &parts),
    }
}

fn open_event_stream(stream: &mut TcpStream) {
    let head =
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.flush();
}

fn write_chunk(stream: &mut TcpStream, data: &str) {
    let head = format!("{:x}\r\n", data.len());
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(data.as_bytes());
    let _ = stream.write_all(b"\r\n");
    let _ = stream.flush();
}

fn hold_silent(stream: &mut TcpStream, stop: &AtomicBool) {
    open_event_stream(stream);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
    let started = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(5) {
        let mut buf = [0_u8; 1];
        match stream.read(&mut buf) {
            Ok(0) => return,
            Err(err)
                if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut => {}
            Err(_) => return,
            Ok(_) => return,
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn pace(stream: &mut TcpStream, gap: Duration, parts: &[String]) {
    open_event_stream(stream);
    for part in parts {
        std::thread::sleep(gap);
        write_chunk(stream, part);
    }
    let _ = stream.write_all(b"0\r\n\r\n");
    let _ = stream.flush();
}

fn read_request(stream: &mut TcpStream) -> Option<Received> {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 1024];
    let head_end = loop {
        if let Some(at) = find(&raw, b"\r\n\r\n") {
            break at;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let mut lines = head.lines();
    let start = lines.next()?;
    let mut parts = start.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_lowercase(), value.trim().to_string()));
        }
    }
    let length: usize = headers
        .iter()
        .find(|(key, _)| key == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = raw[head_end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    Some(Received {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
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

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The one tool this client is ever pointed at in these tests.
fn read_file_tool() -> Tool {
    Tool::new(
        "read_file",
        "Read a file in the workspace.",
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }),
    )
}

#[test]
fn a_catalog_row_keeps_advertised_context_length() {
    let rows = parse_catalog(
            r#"{"data":[{"id":"grok-4.6","context_length":256000,"aliases":["grok-4"],"reasoning_efforts":["low","high"]}]}"#,
        )
        .expect("the catalog parses");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "grok-4.6");
    assert_eq!(rows[0].context_length, Some(256000));
    assert_eq!(rows[0].aliases, vec!["grok-4"]);
    assert_eq!(rows[0].reasoning_efforts, vec!["low", "high"]);
}

#[test]
fn catalog_xai_reasoning_effort_lists_are_kept_in_picker_order() {
    let row = parse_model(
        r#"{"id":"grok-4.6","capabilities":{"reasoning_effort":["xhigh","low","","high","medium"],"default_reasoning_effort":"high"}}"#,
    )
    .unwrap();
    assert_eq!(row.reasoning_efforts, ["low", "medium", "high", "xhigh"]);
    assert!(row.takes_effort());
    let image = parse_model(r#"{"id":"grok-imagine-image"}"#).unwrap();
    assert!(image.reasoning_efforts.is_empty());
}

#[test]
fn catalog_effort_capabilities_keep_every_supported_level() {
    let row = parse_model(
        r#"{"id":"opus","capabilities":{"effort":{"supported":true,"low":{"supported":true},"medium":{"supported":true},"high":{"supported":true},"xhigh":{"supported":true},"max":{"supported":true},"ultra":{"supported":true},"custom":{"supported":true},"disabled":{"supported":false},"missing":{},"malformed":{"supported":"true"}}}}"#,
    )
    .unwrap();
    assert_eq!(
        row.reasoning_efforts,
        ["low", "medium", "high", "xhigh", "max", "ultra", "custom"]
    );
    assert!(row.takes_effort());
}

#[test]
fn catalog_explicit_efforts_take_precedence_over_capabilities() {
    for efforts in [r#"["custom", "ultra"]"#, "[]"] {
        let rows = parse_catalog(&format!(
            r#"{{"data":[{{"id":"opus","reasoning_efforts":{efforts},"capabilities":{{"effort":{{"low":{{"supported":true}}}}}}}}]}}"#,
        ))
        .unwrap();
        let expected = if efforts == "[]" {
            vec!["low"]
        } else {
            vec!["custom", "ultra"]
        };
        assert_eq!(rows[0].reasoning_efforts, expected);
    }
}

#[test]
fn catalog_malformed_effort_capabilities_leave_efforts_empty() {
    for capabilities in [
        "null",
        "{}",
        r#"{"effort":true}"#,
        r#"{"effort":[]}"#,
        r#"{"reasoning_effort":"high"}"#,
        r#"{"reasoning_effort":[""]}"#,
    ] {
        let row =
            parse_model(&format!(r#"{{"id":"opus","capabilities":{capabilities}}}"#,)).unwrap();
        assert!(row.reasoning_efforts.is_empty());
    }
}

#[test]
fn catalog_length_falls_back_through_window_fields() {
    let rows = parse_catalog(r#"{"data":[{"id":"m","max_model_len":32000}]}"#)
        .expect("the catalog parses");
    assert_eq!(rows[0].context_length, Some(32000));
    let rows = parse_catalog(r#"{"data":[{"id":"m","context_window":128000}]}"#)
        .expect("the catalog parses");
    assert_eq!(rows[0].context_length, Some(128000));
    let rows = parse_catalog(r#"{"data":[{"id":"m","max_input_tokens":64000}]}"#)
        .expect("the catalog parses");
    assert_eq!(rows[0].context_length, Some(64000));
}

#[test]
fn fallback_models_name_the_other_provider() {
    let config = Config::from_toml(
        r#"
provider = "office"

[providers.office]
base_url = "x"
model = "office-model"

[providers.local]
base_url = "y"
model = "local-model"
"#,
    )
    .expect("the file parses");
    let rows = fallback_models(&config);
    assert_eq!(rows[0].id, "office-model");
    assert_eq!(rows[0].provider, None);
    let other = rows
        .iter()
        .find(|row| row.provider.as_deref() == Some("local"))
        .expect("the other table is listed");
    assert_eq!(other.id, "local-model");
}

fn a_conversation() -> Vec<Message> {
    vec![
        Message::System {
            content: "You are Kyoto Agent.".into(),
        },
        Message::User {
            content: "What is in README.md?".into(),
        },
    ]
}

/// A config pointed at the fake server, with the API key named or not.
fn config_for(server: &FakeServer, api_key_env: Option<&str>) -> Config {
    let text = match api_key_env {
        Some(name) => format!(
            "base_url = \"{base}\"\nmodel = \"test/model\"\napi_key_env = \"{name}\"\n",
            base = server.base_url()
        ),
        None => format!(
            "base_url = \"{base}\"\nmodel = \"test/model\"\n",
            base = server.base_url()
        ),
    };
    Config::from_toml(&text).expect("the test config parses")
}

fn a_text_reply() -> String {
    serde_json::json!({
        "id": "gen-1",
        "provider": "openrouter",
        "model": "test/model",
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": { "role": "assistant", "content": "It names the binary." }
        }],
        "usage": { "prompt_tokens": 11, "completion_tokens": 7 }
    })
    .to_string()
}

fn parse_sse(text: &str) -> (Reply, String) {
    let sink = Arc::new(Mutex::new(String::new()));
    let mut buf = text.as_bytes().to_vec();
    let mut partial = Partial::default();
    absorb_sse(&mut buf, &mut partial, Some(&sink)).expect("the stream parses");
    let reply = partial.finish().expect("the stream has a choice");
    let thoughts = sink.lock().expect("the thoughts buffer").clone();
    (reply, thoughts)
}

fn sse_line(value: Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&value).expect("the chunk is json")
    )
}

#[test]
fn sse_reasoning_then_content_assembles_the_reply_and_stops_at_done() {
    let mut text = String::new();
    text.push_str(&sse_line(serde_json::json!({
        "choices": [{ "delta": { "reasoning_content": "think " } }]
    })));
    text.push_str(&sse_line(serde_json::json!({
        "choices": [{ "delta": { "reasoning": "more" } }]
    })));
    text.push_str(&sse_line(serde_json::json!({
        "choices": [{ "delta": { "content": "Hello" } }],
        "usage": { "prompt_tokens": 11 }
    })));
    text.push_str(&sse_line(serde_json::json!({
        "choices": [{ "delta": { "tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": { "name": "read_file", "arguments": "{\"path\":\"" }
        }] } }]
    })));
    text.push_str(&sse_line(serde_json::json!({
        "choices": [{ "delta": { "tool_calls": [{
            "index": 0,
            "function": { "arguments": "README.md\"}" }
        }] } }]
    })));
    text.push_str("data: [DONE]\n\n");
    text.push_str(&sse_line(serde_json::json!({
        "choices": [{ "delta": { "content": "nope" } }]
    })));
    let (reply, thoughts) = parse_sse(&text);
    assert_eq!(thoughts, "think more");
    assert_eq!(reply.text(), "Hello");
    assert_eq!(reply.prompt_tokens, Some(11));
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].id, "call_1");
    assert_eq!(reply.tool_calls[0].kind, "function");
    assert_eq!(reply.tool_calls[0].name, "read_file");
    assert_eq!(reply.tool_calls[0].arguments, "{\"path\":\"README.md\"}");
}

#[tokio::test]
async fn a_single_json_object_still_decodes_when_the_server_ignores_the_stream() {
    let server = FakeServer::start(vec![Canned::Raw(a_text_reply())]);
    let client = ChatClient::new(&config_for(&server, None)).expect("the client is built");
    let reply = client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");
    assert_eq!(reply.text(), "It names the binary.");
    assert_eq!(reply.prompt_tokens, Some(11));
    assert_eq!(
        server.one().body_json()["stream"],
        Value::from(true),
        "the request still asked for a stream"
    );
}

#[tokio::test]
async fn a_named_variable_is_sent_as_a_bearer_token() {
    let name = "KYOTOAGENT_CHAT_TEST_WITH_KEY";
    std::env::set_var(name, "sk-test-value");
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let config = config_for(&server, Some(name));
    let client = ChatClient::new(&config).expect("the client is built");

    let reply = client
        .complete(&a_conversation(), &[read_file_tool()])
        .await
        .expect("the completion comes back");

    assert_eq!(reply.text(), "It names the binary.");
    assert_eq!(reply.prompt_tokens, Some(11));
    let request = server.one();
    assert_eq!(
        request.header("authorization"),
        Some("Bearer sk-test-value")
    );

    std::env::remove_var(name);
}

#[tokio::test]
async fn a_config_with_no_variable_sends_no_authorization_header() {
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let config = config_for(&server, None);
    let client = ChatClient::new(&config).expect("the client is built");

    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");

    let request = server.one();
    assert!(
        request.header("authorization").is_none(),
        "a local server is not sent a key: {:?}",
        request.headers
    );
}

#[tokio::test]
async fn both_requests_go_to_chat_completions_as_one_body() {
    let name = "KYOTOAGENT_CHAT_TEST_BODY";
    std::env::set_var(name, "sk-test-value");
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);

    for api_key_env in [Some(name), None] {
        let config = config_for(&server, api_key_env);
        let client = ChatClient::new(&config).expect("the client is built");
        client
            .complete(&a_conversation(), &[read_file_tool()])
            .await
            .expect("the completion comes back");
    }

    for request in server.received() {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.path, "/chat/completions",
            "the path is the server's, not the config's"
        );
        let body = request.body_json();
        assert_eq!(body["model"], Value::from("test/model"));
        assert_eq!(
            body["stream"],
            Value::from(true),
            "complete asks for a stream"
        );
        assert_eq!(body["tool_choice"], Value::from("auto"));
        assert_eq!(body["messages"][0]["role"], Value::from("system"));
        assert_eq!(body["messages"][1]["role"], Value::from("user"));
        assert_eq!(
            body["tools"][0]["type"],
            Value::from("function"),
            "tools use the function shape"
        );
        assert_eq!(
            body["tools"][0]["function"]["name"],
            Value::from("read_file")
        );
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["required"][0],
            Value::from("path")
        );
    }

    std::env::remove_var(name);
}

#[tokio::test]
async fn a_local_complete_omits_reasoning_effort() {
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let client = ChatClient::new(&config_for(&server, None)).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");
    let body = server.one().body_json();
    assert!(
        body.get("reasoning_effort").is_none(),
        "local servers omit the key: {body}"
    );
    assert!(
        body.get("tools").is_none(),
        "empty tools are omitted: {body}"
    );
    assert!(
        body.get("tool_choice").is_none(),
        "empty tools omit tool_choice: {body}"
    );
}

#[tokio::test]
async fn a_provider_that_takes_effort_posts_reasoning_effort() {
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let config = Config::from_toml(&format!(
            "provider = \"office\"\neffort = \"high\"\n\n[providers.office]\nbase_url = \"{base}\"\nmodel = \"office-model\"\neffort = \"high\"\n",
            base = server.base_url()
        ))
        .expect("the file parses");
    let client = ChatClient::new(&config).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");
    let body = server.one().body_json();
    assert_eq!(body["model"], Value::from("office-model"));
    assert_eq!(body["reasoning_effort"], Value::from("high"));
}

#[tokio::test]
async fn an_openrouter_public_catalog_requires_configured_credentials() {
    let configs = [
        Config::default(),
        Config::from_toml("base_url = \"https://openrouter.ai/api/v1\"\nmodel = \"m\"\napi_key_env = \"KYOTOAGENT_TEST_MISSING_OPENROUTER_KEY\"\n").unwrap(),
        Config::from_toml("provider = \"router\"\n[providers.router]\nbase_url = \"https://openrouter.ai/api/v1\"\nmodel = \"m\"\n").unwrap(),
    ];
    for config in configs {
        let server = FakeServer::start(vec![Canned::Json(
            serde_json::json!({"data": [{"id": "unconfigured-router-model"}]}).to_string(),
        )]);
        let mut client = ChatClient::new(&config).unwrap();
        client.models_url = format!("{}/models", server.base_url());
        assert!(matches!(client.catalog().await, Err(ChatError::NeedLogin)));
        assert!(server.received.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn an_authenticated_openrouter_catalog_keeps_advertised_models() {
    let name = "KYOTOAGENT_TEST_CONFIGURED_OPENROUTER_KEY";
    std::env::set_var(name, "configured-router-key");
    let server = FakeServer::start(vec![Canned::Json(
        serde_json::json!({"data": [{"id": "configured-router-model"}]}).to_string(),
    )]);
    let config = Config {
        api_key_env: Some(name.into()),
        ..Config::default()
    };
    let mut client = ChatClient::new(&config).unwrap();
    std::env::remove_var(name);
    client.models_url = format!("{}/models", server.base_url());
    let rows = client.catalog().await.unwrap();
    assert_eq!(rows[0].id, "configured-router-model");
    assert_eq!(
        server.one().header("authorization"),
        Some("Bearer configured-router-key")
    );
}

#[tokio::test]
async fn catalog_fetch_keeps_context_length_and_omits_unavailable_models() {
    let catalog = serde_json::json!({
        "data": [{ "id": "grok-4.6", "context_length": 256000 }]
    })
    .to_string();
    let server = FakeServer::start(vec![Canned::Json(catalog)]);
    let config = Config::from_toml(&format!(
            "provider = \"office\"\n\n[providers.office]\nbase_url = \"{base}\"\nmodel = \"office-model\"\n\n[providers.local]\nbase_url = \"y\"\nmodel = \"local-model\"\n",
            base = server.base_url()
        ))
        .expect("the file parses");
    let rows = list_models(&config, None).await;
    let grok = rows
        .iter()
        .find(|row| row.id == "grok-4.6")
        .expect("the catalog row is kept");
    assert_eq!(grok.context_length, Some(256000));
    assert!(!rows
        .iter()
        .any(|row| row.provider.as_deref() == Some("local")));

    let down = FakeServer::start(vec![Canned::Status(500, "no".into())]);
    let config = Config::from_toml(&format!(
            "provider = \"office\"\n\n[providers.office]\nbase_url = \"{base}\"\nmodel = \"office-model\"\n\n[providers.local]\nbase_url = \"y\"\nmodel = \"local-model\"\n",
            base = down.base_url()
        ))
        .expect("the file parses");
    let rows = list_models(&config, None).await;
    assert!(rows.is_empty());
}

#[tokio::test]
async fn a_canned_tool_call_parses_into_an_id_a_name_and_an_arguments_string() {
    let canned = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"README.md\"}"
                        }
                    },
                    {
                        "id": "call_2",
                        "type": "function",
                        "function": { "name": "list_dir", "arguments": "{\"path\":\".\"}" }
                    }
                ]
            }
        }],
        "provider_specific": { "anything": [1, 2, 3] }
    })
    .to_string();
    let server = FakeServer::start(vec![Canned::Json(canned)]);
    let client = ChatClient::new(&config_for(&server, None)).expect("the client is built");

    let reply = client
        .complete(&a_conversation(), &[read_file_tool()])
        .await
        .expect("the completion comes back");

    assert_eq!(reply.tool_calls.len(), 2, "both calls come back");
    assert_eq!(reply.tool_calls[0].id, "call_1");
    assert_eq!(reply.tool_calls[0].kind, "function");
    assert_eq!(reply.tool_calls[0].name, "read_file");
    assert_eq!(
        reply.tool_calls[0].arguments, "{\"path\":\"README.md\"}",
        "the arguments stay a string: parsing them is the loop's job"
    );
    assert_eq!(reply.tool_calls[1].id, "call_2");
    assert!(reply.wants_tools());
    assert_eq!(reply.text(), "", "a tool call has no prose");
}

#[tokio::test]
async fn the_assistant_message_and_a_tool_result_round_trip_into_the_next_request() {
    let canned = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\":\"README.md\"}" }
                }]
            }
        }]
    })
    .to_string();
    let server = FakeServer::start(vec![Canned::Json(canned)]);
    let client = ChatClient::new(&config_for(&server, None)).expect("the client is built");

    let first = client
        .complete(&a_conversation(), &[read_file_tool()])
        .await
        .expect("the completion comes back");
    let mut transcript = a_conversation();
    transcript.push(Message::assistant_reply(&first));
    for call in &first.tool_calls {
        transcript.push(Message::tool_result(&call.id, "kyotoagent is the binary."));
    }
    client
        .complete(&transcript, &[read_file_tool()])
        .await
        .expect("the second completion comes back");

    let second: Value = server.received()[1].body_json();
    let assistant = &second["messages"][2];
    assert_eq!(assistant["role"], Value::from("assistant"));
    assert_eq!(assistant["tool_calls"][0]["id"], Value::from("call_1"));
    assert_eq!(assistant["tool_calls"][0]["type"], Value::from("function"));
    assert_eq!(
        assistant["tool_calls"][0]["function"]["arguments"],
        Value::from("{\"path\":\"README.md\"}")
    );
    let tool_message = &second["messages"][3];
    assert_eq!(tool_message["role"], Value::from("tool"));
    assert_eq!(tool_message["tool_call_id"], Value::from("call_1"));
    assert_eq!(
        tool_message["content"],
        Value::from("kyotoagent is the binary.")
    );
}

#[tokio::test]
async fn a_reply_of_only_text_is_a_finished_answer() {
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let client = ChatClient::new(&config_for(&server, None)).expect("the client is built");

    let reply = client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");

    assert!(!reply.wants_tools());
    assert!(reply.tool_calls.is_empty());
    assert_eq!(reply.content.as_deref(), Some("It names the binary."));
}

#[tokio::test]
async fn a_non_2xx_answer_is_one_sentence_naming_the_status() {
    let server = FakeServer::start(vec![Canned::Status(
        500,
        "{\"error\":{\"message\":\"model is overloaded\"}}".to_string(),
    )]);
    let client = ChatClient::new(&config_for(&server, None)).expect("the client is built");

    let error = client
        .complete(&a_conversation(), &[])
        .await
        .expect_err("a 500 is not a completion");

    assert!(matches!(error, ChatError::Status { status: 500, .. }));
    let sentence = error.to_string();
    assert!(sentence.contains("500"), "{sentence}");
    assert!(sentence.contains("model is overloaded"), "{sentence}");
    // One line is what makes it a sentence the turn can put on a result
    // card without editing it.
    assert!(!sentence.contains('\n'), "one line: {sentence}");
}

#[tokio::test]
async fn a_transport_failure_is_an_error_the_caller_can_turn_into_a_result() {
    let server = FakeServer::start(vec![Canned::HangUp]);
    let client = ChatClient::new(&config_for(&server, None)).expect("the client is built");

    let error = client
        .complete(&a_conversation(), &[])
        .await
        .expect_err("a hung-up connection is not a completion");

    assert!(matches!(error, ChatError::Transport(_)), "{error:?}");
    assert!(
        error.to_string().contains("could not be reached"),
        "{error}"
    );
}

#[test]
fn a_completion_idles_for_request_timeout_secs() {
    let server = FakeServer::start(vec![Canned::HangUp]);
    let client = ChatClient::new(&config_for(&server, None)).expect("the client is built");
    assert_eq!(
        client.stream_idle,
        Duration::from_secs(REQUEST_TIMEOUT_SECS)
    );
    let named = ChatError::Idle {
        seconds: REQUEST_TIMEOUT_SECS,
    };
    assert_eq!(
        named.to_string(),
        "the model stream timed out after 120 seconds"
    );
}

#[tokio::test]
async fn a_silent_stream_times_out_and_names_the_timeout() {
    let server = FakeServer::start(vec![Canned::Silent]);
    let mut client = ChatClient::new(&config_for(&server, None)).expect("the client is built");
    client.stream_idle = Duration::from_millis(200);
    let started = std::time::Instant::now();
    let error = client
        .complete(&a_conversation(), &[])
        .await
        .expect_err("a silent body is not a completion");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the wait stayed short"
    );
    assert!(matches!(error, ChatError::Idle { .. }), "{error:?}");
    assert!(error.to_string().contains("timed out"), "{error}");
}

#[tokio::test]
async fn chunks_reset_the_idle_timer_past_the_limit() {
    let idle = Duration::from_millis(350);
    let gap = Duration::from_millis(40);
    let mut parts = Vec::new();
    for _ in 0..14 {
        parts.push(sse_line(serde_json::json!({
            "choices": [{ "delta": { "reasoning_content": "think " } }]
        })));
    }
    parts.push(sse_line(serde_json::json!({
        "choices": [{ "delta": { "content": "finished" } }]
    })));
    parts.push("data: [DONE]\n\n".to_string());
    let server = FakeServer::start(vec![Canned::Pace { gap, parts }]);
    let mut client = ChatClient::new(&config_for(&server, None)).expect("the client is built");
    client.stream_idle = idle;
    let started = std::time::Instant::now();
    let reply = client
        .complete(&a_conversation(), &[])
        .await
        .expect("a stream that keeps sending finishes");
    assert!(
        started.elapsed() >= idle,
        "the reply outlasted one idle window"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(reply.text(), "finished");
}

#[tokio::test]
async fn an_answer_with_no_choice_is_an_error_and_not_an_empty_reply() {
    let server = FakeServer::start(vec![Canned::Json("{\"choices\":[]}".to_string())]);
    let client = ChatClient::new(&config_for(&server, None)).expect("the client is built");

    let error = client
        .complete(&a_conversation(), &[])
        .await
        .expect_err("no choice is not an answer");

    assert!(matches!(error, ChatError::NoChoice), "{error:?}");
}

#[test]
fn the_client_posts_to_the_configs_chat_completions() {
    let config = Config::from_toml(
        "base_url = \"https://openrouter.ai/api/v1\"\nmodel = \"openai/gpt-4o\"\n",
    )
    .expect("the config parses");
    let client = ChatClient::new(&config).expect("the client is built");

    assert_eq!(
        client.url(),
        "https://openrouter.ai/api/v1/chat/completions"
    );
    assert_eq!(client.model(), "openai/gpt-4o");
}

#[tokio::test]
async fn a_selected_provider_posts_to_its_url_with_its_key() {
    let name = "KYOTOAGENT_CHAT_TEST_SELECTED_PROVIDER_KEY";
    std::env::set_var(name, "sk-office");
    let office = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let local = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let path = std::env::temp_dir().join(format!(
        "kyotoagent-chat-provider-{}-config.toml",
        std::process::id()
    ));
    std::fs::write(
            &path,
            format!(
                "provider = \"office\"\n\n[providers.office]\nbase_url = \"{office}\"\nmodel = \"office-model\"\napi_key_env = \"{name}\"\n\n[providers.local]\nbase_url = \"{local}\"\nmodel = \"local-model\"\n",
                office = office.base_url(),
                local = local.base_url(),
            ),
        )
        .expect("the file writes");

    let config = Config::load(&path).expect("the file loads");
    let client = ChatClient::new(&config).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the office completion comes back");
    let request = office.one();
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer sk-office"));
    assert!(
        local.received().is_empty(),
        "the unselected server is not posted to"
    );
    let body = request.body_json();
    assert_eq!(body["model"], Value::from("office-model"));

    Config::use_provider(&path, "local").expect("local is in the file");
    let config = Config::load(&path).expect("the file loads");
    let client = ChatClient::new(&config).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the local completion comes back");
    let request = local.one();
    assert_eq!(request.path, "/chat/completions");
    assert!(
        request.header("authorization").is_none(),
        "a provider without api_key_env sends no Authorization: {:?}",
        request.headers
    );
    let body = request.body_json();
    assert_eq!(body["model"], Value::from("local-model"));

    std::env::remove_var(name);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_title_completion_is_one_quiet_request() {
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let config = Config::from_toml(&format!(
        "base_url = \"{}\"\nmodel = \"test/model\"\neffort = \"high\"\n",
        server.base_url()
    ))
    .expect("the config parses");
    let client = ChatClient::new(&config).expect("the client is built");
    client
        .title("title-fast", "Add a readme line that names the binary.")
        .await
        .expect("the title comes back");
    let body = server.one().body_json();
    assert_eq!(body["model"], Value::from("title-fast"));
    assert_eq!(body["stream"], Value::from(false));
    assert!(body.get("tools").is_none(), "{body}");
    assert!(body.get("tool_choice").is_none(), "{body}");
    assert!(body.get("reasoning_effort").is_none(), "{body}");
    assert_eq!(body["messages"][0]["role"], Value::from("system"));
    assert_eq!(body["messages"][0]["content"], Value::from(TITLE_SYSTEM));
    assert_eq!(
        body["messages"][1]["content"],
        Value::from("Add a readme line that names the binary.")
    );
}

fn grok_root(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kyotoagent-chat-grok-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the root exists");
    dir
}

fn grok_file(server: &FakeServer, api_key_env: Option<&str>) -> String {
    let key = match api_key_env {
        Some(name) => format!("api_key_env = \"{name}\"\n"),
        None => String::new(),
    };
    format!(
        "provider = \"grok\"\n\n[providers.grok]\nbase_url = \"{}\"\nmodel = \"grok-4.6\"\n{key}",
        server.base_url()
    )
}

#[tokio::test]
async fn a_grok_provider_sends_the_access_token_and_ignores_api_key_env() {
    let name = "KYOTOAGENT_CHAT_TEST_GROK_IGNORED_KEY";
    std::env::set_var(name, "sk-should-not-be-sent");
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let root = grok_root("bearer");
    std::fs::write(root.join("config.toml"), grok_file(&server, Some(name)))
        .expect("the config writes");
    crate::auth::write_tokens(
        &root.join(crate::auth::AUTH_FILE),
        &crate::auth::Tokens {
            access_token: "grok-access".into(),
            refresh_token: "grok-refresh".into(),
            expires_at: "2035-01-01T00:00:00.000Z".into(),
        },
    )
    .expect("the session writes");
    let decoy = root.join(".grok");
    std::fs::create_dir_all(&decoy).expect("the decoy dir exists");
    std::fs::write(
        decoy.join("auth.json"),
        "{\"access_token\":\"from-grok-home\"}",
    )
    .expect("the decoy writes");
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let client = ChatClient::in_root(&config, Some(&root)).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");
    let request = server.one();
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer grok-access"));
    std::env::remove_var(name);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_grok_provider_without_auth_json_says_to_log_in() {
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let root = grok_root("missing");
    std::fs::write(root.join("config.toml"), grok_file(&server, None)).expect("the config writes");
    let decoy = root.join(".grok");
    std::fs::create_dir_all(&decoy).expect("the decoy dir exists");
    std::fs::write(
        decoy.join("auth.json"),
        "{\"access_token\":\"from-grok-home\"}",
    )
    .expect("the decoy writes");
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let client = ChatClient::in_root(&config, Some(&root)).expect("the client is built");
    let error = client
        .complete(&a_conversation(), &[])
        .await
        .expect_err("missing auth is not a completion");
    assert!(matches!(error, ChatError::NeedLogin), "{error:?}");
    assert_eq!(
        error.to_string(),
        "Open Providers in Kyoto Agent to sign in or enter an API key."
    );
    assert!(
        server.received().is_empty(),
        "no chat request is sent without a session"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_401_refreshes_and_retries_with_the_new_access_token() {
    let chat = FakeServer::start(vec![
        Canned::Status(401, "expired".into()),
        Canned::Json(a_text_reply()),
    ]);
    let token = FakeServer::start(vec![Canned::Json(
        serde_json::json!({
            "access_token": "access-2",
            "refresh_token": "refresh-2",
            "expires_in": 3600
        })
        .to_string(),
    )]);
    let root = grok_root("401");
    std::fs::write(root.join("config.toml"), grok_file(&chat, None)).expect("the config writes");
    crate::auth::write_tokens(
        &root.join(crate::auth::AUTH_FILE),
        &crate::auth::Tokens {
            access_token: "access-1".into(),
            refresh_token: "refresh-1".into(),
            expires_at: "2035-01-01T00:00:00.000Z".into(),
        },
    )
    .expect("the session writes");
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let auth = crate::auth::AuthClient::at(
        &format!("{}/oauth2/device/code", token.base_url()),
        &format!("{}/oauth2/token", token.base_url()),
        crate::auth::DEFAULT_CLIENT_ID,
    );
    let client =
        ChatClient::in_root_with_auth(&config, Some(&root), auth).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the retry comes back");
    let requests = chat.received();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("authorization"), Some("Bearer access-1"));
    assert_eq!(requests[1].header("authorization"), Some("Bearer access-2"));
    let stored = crate::auth::load(&root.join(crate::auth::AUTH_FILE)).expect("the file loads");
    assert_eq!(stored.refresh_token, "refresh-2");
    let token_req = token.one();
    assert_eq!(token_req.path, "/oauth2/token");
    assert!(
        token_req.body.contains("refresh_token=refresh-1"),
        "{}",
        token_req.body
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_expired_session_refreshes_before_the_chat_post() {
    let chat = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let token = FakeServer::start(vec![Canned::Json(
        serde_json::json!({
            "access_token": "access-2",
            "refresh_token": "refresh-2",
            "expires_in": 3600
        })
        .to_string(),
    )]);
    let root = grok_root("before-expiry");
    std::fs::write(root.join("config.toml"), grok_file(&chat, None)).expect("the config writes");
    crate::auth::write_tokens(
        &root.join(crate::auth::AUTH_FILE),
        &crate::auth::Tokens {
            access_token: "access-1".into(),
            refresh_token: "refresh-1".into(),
            expires_at: "2020-01-01T00:00:00.000Z".into(),
        },
    )
    .expect("the session writes");
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let auth = crate::auth::AuthClient::at(
        &format!("{}/oauth2/device/code", token.base_url()),
        &format!("{}/oauth2/token", token.base_url()),
        crate::auth::DEFAULT_CLIENT_ID,
    );
    let client =
        ChatClient::in_root_with_auth(&config, Some(&root), auth).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");
    let request = chat.one();
    assert_eq!(request.header("authorization"), Some("Bearer access-2"));
    let stored = crate::auth::load(&root.join(crate::auth::AUTH_FILE)).expect("the file loads");
    assert_eq!(stored.refresh_token, "refresh-2");
    let _ = std::fs::remove_dir_all(&root);
}

fn codex_file(server: &FakeServer) -> String {
    format!(
        "provider = \"codex\"\n\n[providers.codex]\nkind = \"codex\"\nbase_url = \"{}\"\nmodel = \"gpt-6.1-sol\"\n",
        server.base_url()
    )
}

fn write_codex(root: &std::path::Path, access: &str, refresh: &str, expires: &str) {
    crate::auth::write_codex_tokens(
        &root.join(crate::auth::CODEX_AUTH_FILE),
        &crate::auth::CodexTokens {
            access_token: access.into(),
            refresh_token: refresh.into(),
            id_token: "id-1".into(),
            account_id: "acc-1".into(),
            expires_at: expires.into(),
        },
    )
    .expect("the session writes");
}

#[tokio::test]
async fn a_codex_catalog_requests_a_supported_version_and_keeps_visible_models() {
    let server = FakeServer::start(vec![Canned::Json(serde_json::json!({
        "models": [
            {"slug": "gpt-6.1-sol", "visibility": "list", "supported_in_api": true,
             "context_window": 272000, "max_context_window": 872000,
             "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}, {"effort": "ultra"}]},
            {"slug": "codex-only", "visibility": "list", "supported_in_api": false},
            {"slug": "internal-review", "visibility": "hide", "supported_in_api": true},
            {"slug": "deprecated", "visibility": "unlisted"},
            {"slug": "missing-visibility"},
            {"slug": "", "visibility": "list"}
        ]
    }).to_string())]);
    let root = grok_root("codex-catalog");
    std::fs::write(root.join("config.toml"), codex_file(&server)).unwrap();
    write_codex(&root, "access-1", "refresh-1", "2035-01-01T00:00:00.000Z");
    let config = Config::load(&root.join("config.toml")).unwrap();
    let client = ChatClient::in_root(&config, Some(&root)).unwrap();
    let rows = client.catalog().await.expect("the Codex catalog parses");
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["gpt-6.1-sol", "codex-only"]
    );
    assert_eq!(rows[0].reasoning_efforts, ["low", "high", "ultra"]);
    assert_eq!(rows[0].context_length, Some(272000));
    let request = server.one();
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/models?client_version=0.160.0");
    assert_eq!(request.header("authorization"), Some("Bearer access-1"));
    assert_eq!(request.header("chatgpt-account-id"), Some("acc-1"));
    std::fs::remove_dir_all(root).unwrap();
}

fn plant_codex_decoy(root: &std::path::Path) -> Vec<u8> {
    let decoy = root.join(".codex").join("auth.json");
    std::fs::create_dir_all(decoy.parent().expect("the decoy parent")).expect("the decoy dir");
    let canary = "{\"tokens\":{\"access_token\":\"canary-token-value\",\"refresh_token\":\"decoy-refresh\"}}";
    std::fs::write(&decoy, canary).expect("the decoy writes");
    std::fs::read(&decoy).expect("the decoy reads")
}

fn a_responses_reply() -> String {
    serde_json::json!({
        "output": [
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "Reading." }]
            },
            {
                "type": "function_call",
                "call_id": "call-1",
                "name": "read_file",
                "arguments": "{\"path\":\"a\"}"
            }
        ],
        "usage": { "input_tokens": 9 }
    })
    .to_string()
}

fn codex_messages() -> Vec<Message> {
    let mut messages = a_conversation();
    messages.push(Message::Assistant {
        content: None,
        tool_calls: vec![ToolCall {
            id: "call-0".into(),
            kind: "function".into(),
            name: "read_file".into(),
            arguments: "{\"path\":\"b\"}".into(),
        }],
    });
    messages.push(Message::tool_result("call-0", "file text"));
    messages
}

#[tokio::test]
async fn a_codex_turn_posts_responses_and_maps_the_tool_call() {
    let server = FakeServer::start(vec![Canned::Json(a_responses_reply())]);
    let root = grok_root("codex-turn");
    std::fs::write(root.join("config.toml"), codex_file(&server)).expect("the config writes");
    write_codex(&root, "access-1", "refresh-1", "2035-01-01T00:00:00.000Z");
    let before = plant_codex_decoy(&root);
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let client = ChatClient::in_root(&config, Some(&root)).expect("the client is built");
    let reply = client
        .complete(
            &codex_messages(),
            &[Tool::new(
                "read_file",
                "Read a file",
                serde_json::json!({"type": "object"}),
            )],
        )
        .await
        .expect("the completion comes back");
    assert_eq!(reply.text(), "Reading.");
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].id, "call-1");
    assert_eq!(reply.tool_calls[0].kind, "function");
    assert_eq!(reply.tool_calls[0].name, "read_file");
    assert_eq!(reply.tool_calls[0].arguments, "{\"path\":\"a\"}");
    assert_eq!(reply.prompt_tokens, Some(9));
    let request = server.one();
    assert_eq!(request.path, "/responses");
    assert_eq!(request.method, "POST");
    assert_eq!(request.header("authorization"), Some("Bearer access-1"));
    assert_eq!(request.header("chatgpt-account-id"), Some("acc-1"));
    assert_eq!(request.header("originator"), Some("codex_cli_rs"));
    assert_eq!(request.header("version"), Some("0.160.0"));
    let body = request.body_json();
    assert_eq!(body["model"], Value::from("gpt-6.1-sol"));
    assert_eq!(body["stream"], Value::from(true));
    assert_eq!(body["store"], Value::from(false));
    assert_eq!(body["instructions"], Value::from("You are Kyoto Agent."));
    assert_eq!(body["tools"][0]["type"], Value::from("function"));
    assert_eq!(body["tools"][0]["name"], Value::from("read_file"));
    assert_eq!(body["tools"][0]["strict"], Value::Bool(false));
    assert!(body["tools"][0].get("function").is_none(), "{body}");
    assert_eq!(body["input"][1]["type"], Value::from("function_call"));
    assert_eq!(body["input"][1]["call_id"], Value::from("call-0"));
    assert_eq!(
        body["input"][2]["type"],
        Value::from("function_call_output")
    );
    assert_eq!(body["input"][2]["call_id"], Value::from("call-0"));
    assert_eq!(body["input"][2]["output"], Value::from("file text"));
    assert!(!request.body.contains("canary-token-value"));
    assert!(!request.body.contains("decoy-refresh"));
    assert_eq!(
        std::fs::read(root.join(".codex").join("auth.json")).expect("the decoy reads"),
        before
    );
    let written = std::fs::read_to_string(root.join("config.toml")).expect("the config reads");
    assert!(!written.contains("access-1"));
    assert!(!written.contains("canary-token-value"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_codex_401_refreshes_with_the_stored_refresh_token() {
    let chat = FakeServer::start(vec![
        Canned::Status(401, "expired".into()),
        Canned::Json(a_responses_reply()),
    ]);
    let token = FakeServer::start(vec![Canned::Json(
        serde_json::json!({
            "access_token": "access-2",
            "expires_in": 3600
        })
        .to_string(),
    )]);
    let root = grok_root("codex-401");
    std::fs::write(root.join("config.toml"), codex_file(&chat)).expect("the config writes");
    write_codex(&root, "access-1", "refresh-1", "2035-01-01T00:00:00.000Z");
    let before = plant_codex_decoy(&root);
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let auth = crate::auth::CodexAuth::at(&token.base_url());
    let client =
        ChatClient::in_root_with_codex(&config, Some(&root), auth).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the retry comes back");
    let requests = chat.received();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/responses");
    assert_eq!(requests[0].header("authorization"), Some("Bearer access-1"));
    assert_eq!(requests[1].header("authorization"), Some("Bearer access-2"));
    assert_eq!(requests[1].header("chatgpt-account-id"), Some("acc-1"));
    let token_req = token.one();
    assert_eq!(token_req.path, "/oauth/token");
    let payload = token_req.body_json();
    assert_eq!(payload["grant_type"], Value::from("refresh_token"));
    assert_eq!(payload["refresh_token"], Value::from("refresh-1"));
    assert_eq!(
        payload["client_id"],
        Value::from(crate::auth::CODEX_CLIENT_ID)
    );
    assert!(!token_req.body.contains("decoy-refresh"));
    let stored = crate::auth::load_codex(&root.join(crate::auth::CODEX_AUTH_FILE)).expect("loads");
    assert_eq!(stored.access_token, "access-2");
    assert_eq!(stored.refresh_token, "refresh-1");
    assert_eq!(stored.account_id, "acc-1");
    assert_eq!(
        std::fs::read(root.join(".codex").join("auth.json")).expect("the decoy reads"),
        before
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_expired_codex_session_refreshes_before_the_post() {
    let chat = FakeServer::start(vec![Canned::Json(a_responses_reply())]);
    let token = FakeServer::start(vec![Canned::Json(
        serde_json::json!({
            "access_token": "access-2",
            "refresh_token": "refresh-2",
            "expires_in": 3600
        })
        .to_string(),
    )]);
    let root = grok_root("codex-skew");
    std::fs::write(root.join("config.toml"), codex_file(&chat)).expect("the config writes");
    write_codex(&root, "access-1", "refresh-1", "2020-01-01T00:00:00.000Z");
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let auth = crate::auth::CodexAuth::at(&token.base_url());
    let client =
        ChatClient::in_root_with_codex(&config, Some(&root), auth).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");
    let request = chat.one();
    assert_eq!(request.path, "/responses");
    assert_eq!(request.header("authorization"), Some("Bearer access-2"));
    let stored = crate::auth::load_codex(&root.join(crate::auth::CODEX_AUTH_FILE)).expect("loads");
    assert_eq!(stored.refresh_token, "refresh-2");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_codex_title_posts_responses_without_tools() {
    let server = FakeServer::start(vec![Canned::Json(
        serde_json::json!({
            "output": [{
                "type": "message",
                "content": [{ "type": "output_text", "text": "Name the binary" }]
            }]
        })
        .to_string(),
    )]);
    let root = grok_root("codex-title");
    std::fs::write(root.join("config.toml"), codex_file(&server)).expect("the config writes");
    write_codex(&root, "access-1", "refresh-1", "2035-01-01T00:00:00.000Z");
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let client = ChatClient::in_root(&config, Some(&root)).expect("the client is built");
    let reply = client
        .title("title-fast", "Add a readme line")
        .await
        .expect("the title comes back");
    assert_eq!(reply.text(), "Name the binary");
    let body = server.one().body_json();
    assert_eq!(body["model"], Value::from("title-fast"));
    assert_eq!(body["stream"], Value::from(true));
    assert!(body.get("tools").is_none(), "{body}");
    assert!(body.get("messages").is_none(), "{body}");
    assert_eq!(body["instructions"], Value::from(TITLE_SYSTEM));
    let _ = std::fs::remove_dir_all(&root);
}

fn limited(delay: u64) -> Canned {
    Canned::Status(429, serde_json::json!({"error": {"message": "Credit check timed out. Retry shortly.", "metadata": {"headers": {"Retry-After": delay.to_string()}}}}).to_string())
}
#[tokio::test]
async fn a_429_retries_the_same_request_then_succeeds() {
    let server = FakeServer::start(vec![limited(0), Canned::Json(a_text_reply())]);
    let client = ChatClient::new(&config_for(&server, None)).unwrap();
    let status = Arc::new(Mutex::new(None));
    let reply = client
        .complete_with_status(&a_conversation(), &[], None, Some(&status))
        .await
        .unwrap();
    assert_eq!(reply.text(), "It names the binary.");
    let requests = server.received.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
    assert!(status.lock().unwrap().is_none());
}
#[tokio::test]
async fn repeated_429s_stop_after_three_retries_with_a_readable_error() {
    let server = FakeServer::start(vec![limited(0); 4]);
    let client = ChatClient::new(&config_for(&server, None)).unwrap();
    let error = client
        .complete(&a_conversation(), &[])
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Credit check timed out"));
    assert!(!error.contains("metadata"));
    assert_eq!(server.received.lock().unwrap().len(), 4);
}
#[tokio::test]
async fn a_server_delay_beyond_the_budget_is_not_retried_early() {
    let server = FakeServer::start(vec![limited(121)]);
    let client = ChatClient::new(&config_for(&server, None)).unwrap();
    assert!(client.complete(&a_conversation(), &[]).await.is_err());
    assert_eq!(server.received.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn cancellation_clears_retry_status_and_does_not_send_again() {
    let server = FakeServer::start(vec![limited(10)]);
    let client = ChatClient::new(&config_for(&server, None)).unwrap();
    let status = Arc::new(Mutex::new(None));
    let result = tokio::time::timeout(
        Duration::from_millis(150),
        client.complete_with_status(&a_conversation(), &[], None, Some(&status)),
    )
    .await;
    assert!(result.is_err());
    assert!(status.lock().unwrap().is_none());
    assert_eq!(server.received.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn permanent_billing_errors_are_not_retried() {
    let body = r#"{"error":{"message":"Insufficient credits"}}"#;
    let server = FakeServer::start(vec![Canned::Status(429, body.into())]);
    let client = ChatClient::new(&config_for(&server, None)).unwrap();
    assert!(client.complete(&a_conversation(), &[]).await.is_err());
    assert_eq!(server.received.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn independent_clients_observe_the_same_account_cooldown() {
    let server = FakeServer::start(vec![limited(10)]);
    let config = config_for(&server, None);
    let first = ChatClient::new(&config).unwrap();
    let second = ChatClient::new(&config).unwrap();
    let _ = tokio::time::timeout(
        Duration::from_millis(150),
        first.complete(&a_conversation(), &[]),
    )
    .await;
    let status = Arc::new(Mutex::new(None));
    let result = tokio::time::timeout(
        Duration::from_millis(100),
        second.complete_with_status(&a_conversation(), &[], None, Some(&status)),
    )
    .await;
    assert!(result.is_err());
    assert!(status.lock().unwrap().is_none());
    assert_eq!(server.received.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn background_titles_do_not_amplify_429s() {
    let server = FakeServer::start(vec![limited(10)]);
    let client = ChatClient::new(&config_for(&server, None)).unwrap();
    assert!(client.title("test", "hello").await.is_err());
    assert!(client.title("test", "hello again").await.is_err());
    assert_eq!(server.received.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_partial_stream_timeout_is_not_replayed() {
    let part = sse_line(serde_json::json!({"choices": [{"delta": {"content": "partial"}}]}));
    let server = FakeServer::start(vec![Canned::Pace {
        gap: Duration::from_millis(200),
        parts: vec![part, "data: [DONE]\n\n".into()],
    }]);
    let mut client = ChatClient::new(&config_for(&server, None)).unwrap();
    client.stream_idle = Duration::from_millis(30);
    assert!(client.complete(&a_conversation(), &[]).await.is_err());
    assert_eq!(server.received.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn compact_recovers_from_a_rejected_request() {
    let server = FakeServer::start(vec![limited(0), Canned::Json(a_text_reply())]);
    let client = ChatClient::new(&config_for(&server, None)).unwrap();
    assert!(client.compact(&a_conversation()).await.is_ok());
    assert_eq!(server.received.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn auth_and_payment_errors_are_not_retried() {
    for code in [400, 401, 402, 403] {
        let server = FakeServer::start(vec![Canned::Status(code, "not retryable".into())]);
        let client = ChatClient::new(&config_for(&server, None)).unwrap();
        assert!(client.complete(&a_conversation(), &[]).await.is_err());
        assert_eq!(server.received.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn retry_status_is_visible_during_wait_and_clears_on_drop() {
    let server = FakeServer::start(vec![limited(10)]);
    let client = ChatClient::new(&config_for(&server, None)).unwrap();
    let status = Arc::new(Mutex::new(None));
    let messages = a_conversation();
    let mut request = Box::pin(client.complete_with_status(&messages, &[], None, Some(&status)));
    tokio::select! { _ = &mut request => panic!("should wait"), _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
    assert!(status
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|s| s.starts_with("Provider busy. Retrying in ")));
    drop(request);
    assert!(status.lock().unwrap().is_none());
}

fn opencode_file(server: &FakeServer, model: &str, api_key_env: Option<&str>) -> String {
    let key = match api_key_env {
        Some(name) => format!("api_key_env = \"{name}\"\n"),
        None => String::new(),
    };
    format!(
        "provider = \"opencode\"\n\n[providers.opencode]\nkind = \"opencode\"\nbase_url = \"{}\"\nmodel = \"{model}\"\n{key}",
        server.base_url()
    )
}

fn write_opencode_key(root: &std::path::Path, key: &str) {
    crate::auth::write_opencode_key(&root.join(crate::auth::OPENCODE_AUTH_FILE), key)
        .expect("the opencode key writes");
}

fn plant_opencode_decoy(root: &std::path::Path) -> Vec<u8> {
    let decoy = root
        .join(".local")
        .join("share")
        .join("opencode")
        .join("auth.json");
    std::fs::create_dir_all(decoy.parent().expect("the decoy parent")).expect("the decoy dir");
    let bytes = b"{\"api_key\":\"decoy-opencode-cli-key\"}".to_vec();
    std::fs::write(&decoy, &bytes).expect("the decoy writes");
    bytes
}

fn assert_no_codex_account(request: &Received) {
    assert!(request.header("chatgpt-account-id").is_none());
    assert!(request.header("originator").is_none());
}

#[tokio::test]
async fn an_opencode_chat_model_posts_completions_with_the_file_key() {
    let key = "oc-file-key";
    std::env::set_var("OPENCODE_API_KEY", "stale-opencode-env");
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let root = grok_root("opencode-chat");
    std::fs::write(
        root.join("config.toml"),
        opencode_file(&server, "kimi-k2.7-code", None),
    )
    .expect("the config writes");
    write_opencode_key(&root, key);
    write_codex(
        &root,
        "codex-access",
        "codex-refresh",
        "2035-01-01T00:00:00.000Z",
    );
    let decoy = plant_opencode_decoy(&root);
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let client = ChatClient::in_root(&config, Some(&root)).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");
    let request = server.one();
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(request.method, "POST");
    assert_eq!(request.header("authorization"), Some("Bearer oc-file-key"));
    assert_no_codex_account(&request);
    assert!(!request.body.contains("stale-opencode-env"));
    assert!(!request.body.contains("decoy-opencode-cli-key"));
    assert!(!request.body.contains("codex-access"));
    let body = request.body_json();
    assert_eq!(body["model"], Value::from("kimi-k2.7-code"));
    assert!(body.get("input").is_none(), "{body}");
    assert_eq!(
        std::fs::read(
            root.join(".local")
                .join("share")
                .join("opencode")
                .join("auth.json")
        )
        .expect("the decoy reads"),
        decoy
    );
    let written = std::fs::read_to_string(root.join("config.toml")).expect("the config reads");
    assert!(!written.contains(key));
    std::env::remove_var("OPENCODE_API_KEY");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_opencode_gpt_model_posts_responses_without_the_codex_account() {
    let server = FakeServer::start(vec![Canned::Json(a_responses_reply())]);
    let root = grok_root("opencode-responses");
    std::fs::write(
        root.join("config.toml"),
        opencode_file(&server, "gpt-6.1-sol", None),
    )
    .expect("the config writes");
    write_opencode_key(&root, "oc-responses-key");
    write_codex(
        &root,
        "codex-access",
        "codex-refresh",
        "2035-01-01T00:00:00.000Z",
    );
    let decoy = plant_opencode_decoy(&root);
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    assert!(config.chat_url().ends_with("/responses"));
    let client = ChatClient::in_root(&config, Some(&root)).expect("the client is built");
    let reply = client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");
    assert_eq!(reply.text(), "Reading.");
    let request = server.one();
    assert_eq!(request.path, "/responses");
    assert_eq!(
        request.header("authorization"),
        Some("Bearer oc-responses-key")
    );
    assert_no_codex_account(&request);
    let body = request.body_json();
    assert_eq!(body["model"], Value::from("gpt-6.1-sol"));
    assert!(body.get("messages").is_none(), "{body}");
    assert_eq!(body["input"][0]["type"], Value::from("message"));
    assert!(!request.body.contains("codex-access"));
    assert!(!request.body.contains("decoy-opencode-cli-key"));
    assert_eq!(
        std::fs::read(
            root.join(".local")
                .join("share")
                .join("opencode")
                .join("auth.json")
        )
        .expect("the decoy reads"),
        decoy
    );
    let codex =
        std::fs::read_to_string(root.join(crate::auth::CODEX_AUTH_FILE)).expect("codex stays");
    assert!(codex.contains("codex-access"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_opencode_env_key_is_sent_and_the_auth_file_is_unread() {
    let name = "KYOTOAGENT_CHAT_TEST_OPENCODE_ENV";
    std::env::set_var(name, "oc-env-key");
    let server = FakeServer::start(vec![Canned::Json(a_text_reply())]);
    let root = grok_root("opencode-env");
    std::fs::write(
        root.join("config.toml"),
        opencode_file(&server, "kimi-k2.7-code", Some(name)),
    )
    .expect("the config writes");
    write_opencode_key(&root, "oc-file-should-stay");
    let before = std::fs::read(root.join(crate::auth::OPENCODE_AUTH_FILE)).expect("the file reads");
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let client = ChatClient::in_root(&config, Some(&root)).expect("the client is built");
    client
        .complete(&a_conversation(), &[])
        .await
        .expect("the completion comes back");
    let request = server.one();
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer oc-env-key"));
    assert!(!request.body.contains("oc-file-should-stay"));
    assert_eq!(
        std::fs::read(root.join(crate::auth::OPENCODE_AUTH_FILE)).expect("the file stays"),
        before
    );
    std::env::remove_var(name);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_opencode_picker_drops_models_the_client_cannot_call() {
    let catalog = serde_json::json!({
        "data": [
            {"id": "kimi-k2.7-code"},
            {"id": "gpt-6.1-sol"},
            {"id": "grok-4"},
            {"id": "muse-spark-1"},
            {"id": "qwen3.8-max"},
            {"id": "qwen3.6"},
            {"id": "claude-sonnet-5"},
            {"id": "gemini-3-flash"},
            {"id": "jev-1.13"},
            {"id": "big-pickle"}
        ]
    })
    .to_string();
    let server = FakeServer::start(vec![Canned::Json(catalog)]);
    let root = grok_root("opencode-picker");
    std::fs::write(
        root.join("config.toml"),
        opencode_file(&server, "kimi-k2.7-code", None),
    )
    .expect("the config writes");
    write_opencode_key(&root, "oc-picker-key");
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let rows = list_models(&config, Some(&root)).await;
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert!(ids.contains(&"kimi-k2.7-code"));
    assert!(ids.contains(&"gpt-6.1-sol"));
    assert!(ids.contains(&"grok-4"));
    assert!(ids.contains(&"muse-spark-1"));
    assert!(ids.contains(&"qwen3.8-max"));
    assert!(ids.contains(&"big-pickle"));
    assert!(!ids.contains(&"claude-sonnet-5"));
    assert!(!ids.contains(&"gemini-3-flash"));
    assert!(!ids.contains(&"jev-1.13"));
    assert!(!ids.contains(&"qwen3.6"));
    let request = server.one();
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/models");
    assert_eq!(
        request.header("authorization"),
        Some("Bearer oc-picker-key")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_failed_opencode_model_list_does_not_offer_unavailable_models() {
    let server = FakeServer::start(vec![Canned::Status(500, "down".into())]);
    let root = grok_root("opencode-picker-down");
    std::fs::write(
        root.join("config.toml"),
        opencode_file(&server, "kimi-k2.7-code", None),
    )
    .expect("the config writes");
    write_opencode_key(&root, "oc-picker-key");
    let config = Config::load(&root.join("config.toml")).expect("the config loads");
    let rows = list_models(&config, Some(&root)).await;
    assert!(rows.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn every_provider_offers_only_its_advertised_models() {
    let first = FakeServer::start(vec![Canned::Json(
        serde_json::json!({"data": [{"id": "first-live"}, {"id": "first-live"}]}).to_string(),
    )]);
    let second = FakeServer::start(vec![Canned::Json(
        serde_json::json!({"data": [{"id": "second-live"}]}).to_string(),
    )]);
    let config = Config::from_toml(&format!("provider = \"first\"\n[providers.first]\nbase_url = \"{}\"\nmodel = \"first-unavailable\"\n[providers.second]\nbase_url = \"{}\"\nmodel = \"second-unavailable\"\n", first.base_url(), second.base_url())).unwrap();
    let rows = list_models(&config, None).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "first-live");
    assert!(rows[0].provider.is_none());
    assert_eq!(rows[1].id, "second-live");
    assert_eq!(rows[1].provider.as_deref(), Some("second"));
    assert_eq!(first.one().method, "GET");
    assert_eq!(second.one().method, "GET");
}

#[tokio::test]
async fn mixed_codex_and_openai_catalogs_report_failures_without_inventing_models() {
    for (index, response) in [
        Canned::Json(serde_json::json!({"data":[{"id":"claude-opus-5-5"}]}).to_string()),
        Canned::Status(503, "private-provider-error-secret".into()),
        Canned::Json("{\"data\":[]}".into()),
    ]
    .into_iter()
    .enumerate()
    {
        let codex = FakeServer::start(vec![Canned::Json(
            serde_json::json!({"models":[{"slug":"gpt-6.1-sol","visibility":"list"}]}).to_string(),
        )]);
        let meridian = FakeServer::start(vec![response]);
        let root = grok_root(&format!("mixed-catalog-{index}"));
        write_codex(&root, "codex-access", "refresh", "2035-01-01T00:00:00.000Z");
        let config = Config::from_toml(&format!(
            "{}\n[providers.meridian]\nbase_url = \"{}\"\nmodel = \"configured-unavailable\"\n",
            codex_file(&codex),
            meridian.base_url()
        ))
        .unwrap();
        let catalog = model_catalog(&config, Some(&root)).await;
        assert_eq!(catalog.models[0].id, "gpt-6.1-sol");
        assert!(catalog.models[0].provider.is_none());
        if index == 0 {
            assert_eq!(catalog.models.len(), 2);
            assert_eq!(catalog.models[1].id, "claude-opus-5-5");
            assert_eq!(catalog.models[1].provider.as_deref(), Some("meridian"));
            assert!(catalog.errors.is_empty());
        } else {
            assert_eq!(catalog.models.len(), 1);
            assert_eq!(catalog.errors.len(), 1);
            assert!(catalog.errors["meridian"].contains(if index == 1 {
                "503"
            } else {
                "no usable models"
            }));
            assert!(!serde_json::to_string(&catalog).unwrap().contains("secret"));
        }
        assert_eq!(
            codex.one().header("authorization"),
            Some("Bearer codex-access")
        );
        assert_eq!(meridian.one().header("authorization"), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn responses_stream(output: Value) -> String {
    sse_line(
        serde_json::json!({"type": "response.reasoning_summary_text.delta", "delta": "Checking."}),
    ) + &sse_line(serde_json::json!({"type": "response.output_text.delta", "delta": "Reading."}))
        + &sse_line(serde_json::json!({"type": "response.completed", "response": output}))
}

#[tokio::test]
async fn codex_streams_preserve_tools_text_usage_and_reasoning() {
    let output: Value = serde_json::from_str(&a_responses_reply()).unwrap();
    let server = FakeServer::start(vec![Canned::Raw(responses_stream(output))]);
    let root = grok_root("codex-stream");
    std::fs::write(root.join("config.toml"), codex_file(&server)).unwrap();
    write_codex(&root, "access", "refresh", "2035-01-01T00:00:00.000Z");
    let config = Config::load(&root.join("config.toml")).unwrap();
    let client = ChatClient::in_root(&config, Some(&root)).unwrap();
    let thoughts = Arc::new(Mutex::new(String::new()));
    let reply = client
        .complete_showing(&codex_messages(), &[read_file_tool()], Some(&thoughts))
        .await
        .unwrap();
    assert_eq!(reply.text(), "Reading.");
    assert_eq!(reply.tool_calls[0].id, "call-1");
    assert_eq!(reply.tool_calls[0].name, "read_file");
    assert_eq!(reply.tool_calls[0].arguments, "{\"path\":\"a\"}");
    assert_eq!(reply.prompt_tokens, Some(9));
    assert_eq!(*thoughts.lock().unwrap(), "Checking.");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn codex_quiet_requests_read_completed_streams() {
    let response = serde_json::json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": "Done"}]}]});
    let server = FakeServer::start(vec![Canned::Raw(responses_stream(response)); 3]);
    let root = grok_root("codex-quiet-stream");
    std::fs::write(root.join("config.toml"), codex_file(&server)).unwrap();
    write_codex(&root, "access", "refresh", "2035-01-01T00:00:00.000Z");
    let config = Config::load(&root.join("config.toml")).unwrap();
    let client = ChatClient::in_root(&config, Some(&root)).unwrap();
    assert_eq!(
        client
            .title("gpt-6-sol", "Title this")
            .await
            .unwrap()
            .text(),
        "Done"
    );
    assert_eq!(
        client
            .rewrite("gpt-6-sol", &a_conversation())
            .await
            .unwrap()
            .text(),
        "Done"
    );
    assert_eq!(
        client.compact(&a_conversation()).await.unwrap().text(),
        "Done"
    );
    for request in server.received() {
        assert_eq!(request.body_json()["stream"], true);
        assert_eq!(request.header("version"), Some("0.160.0"));
    }
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn codex_failed_and_incomplete_streams_are_errors() {
    for event in [
        serde_json::json!({"type":"response.failed", "response":{"error":{"message":"model unavailable"}}}),
        serde_json::json!({"type":"response.incomplete", "response":{"incomplete_details":{"reason":"max_output_tokens"}}}),
        serde_json::json!({"type":"error", "message":"invalid model"}),
        serde_json::json!({"type":"response.completed", "response": []}),
        serde_json::json!({"type":"response.completed", "response": "invalid"}),
        serde_json::json!({"type":"response.output_text.delta", "delta":"unfinished"}),
    ] {
        let server = FakeServer::start(vec![Canned::Raw(sse_line(event))]);
        let root = grok_root("codex-stream-error");
        std::fs::write(root.join("config.toml"), codex_file(&server)).unwrap();
        write_codex(&root, "access", "refresh", "2035-01-01T00:00:00.000Z");
        let config = Config::load(&root.join("config.toml")).unwrap();
        let client = ChatClient::in_root(&config, Some(&root)).unwrap();
        assert!(client.complete(&a_conversation(), &[]).await.is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}

#[tokio::test]
async fn codex_completed_streams_keep_output_items_when_the_final_output_is_empty() {
    let output: Value = serde_json::from_str(&a_responses_reply()).unwrap();
    let mut stream = String::new();
    for (index, item) in output["output"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .rev()
    {
        stream.push_str(&sse_line(serde_json::json!({"type":"response.output_item.done","output_index":index,"item":item})));
    }
    stream.push_str(&sse_line(serde_json::json!({"type":"response.completed","response":{"output":[],"usage":{"input_tokens":9,"output_tokens":17}}})));
    let parts = stream
        .as_bytes()
        .chunks(7)
        .map(|part| String::from_utf8(part.to_vec()).unwrap())
        .collect();
    let server = FakeServer::start(vec![Canned::Pace {
        gap: Duration::ZERO,
        parts,
    }]);
    let root = grok_root("codex-empty-final-output");
    std::fs::write(root.join("config.toml"), codex_file(&server)).unwrap();
    write_codex(&root, "access", "refresh", "2035-01-01T00:00:00.000Z");
    let config = Config::load(&root.join("config.toml")).unwrap();
    let client = ChatClient::in_root(&config, Some(&root)).unwrap();
    let reply = client.complete(&a_conversation(), &[]).await.unwrap();
    assert_eq!(reply.text(), "Reading.");
    assert_eq!(reply.tool_calls[0].name, "read_file");
    assert_eq!(reply.tool_calls[0].arguments, "{\"path\":\"a\"}");
    assert_eq!(reply.prompt_tokens, Some(9));
    assert_eq!(reply.completion_tokens, Some(17));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn provider_titles_use_their_default_model_and_keep_foreground_effort() {
    for (provider, kind, foreground, title, expected_path, responses) in [
        (
            "grok",
            "",
            "grok-4.6",
            "grok-4.6",
            "/chat/completions",
            false,
        ),
        (
            "codex",
            "codex",
            "gpt-6.1-sol",
            "gpt-6-luna",
            "/responses",
            true,
        ),
        (
            "opencode",
            "opencode",
            "gpt-6.1-sol",
            "deepseek-v4.1-flash",
            "/chat/completions",
            true,
        ),
        (
            "openrouter",
            "",
            "foreground-model",
            crate::config::DEFAULT_TITLE_MODEL,
            "/chat/completions",
            false,
        ),
    ] {
        let response_reply = serde_json::json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": "Session title"}]}]}).to_string();
        let server = FakeServer::start(vec![
            Canned::Json(if provider == "codex" {
                response_reply.clone()
            } else {
                a_text_reply()
            }),
            Canned::Json(if responses {
                response_reply
            } else {
                a_text_reply()
            }),
        ]);
        let root = grok_root(&format!("provider-default-title-{provider}"));
        let config = Config::from_toml(&format!(
            "provider = {provider:?}\neffort = \"high\"\ntitle_model = {:?}\n\n[providers.{provider}]\nkind = {kind:?}\nbase_url = {:?}\nmodel = {foreground:?}\n",
            crate::config::DEFAULT_TITLE_MODEL, server.base_url()
        )).unwrap();
        match provider {
            "grok" => crate::auth::write_tokens(
                &root.join(crate::auth::AUTH_FILE),
                &crate::auth::Tokens {
                    access_token: "grok-title-access".into(),
                    refresh_token: "grok-title-refresh".into(),
                    expires_at: "2035-01-01T00:00:00.000Z".into(),
                },
            )
            .unwrap(),
            "codex" => write_codex(
                &root,
                "codex-title-access",
                "codex-title-refresh",
                "2035-01-01T00:00:00.000Z",
            ),
            "opencode" => write_opencode_key(&root, "opencode-title-key"),
            _ => {}
        }
        let client = ChatClient::in_root(&config, Some(&root)).unwrap();
        assert_eq!(config.title_model(), Some(title));
        client
            .title(config.title_model().unwrap(), "Name this session")
            .await
            .unwrap();
        client.complete(&a_conversation(), &[]).await.unwrap();
        let requests = server.received.lock().unwrap().clone();
        assert_eq!(requests.len(), 2, "provider {provider}");
        assert_eq!(requests[0].path, expected_path, "provider {provider}");
        let expected_auth = match provider {
            "grok" => Some("Bearer grok-title-access"),
            "codex" => Some("Bearer codex-title-access"),
            "opencode" => Some("Bearer opencode-title-key"),
            _ => None,
        };
        assert_eq!(requests[0].header("authorization"), expected_auth);
        assert_eq!(
            requests[0].header("chatgpt-account-id"),
            (provider == "codex").then_some("acc-1")
        );

        let title_body = requests[0].body_json();
        assert_eq!(title_body["model"], title);
        assert!(title_body.get("tools").is_none());
        assert!(title_body.get("tool_choice").is_none());
        if provider == "grok" {
            assert_eq!(title_body["reasoning_effort"], "low");
        } else {
            assert!(title_body.get("reasoning_effort").is_none());
            assert!(title_body.get("reasoning").is_none());
        }
        assert_eq!(
            requests[1].header("authorization"),
            requests[0].header("authorization")
        );
        let foreground_body = requests[1].body_json();
        assert_eq!(foreground_body["model"], foreground);
        if responses {
            assert_eq!(requests[1].path, "/responses");
            assert_eq!(foreground_body["reasoning"]["effort"], "high");
        } else {
            assert_eq!(foreground_body["reasoning_effort"], "high");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
