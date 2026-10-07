use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

use base64::Engine;
use kyotoagent::config::Config;
use kyotoagent::permit::Answer;
use kyotoagent::session::{Session, SessionMeta};
use kyotoagent::tools::{ImageGeneration, Tools};
use serde_json::{json, Value};

struct Fixture {
    root: PathBuf,
    tools: Tools,
    config: Config,
}

impl Fixture {
    fn new(name: &str, base_url: &str) -> Self {
        let root = std::env::temp_dir().join(format!("kyoto-image-{}-{name}", std::process::id()));
        fs::create_dir_all(root.join("workspace")).unwrap();
        let session = Session::at(&root.join("session"));
        session
            .create(&SessionMeta::new(
                "image-test",
                &root.join("workspace"),
                "chat-model",
                "2026-10-07T00:00:00.000Z",
            ))
            .unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!("base_url = {base_url:?}\nmodel = \"chat-model\"\n"),
        )
        .unwrap();
        Self {
            tools: Tools::at(&session).unwrap(),
            config: Config::load(&config_path).unwrap(),
            root,
        }
    }

    fn request(&self) -> ImageGeneration {
        serde_json::from_value(json!({
            "prompt": "A Kyoto garden",
            "model": "image-model",
            "path": "garden.png"
        }))
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn image_bytes() -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(1, 1)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn server(response: Value, status: u16) -> (String, std::thread::JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/", listener.local_addr().unwrap());
    let task = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let header_end = loop {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break request.len();
            }
        };
        let headers = String::from_utf8_lossy(&request).to_ascii_lowercase();
        assert!(headers.starts_with("post /v1/images/generations http/1.1"));
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        request.resize(header_end + length, 0);
        stream.read_exact(&mut request[header_end..]).unwrap();
        let body = serde_json::from_slice(&request[header_end..]).unwrap();
        let response = response.to_string();
        write!(stream, "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        body
    });
    (url, task)
}

#[test]
fn generates_base64_image_with_provider_endpoint_and_explicit_model() {
    let bytes = image_bytes();
    let (url, server) = server(
        json!({"data": [{"b64_json": base64::engine::general_purpose::STANDARD.encode(&bytes)}]}),
        200,
    );
    let fixture = Fixture::new("base64", &url);
    fixture.tools.gate().queue(Answer::allow_once());
    fixture.tools.gate().queue(Answer::allow_once());
    let write = fixture
        .tools
        .generate_image("t1", &fixture.config, fixture.request())
        .unwrap();
    assert!(write.created);
    assert_eq!(fs::read(&write.path).unwrap(), bytes);
    assert_eq!(
        server.join().unwrap(),
        json!({"prompt": "A Kyoto garden", "model": "image-model", "n": 1})
    );
}

#[test]
fn denial_prevents_network_request_and_file_creation() {
    let fixture = Fixture::new("deny", "http://127.0.0.1:1/v1");
    fixture.tools.gate().queue(Answer::deny());
    let write = fixture
        .tools
        .generate_image("t1", &fixture.config, fixture.request())
        .unwrap();
    assert!(write.denied);
    assert!(!fixture.tools.workspace().join("garden.png").exists());
}

#[test]
fn refuses_unsafe_destination_before_network_request() {
    let fixture = Fixture::new("symlink", "http://127.0.0.1:1/v1");
    std::os::unix::fs::symlink(&fixture.root, fixture.tools.workspace().join("escape")).unwrap();
    let mut request = fixture.request();
    request.path = "escape/image.png".into();
    assert!(fixture
        .tools
        .generate_image("t1", &fixture.config, request)
        .is_err());
    assert!(!fixture.root.join("image.png").exists());
}

#[test]
fn malformed_responses_and_http_errors_preserve_existing_file() {
    for (name, response, status) in [
        ("base64-error", json!({"data": [{"b64_json": "!"}]}), 200),
        ("empty", json!({"data": []}), 200),
        (
            "not-image",
            json!({"data": [{"b64_json": "aGVsbG8="}]}),
            200,
        ),
        ("http-error", json!({"error": {"message": "secret"}}), 401),
        (
            "private-url",
            json!({"data": [{"url": "http://127.0.0.1:1/image"}]}),
            200,
        ),
    ] {
        let (url, server) = server(response, status);
        let fixture = Fixture::new(name, &url);
        let destination = fixture.tools.workspace().join("garden.png");
        fs::write(&destination, "original").unwrap();
        fixture.tools.gate().queue(Answer::allow_once());
        let error = fixture
            .tools
            .generate_image("t1", &fixture.config, fixture.request())
            .unwrap_err();
        assert!(!error.to_string().contains("secret"));
        assert_eq!(fs::read_to_string(destination).unwrap(), "original");
        server.join().unwrap();
    }
}

#[test]
fn write_denial_preserves_file_after_generation() {
    let (url, server) = server(
        json!({"data": [{"b64_json": base64::engine::general_purpose::STANDARD.encode(image_bytes())}]}),
        200,
    );
    let fixture = Fixture::new("write-deny", &url);
    fs::write(fixture.tools.workspace().join("garden.png"), "original").unwrap();
    fixture.tools.gate().queue(Answer::allow_once());
    fixture.tools.gate().queue(Answer::deny());
    let result = fixture
        .tools
        .generate_image("t1", &fixture.config, fixture.request())
        .unwrap();
    assert!(result.denied);
    assert_eq!(fs::read_to_string(result.path).unwrap(), "original");
    server.join().unwrap();
}

#[test]
fn tool_is_discoverable_and_respects_profiles() {
    let fixture = Fixture::new("definition", "http://127.0.0.1:1/v1");
    assert!(kyotoagent::turn::known_tool_names().contains(&"generate_image"));
    let tools = kyotoagent::turn::tool_definitions(&fixture.config, false);
    assert!(tools.iter().any(|tool| tool.name == "generate_image"));
    let tools = kyotoagent::turn::tool_definitions_for(&fixture.config, false, Some("missing"));
    assert!(tools.is_empty());
}
