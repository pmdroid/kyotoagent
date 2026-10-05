//! Send one real completion to a local server and print what went out and
//! what came back.
//!
//! This is proof, not decoration: it starts an HTTP server on loopback, points
//! the real `ChatClient` at it with a real `Config`, and writes down the bytes
//! that crossed the wire. The interesting part is the header, because that is
//! the difference between an OpenRouter key and a local model.
//!
//!     cargo run --example chat

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};

use kyotoagent::chat::{ChatClient, Message, Tool};
use kyotoagent::config::Config;

fn main() {
    let with_key = std::env::args().any(|arg| arg == "--with-key");
    if with_key {
        std::env::set_var("KYOTOAGENT_EXAMPLE_KEY", "sk-example-not-a-real-key");
    }

    let (addr, seen) = start_fake_server();
    let config = Config::from_toml(&format!(
        "base_url = \"http://{addr}\"\nmodel = \"openai/gpt-4o\"\n{}",
        if with_key {
            "api_key_env = \"KYOTOAGENT_EXAMPLE_KEY\""
        } else {
            ""
        }
    ))
    .expect("the config parses");

    println!("config.toml\n------------");
    println!("base_url   = \"{}\"", config.base_url);
    println!("model      = \"{}\"", config.model);
    match &config.api_key_env {
        Some(name) => println!("api_key_env = \"{name}\""),
        None => println!("api_key_env = (left out)"),
    }
    println!();
    println!("chat url   = {}", config.chat_url());
    println!(
        "api key    = {}",
        match config.api_key() {
            Some(_) => "read from the environment",
            None => "none, so no Authorization header is sent",
        }
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the runtime builds");
    let tools = vec![Tool::new(
        "read_file",
        "Read a file in the workspace.",
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
    )];
    let messages = vec![
        Message::System {
            content: "You are kyotoagent.".into(),
        },
        Message::User {
            content: "What is in README.md?".into(),
        },
    ];

    let reply = runtime.block_on(async {
        let client = ChatClient::new(&config).expect("the client is built");
        client.complete(&messages, &tools).await
    });

    let request = seen.lock().expect("the log is not poisoned").remove(0);
    println!();
    println!("request\n-------");
    println!("{} {}", request.method, request.path);
    for (key, value) in &request.headers {
        if matches!(key.as_str(), "authorization" | "content-type") {
            println!("{key}: {value}");
        }
    }
    println!("{}", pretty(&request.body));

    println!();
    println!("reply\n-----");
    match reply {
        Ok(reply) => {
            for call in &reply.tool_calls {
                println!("tool call {}: {} {}", call.id, call.name, call.arguments);
            }
            if !reply.wants_tools() {
                println!("text: {}", reply.text());
            }
            let mut transcript = messages.clone();
            transcript.push(Message::assistant_reply(&reply));
            for call in &reply.tool_calls {
                transcript.push(Message::tool_result(&call.id, "kyotoagent is the binary."));
            }
            println!();
            println!("the transcript the loop would send next\n-------------------------------------------");
            println!(
                "{}",
                pretty(&serde_json::to_string(&transcript).expect("it serializes"))
            );
        }
        Err(error) => println!("error: {error}"),
    }
}

fn pretty(json: &str) -> String {
    serde_json::to_string_pretty(
        &serde_json::from_str::<serde_json::Value>(json).expect("it is JSON"),
    )
    .expect("it pretty prints")
}

struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

/// A one-shot HTTP server that records the request and answers with a canned
/// tool call. It exists only long enough for the example to run.
fn start_fake_server() -> (SocketAddr, std::sync::Arc<std::sync::Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake server binds");
    let addr = listener.local_addr().expect("it has an address");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = std::sync::Arc::clone(&seen);
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a request arrives");
        let request = read_request(&mut stream).expect("the request reads");
        let request_body = request.body.clone();
        recorder
            .lock()
            .expect("the log is not poisoned")
            .push(request);
        let body = serde_json::json!({
            "id": "gen-1",
            "provider": "openrouter",
            "model": "openai/gpt-4o",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"README.md\"}"
                        }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 42, "completion_tokens": 9 }
        })
        .to_string();
        kyotoagent::chat::answer_completion(&mut stream, 200, &body, &request_body);
    });
    (addr, seen)
}

fn read_request(stream: &mut TcpStream) -> Option<Seen> {
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
    let method = start.next()?.to_string();
    let path = start.next()?.to_string();
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

    Some(Seen {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}
