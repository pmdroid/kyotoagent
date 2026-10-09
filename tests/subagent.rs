use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
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

const AT: &str = "2026-09-29T00:00:00.000Z";

struct HoldGate {
    released: AtomicBool,
}

impl HoldGate {
    fn new() -> Arc<HoldGate> {
        Arc::new(HoldGate {
            released: AtomicBool::new(false),
        })
    }

    fn release(&self) {
        self.released.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone)]
enum Canned {
    Json(String),
    Hold { tail: String, gate: Arc<HoldGate> },
    When { needle: String, inner: Box<Canned> },
}

struct FakeServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    bodies: Arc<Mutex<Vec<String>>>,
}

impl FakeServer {
    fn start(parent: Vec<Canned>, child: Vec<Canned>) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the fake server binds");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let parent = Arc::new(Mutex::new(parent));
        let child = Arc::new(Mutex::new(child));
        let stop = Arc::new(AtomicBool::new(false));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let handle = {
            let stop = Arc::clone(&stop);
            let bodies = Arc::clone(&bodies);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let parent = Arc::clone(&parent);
                            let child = Arc::clone(&child);
                            let bodies = Arc::clone(&bodies);
                            let stop = Arc::clone(&stop);
                            std::thread::spawn(move || {
                                let _ = stream.set_nonblocking(false);
                                serve_one(stream, &parent, &child, &bodies, &stop);
                            });
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
        format!("http://{}", self.addr)
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().expect("bodies").clone()
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
    parent: &Mutex<Vec<Canned>>,
    child: &Mutex<Vec<Canned>>,
    bodies: &Mutex<Vec<String>>,
    stop: &AtomicBool,
) {
    let Some((path, body)) = read_request(&mut stream) else {
        return;
    };
    bodies.lock().expect("bodies").push(body.clone());
    if path.contains("/models") {
        let catalog = serde_json::json!({
            "data": [{ "id": "test/model", "context_length": 200000 }]
        })
        .to_string();
        respond(&mut stream, &catalog);
        return;
    }
    let child_turn = body.contains("You are a Kyoto Agent subagent.");
    let queue = if child_turn { child } else { parent };
    let mut reply = {
        let mut queue = queue.lock().expect("queue");
        take_canned(&mut queue, &body)
    };
    loop {
        match reply {
            Canned::When { inner, .. } => reply = *inner,
            Canned::Json(body) => {
                kyotoagent::chat::answer_completion(
                    &mut stream,
                    200,
                    &body,
                    &body_of(bodies, &body),
                );
                break;
            }
            Canned::Hold { tail, gate } => {
                hold_stream(&mut stream, &tail, &gate, stop);
                break;
            }
        }
    }
}

fn body_of(bodies: &Mutex<Vec<String>>, fallback: &str) -> String {
    bodies
        .lock()
        .expect("bodies")
        .last()
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

fn hold_stream(stream: &mut TcpStream, tail: &str, gate: &HoldGate, stop: &AtomicBool) {
    let opening = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
    let _ = stream.write_all(opening.as_bytes());
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    loop {
        if stop.load(Ordering::Relaxed) || gate.released.load(Ordering::Relaxed) {
            break;
        }
        let mut buf = [0_u8; 1];
        match stream.peek(&mut buf) {
            Ok(0) => return,
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return,
            Ok(_) => {}
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    if stop.load(Ordering::Relaxed) {
        return;
    }
    write_chunk(stream, tail);
    if !tail.contains("[DONE]") {
        write_chunk(stream, "data: [DONE]\n\n");
    }
    let _ = stream.write_all(b"0\r\n\r\n");
    let _ = stream.flush();
}

fn write_chunk(stream: &mut TcpStream, data: &str) {
    let head = format!("{:x}\r\n", data.len());
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(data.as_bytes());
    let _ = stream.write_all(b"\r\n");
    let _ = stream.flush();
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

fn respond(stream: &mut TcpStream, body: &str) {
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn take_canned(queue: &mut Vec<Canned>, body: &str) -> Canned {
    if let Some(index) = queue
        .iter()
        .position(|item| matches!(item, Canned::When { needle, .. } if body.contains(needle)))
    {
        return unwrap_when(queue.remove(index));
    }
    let next = if queue.len() > 1 {
        queue.remove(0)
    } else {
        queue
            .first()
            .cloned()
            .unwrap_or_else(|| Canned::Json(text_reply("Noted.")))
    };
    unwrap_when(next)
}

fn unwrap_when(canned: Canned) -> Canned {
    match canned {
        Canned::When { inner, .. } => *inner,
        other => other,
    }
}

fn text_reply(text: &str) -> String {
    serde_json::json!({
        "choices": [{ "message": { "role": "assistant", "content": text } }]
    })
    .to_string()
}

fn tool_call(name: &str, args: serde_json::Value) -> String {
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
        }]
    })
    .to_string()
}

fn finish_reply(text: &str, proof: &str) -> String {
    tool_call(
        "finish",
        serde_json::json!({ "text": text, "proof": proof }),
    )
}

fn spawn_args(extra: serde_json::Value) -> serde_json::Value {
    let mut args = serde_json::json!({
        "prompt": "Do the child work.",
        "description": "Do the child work",
    });
    if let (Some(base), Some(more)) = (args.as_object_mut(), extra.as_object()) {
        for (key, value) in more {
            base.insert(key.clone(), value.clone());
        }
    }
    args
}

struct Fixture {
    runner: Arc<Runner>,
    server: FakeServer,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(name: &str, parent: Vec<Canned>, child: Vec<Canned>) -> Fixture {
        Self::open(name, parent, child, "")
    }

    fn open(name: &str, parent: Vec<Canned>, child: Vec<Canned>, extra: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "kyotoagent-sub-{}-{}-{name}",
            std::process::id(),
            name.len()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("root");
        let server = FakeServer::start(parent, child);
        let config = Config::from_toml(&format!(
            "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n{extra}",
            server.base_url()
        ))
        .expect("config");
        let runner = Runner::new(&config).expect("runner");
        Fixture {
            runner,
            server,
            root,
        }
    }

    fn configured(name: &str, parent: Vec<Canned>, child: Vec<Canned>, extra: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "kyotoagent-sub-{}-{}-{name}",
            std::process::id(),
            name.len()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("root");
        let server = FakeServer::start(parent, child);
        let config = Config::from_toml(&format!(
            "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n{extra}",
            server.base_url()
        ))
        .expect("config");
        let runner = Runner::new(&config).expect("runner");
        Fixture {
            runner,
            server,
            root,
        }
    }

    fn add_session(&self, id: &str) -> PathBuf {
        let workspace = self.root.join(format!("w-{id}"));
        fs::create_dir_all(&workspace).expect("workspace");
        let session = Session::at(&self.root.join(format!("session-{id}")));
        session
            .create(&SessionMeta::new(id, &workspace, "test/model", AT))
            .expect("session");
        self.runner.add_session(&session).expect("added");
        workspace
    }

    fn session_count(&self) -> usize {
        fs::read_dir(&self.root)
            .expect("root")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().join("meta.json").is_file())
            .count()
    }

    async fn wait_status(&self, id: &str, status: Status) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let current = self
                .runner
                .view(id)
                .map(|view| view.status)
                .unwrap_or(Status::Idle);
            if current == status {
                return;
            }
            assert!(Instant::now() < deadline, "session {id} stayed {current:?}");
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }
}

fn git_repo(path: &Path) {
    fs::create_dir_all(path).expect("repo");
    let run = |args: &[&str]| {
        let status = Command::new("git")
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
async fn a_child_is_created_with_enhance_off() {
    let fixture = Fixture::open(
        "enhance-child",
        Vec::new(),
        vec![Canned::Json(finish_reply("Done.", "the child finished."))],
        "enhance = true\n",
    );
    assert!(fixture.runner.current_config().enhance);
    fixture.add_session("parent");
    let parent = Session::at(&fixture.root.join("session-parent"));
    parent.set_enhance(true).expect("the parent flag is stored");
    let summary = fixture
        .runner
        .spawn_subagent(
            "parent",
            &spawn_args(serde_json::json!({ "run_in_background": true })),
        )
        .await;
    let id = child_id(&summary);
    let child = Session::at(&fixture.root.join(&id))
        .meta()
        .expect("child meta");
    assert!(!child.enhance);
    assert!(child.hidden);
    assert_eq!(child.parent_id.as_deref(), Some("parent"));
    assert!(parent.meta().expect("parent meta").enhance);
    let shown = fixture
        .runner
        .spawn_subagent(
            "parent",
            &spawn_args(serde_json::json!({ "run_in_background": true, "visible": true })),
        )
        .await;
    let shown_id = child_id(&shown);
    assert!(
        !Session::at(&fixture.root.join(&shown_id))
            .meta()
            .expect("shown meta")
            .hidden
    );
}

fn child_id(summary: &str) -> String {
    serde_json::from_str::<serde_json::Value>(summary)
        .ok()
        .and_then(|value| {
            value
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| panic!("spawn summary has no id: {summary}"))
}

#[tokio::test]
async fn a_background_spawn_returns_while_the_child_is_working_and_the_parent_keeps_going() {
    let gate = HoldGate::new();
    let tail = kyotoagent::chat::completion_as_sse(&finish_reply("held result", "held proof"));
    let parent = vec![
        Canned::Json(tool_call(
            "spawn_subagent",
            spawn_args(serde_json::json!({ "run_in_background": true })),
        )),
        Canned::Json(tool_call("list_dir", serde_json::json!({ "path": "." }))),
        Canned::Json(finish_reply(
            "Parent moved on.",
            "The child was still working.",
        )),
    ];
    let child = vec![Canned::Hold {
        tail,
        gate: Arc::clone(&gate),
    }];
    let fixture = Fixture::new("overlap", parent, child);
    let workspace = fixture.add_session("parent");
    fixture
        .runner
        .ask("parent", "Delegate the listing.")
        .expect("ask");
    let deadline = Instant::now() + Duration::from_secs(8);
    let child = loop {
        let events = Session::at(&fixture.root.join("session-parent"))
            .events()
            .expect("log");
        let listed = events.iter().any(|event| {
            event.kind == EventKind::ToolCall
                && event.body.get("tool").and_then(|value| value.as_str()) == Some("list_dir")
        });
        let spawned = events.iter().rev().find(|event| {
            event.kind == EventKind::ToolResult
                && event.body.get("tool").and_then(|value| value.as_str()) == Some("spawn_subagent")
        });
        if listed {
            let summary = spawned
                .and_then(|event| event.body.get("output").and_then(|value| value.as_str()))
                .unwrap_or("");
            let id = child_id(summary);
            assert_eq!(
                fixture.runner.view(&id).expect("child view").status,
                Status::Working
            );
            break id;
        }
        assert!(
            Instant::now() < deadline,
            "the parent did not list while the child worked"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    };
    fixture.wait_status("parent", Status::Idle).await;
    let cards =
        serde_json::to_string(&fixture.runner.view("parent").expect("view").cards).expect("cards");
    assert!(!cards.contains("spawn_subagent"), "{cards}");
    assert!(!cards.contains("list_dir"), "{cards}");
    gate.release();
    fixture.wait_status(&child, Status::Idle).await;
    let checked = fixture
        .runner
        .check_task("parent", &[&child], Some(0))
        .await;
    assert!(checked.contains("held result"), "{checked}");
    assert!(checked.contains("held proof"), "{checked}");
    assert!(workspace.join(".").is_dir());
}

#[tokio::test]
async fn check_task_after_finish_returns_the_result_and_the_proof() {
    let child = vec![Canned::Json(finish_reply(
        "The file is in place.",
        "cargo test passed.",
    ))];
    let fixture = Fixture::new("check", vec![Canned::Json(text_reply("Noted."))], child);
    fixture.add_session("parent");
    let summary = fixture
        .runner
        .spawn_subagent(
            "parent",
            &spawn_args(serde_json::json!({ "run_in_background": true })),
        )
        .await;
    let id = child_id(&summary);
    assert!(
        summary.contains("working") || summary.contains("idle"),
        "{summary}"
    );
    fixture.wait_status(&id, Status::Idle).await;
    let checked = fixture.runner.check_task("parent", &[&id], None).await;
    let value: serde_json::Value = serde_json::from_str(&checked).expect("one snapshot");
    assert!(value.is_object(), "{checked}");
    assert_eq!(value["state"], "idle", "{checked}");
    assert_eq!(value["result"], "The file is in place.", "{checked}");
    assert_eq!(value["proof"], "cargo test passed.", "{checked}");
}

#[tokio::test]
async fn a_child_cannot_spawn_past_depth_1() {
    let child = vec![Canned::Json(finish_reply("Done.", "proof"))];
    let fixture = Fixture::new("depth", vec![Canned::Json(text_reply("Noted."))], child);
    fixture.add_session("parent");
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent("parent", &spawn_args(serde_json::json!({})))
            .await,
    );
    fixture.wait_status(&id, Status::Idle).await;
    let before = fixture.session_count();
    let error = fixture
        .runner
        .spawn_subagent(&id, &spawn_args(serde_json::json!({})))
        .await;
    assert!(error.contains("depth 1"), "{error}");
    assert_eq!(fixture.session_count(), before);
}

#[tokio::test]
async fn cwd_and_worktree_do_not_create_a_session() {
    let fixture = Fixture::new(
        "pair",
        vec![Canned::Json(text_reply("Noted."))],
        vec![Canned::Json(text_reply("Noted."))],
    );
    fixture.add_session("parent");
    let before = fixture.session_count();
    let error = fixture
        .runner
        .spawn_subagent(
            "parent",
            &spawn_args(serde_json::json!({
                "isolation": "worktree",
                "cwd": "/tmp",
            })),
        )
        .await;
    assert!(error.contains("cwd and worktree"), "{error}");
    assert_eq!(fixture.session_count(), before);
}

#[tokio::test]
async fn an_unknown_model_is_refused() {
    let fixture = Fixture::new(
        "model",
        vec![Canned::Json(text_reply("Noted."))],
        vec![Canned::Json(text_reply("Noted."))],
    );
    fixture.add_session("parent");
    let before = fixture.session_count();
    let error = fixture
        .runner
        .spawn_subagent(
            "parent",
            &spawn_args(serde_json::json!({ "model": "not-a-catalog-model" })),
        )
        .await;
    assert!(error.contains("catalog"), "{error}");
    assert_eq!(fixture.session_count(), before);
}

#[tokio::test]
async fn kill_task_cancels_the_child() {
    let gate = HoldGate::new();
    let tail = kyotoagent::chat::completion_as_sse(&finish_reply("should not land", "nope"));
    let fixture = Fixture::new(
        "kill",
        vec![Canned::Json(text_reply("Noted."))],
        vec![Canned::Hold {
            tail,
            gate: Arc::clone(&gate),
        }],
    );
    fixture.add_session("parent");
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent(
                "parent",
                &spawn_args(serde_json::json!({ "run_in_background": true })),
            )
            .await,
    );
    fixture.wait_status(&id, Status::Working).await;
    let before = fixture.session_count();
    let killed = fixture.runner.kill_task("parent", &id).await;
    assert_eq!(killed, format!("closed {id}"));
    assert!(fixture.runner.view(&id).is_err());
    assert!(!fixture.root.join(&id).join("meta.json").exists());
    assert_eq!(fixture.session_count(), before - 1);
    assert!(fixture.runner.view("parent").is_ok());
    gate.release();
}

#[tokio::test]
async fn resume_sends_the_new_prompt_after_the_childs_finish() {
    let child = vec![
        Canned::Json(finish_reply("alpha result", "alpha proof")),
        Canned::Json(finish_reply("beta result", "beta proof")),
    ];
    let fixture = Fixture::new("resume", vec![Canned::Json(text_reply("Noted."))], child);
    fixture.add_session("parent");
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent("parent", &spawn_args(serde_json::json!({})))
            .await,
    );
    fixture.wait_status(&id, Status::Idle).await;
    let again = fixture
        .runner
        .spawn_subagent(
            "parent",
            &spawn_args(serde_json::json!({
                "resume_from": id,
                "prompt": "do the next part",
            })),
        )
        .await;
    assert!(!again.contains("resume_from needs"), "{again}");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let found = fixture.server.bodies().into_iter().any(|body| {
            let result = body.find("alpha result");
            let prompt = body.find("do the next part");
            match (result, prompt) {
                (Some(result), Some(prompt)) => result < prompt,
                _ => false,
            }
        });
        if found {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the model did not see the prior finish"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

#[tokio::test]
async fn worktree_isolation_keeps_the_write_out_of_the_parent_workspace() {
    let child = vec![
        Canned::Json(tool_call(
            "write_file",
            serde_json::json!({ "path": "note.txt", "contents": "from the child\n" }),
        )),
        Canned::Json(finish_reply(
            "Wrote the note.",
            "note.txt exists in the worktree",
        )),
    ];
    let fixture = Fixture::new("worktree", vec![Canned::Json(text_reply("Noted."))], child);
    let workspace = fixture.root.join("repo");
    git_repo(&workspace);
    let session = Session::at(&fixture.root.join("session-parent"));
    session
        .create(&SessionMeta::new("parent", &workspace, "test/model", AT))
        .expect("session");
    fixture.runner.add_session(&session).expect("added");
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent(
                "parent",
                &spawn_args(serde_json::json!({ "isolation": "worktree" })),
            )
            .await,
    );
    fixture.wait_status(&id, Status::Waiting).await;
    assert!(!workspace.join("note.txt").exists());
    fixture
        .runner
        .answer(&id, Answer::allow_once())
        .expect("allow");
    fixture.wait_status(&id, Status::Idle).await;
    let child_workspace = Session::at(&fixture.root.join(&id))
        .meta()
        .expect("child meta")
        .workspace;
    assert_ne!(child_workspace, workspace.display().to_string());
    assert!(Path::new(&child_workspace).join("note.txt").is_file());
    assert!(!workspace.join("note.txt").exists());
}

#[tokio::test]
async fn isolation_none_writes_through_the_permission_gate() {
    let child = vec![
        Canned::Json(tool_call(
            "write_file",
            serde_json::json!({ "path": "note.txt", "contents": "in the parent\n" }),
        )),
        Canned::Json(finish_reply("Wrote it.", "the gate allowed the write")),
    ];
    let fixture = Fixture::new("same", vec![Canned::Json(text_reply("Noted."))], child);
    let workspace = fixture.add_session("parent");
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent(
                "parent",
                &spawn_args(serde_json::json!({ "isolation": "none" })),
            )
            .await,
    );
    fixture.wait_status(&id, Status::Waiting).await;
    assert!(!workspace.join("note.txt").exists());
    fixture
        .runner
        .answer(&id, Answer::allow_once())
        .expect("allow");
    fixture.wait_status(&id, Status::Idle).await;
    assert_eq!(
        fs::read_to_string(workspace.join("note.txt")).expect("written"),
        "in the parent\n"
    );
}

#[tokio::test]
async fn a_child_cannot_run_closeout_or_prompt_the_user_after_writing() {
    let fixture = Fixture::new(
        "child-closeout",
        vec![Canned::Json(text_reply("Noted."))],
        vec![
            Canned::Json(tool_call(
                "write_file",
                serde_json::json!({ "path": "note.txt", "contents": "child work" }),
            )),
            Canned::Json(tool_call("get_closeout", serde_json::json!({}))),
            Canned::Json(tool_call(
                "run_closeout",
                serde_json::json!({ "id": "test" }),
            )),
            Canned::Json(finish_reply("Need the parent to choose a name.", "")),
        ],
    );
    let workspace = fixture.add_session("parent");
    fs::create_dir_all(workspace.join(".kyotoagent")).unwrap();
    fs::write(
        workspace.join(".kyotoagent/closeout.yaml"),
        "version: 1\nretry:\n  maxFailedAttemptsPerItem: 1\nitems:\n  - id: test\n    kind: command\n    run: sh -c 'touch check-ran; exit 1'\n    hint: Fix the failing test\n",
    ).unwrap();
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent(
                "parent",
                &spawn_args(serde_json::json!({ "isolation": "none" })),
            )
            .await,
    );
    fixture.wait_status(&id, Status::Waiting).await;
    fixture.runner.answer(&id, Answer::allow_once()).unwrap();
    fixture.wait_status(&id, Status::Idle).await;
    assert_eq!(
        fs::read_to_string(workspace.join("note.txt")).unwrap(),
        "child work"
    );
    assert!(!workspace.join("check-ran").exists());
    let events = Session::at(&fixture.root.join(&id)).events().unwrap();
    assert!(events.iter().all(|event| event.kind != EventKind::Question));
    for tool in ["get_closeout", "run_closeout"] {
        assert!(events
            .iter()
            .any(|event| event.kind == EventKind::ToolResult
                && event.body.get("tool").and_then(|value| value.as_str()) == Some(tool)
                && event.body.get("output").and_then(|value| value.as_str())
                    == Some(format!("unknown tool: {tool}").as_str())));
    }
    let view = fixture.runner.view(&id).unwrap();
    assert!(view.closeout.is_empty());
    assert!(events.iter().any(|event| event.kind == EventKind::Result
        && event.body.get("text").and_then(|value| value.as_str())
            == Some("Need the parent to choose a name.")));
    assert!(!fixture.runner.view("parent").unwrap().closeout.is_empty());
}

#[tokio::test]
async fn a_child_ask_is_refused_and_finish_wakes_the_parent() {
    let gate = HoldGate::new();
    let tail = kyotoagent::chat::completion_as_sse(&finish_reply("found 3 files", "grep exit 0"));
    let fixture = Fixture::new(
        "child-ask",
        vec![Canned::Json(text_reply("Noted."))],
        vec![
            Canned::Json(tool_call(
                "ask",
                serde_json::json!({ "text": "Which name?" }),
            )),
            Canned::Hold {
                tail,
                gate: Arc::clone(&gate),
            },
        ],
    );
    fixture.add_session("parent");
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent(
                "parent",
                &spawn_args(serde_json::json!({ "run_in_background": true })),
            )
            .await,
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let events = Session::at(&fixture.root.join(&id))
            .events()
            .expect("child log");
        let asked = events.iter().any(|event| {
            event.kind == EventKind::ToolResult
                && event.body.get("tool").and_then(|value| value.as_str()) == Some("ask")
                && event.body.get("output").and_then(|value| value.as_str())
                    == Some("A subagent has no ask tool. Call finish with the decision you needed.")
        });
        let child_posts = fixture
            .server
            .bodies()
            .into_iter()
            .filter(|body| body.contains("You are a Kyoto Agent subagent."))
            .count();
        if asked && child_posts >= 2 {
            assert!(
                events.iter().all(|event| event.kind != EventKind::Question),
                "a refused ask appends no question"
            );
            assert_eq!(
                fixture.runner.view(&id).expect("child view").status,
                Status::Working
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the child did not refuse ask while still working"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    let child_tools = fixture
        .server
        .bodies()
        .into_iter()
        .find(|body| body.contains("You are a Kyoto Agent subagent."))
        .expect("child request");
    let names = function_names(&child_tools);
    assert!(names.iter().any(|name| name == "finish"), "{names:?}");
    assert!(names.iter().any(|name| name == "read_file"), "{names:?}");
    assert!(!names.iter().any(|name| name == "ask"), "{names:?}");
    assert!(
        !names.iter().any(|name| name == "spawn_subagent"),
        "{names:?}"
    );
    gate.release();
    fixture.wait_status(&id, Status::Idle).await;
    let events = Session::at(&fixture.root.join(&id))
        .events()
        .expect("child log");
    assert!(events.iter().all(|event| event.kind != EventKind::Question));
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let parent = Session::at(&fixture.root.join("session-parent"))
            .events()
            .expect("parent log");
        let woke = parent.iter().any(|event| {
            event.kind == EventKind::UserAsk
                && event.body.get("text").and_then(|value| value.as_str())
                    == Some("Subagent finished.")
                && event.body.get("silent").and_then(|value| value.as_bool()) == Some(true)
                && event
                    .body
                    .get("context")
                    .and_then(|value| value.as_str())
                    .is_some_and(|context| {
                        context.contains("result: found 3 files\n")
                            && context.contains("proof: grep exit 0\n")
                            && context.contains(&format!("resume_from=\"{id}\""))
                            && !event
                                .body
                                .get("text")
                                .and_then(|value| value.as_str())
                                .unwrap_or("")
                                .contains(&id)
                    })
        });
        if woke {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the parent was not woken with the child result"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    fixture.wait_status("parent", Status::Idle).await;
    assert_quiet(&fixture, &[&id]);
    assert!(
        parent_transcript(&fixture, &["found 3 files", "grep exit 0", &id]),
        "the parent transcript missed the child finish"
    );
    let parent_tools = fixture
        .server
        .bodies()
        .into_iter()
        .find(|body| body.contains("You are Kyoto Agent, a coding agent"))
        .expect("parent request");
    let names = function_names(&parent_tools);
    assert!(names.iter().any(|name| name == "ask"), "{names:?}");
    assert!(
        names.iter().any(|name| name == "spawn_subagent"),
        "{names:?}"
    );
}

fn held_pair() -> (Vec<Canned>, Arc<HoldGate>, Arc<HoldGate>) {
    let gate_a = HoldGate::new();
    let gate_b = HoldGate::new();
    let child = vec![
        Canned::When {
            needle: "PROMPT-ALPHA".to_string(),
            inner: Box::new(Canned::Hold {
                tail: kyotoagent::chat::completion_as_sse(&finish_reply(
                    "alpha result",
                    "alpha proof",
                )),
                gate: Arc::clone(&gate_a),
            }),
        },
        Canned::When {
            needle: "PROMPT-BETA".to_string(),
            inner: Box::new(Canned::Hold {
                tail: kyotoagent::chat::completion_as_sse(&finish_reply(
                    "beta result",
                    "beta proof",
                )),
                gate: Arc::clone(&gate_b),
            }),
        },
    ];
    (child, gate_a, gate_b)
}

async fn spawn_held_pair(fixture: &Fixture) -> (String, String) {
    let id_a = child_id(
        &fixture
            .runner
            .spawn_subagent(
                "parent",
                &spawn_args(serde_json::json!({
                    "run_in_background": true,
                    "prompt": "PROMPT-ALPHA",
                })),
            )
            .await,
    );
    let id_b = child_id(
        &fixture
            .runner
            .spawn_subagent(
                "parent",
                &spawn_args(serde_json::json!({
                    "run_in_background": true,
                    "prompt": "PROMPT-BETA",
                })),
            )
            .await,
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let bodies = fixture.server.bodies();
        if bodies.iter().any(|body| body.contains("PROMPT-ALPHA"))
            && bodies.iter().any(|body| body.contains("PROMPT-BETA"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "both children did not reach the model"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    tokio::time::sleep(Duration::from_millis(40)).await;
    (id_a, id_b)
}

fn parent_events(fixture: &Fixture) -> Vec<kyotoagent::events::Event> {
    Session::at(&fixture.root.join("session-parent"))
        .events()
        .expect("parent log")
}

async fn wait_parent_tool(fixture: &Fixture, tool: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let found = parent_events(fixture).into_iter().find_map(|event| {
            if event.kind == EventKind::ToolResult
                && event.body.get("tool").and_then(|value| value.as_str()) == Some(tool)
            {
                event
                    .body
                    .get("output")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            } else {
                None
            }
        });
        if let Some(output) = found {
            return output;
        }
        assert!(
            Instant::now() < deadline,
            "the parent did not record {tool}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

#[tokio::test]
async fn check_task_without_an_id_is_refused() {
    let parent = vec![
        Canned::Json(tool_call("check_task", serde_json::json!({}))),
        Canned::Json(finish_reply("Refused.", "no id")),
    ];
    let fixture = Fixture::new("no-id", parent, vec![Canned::Json(text_reply("Noted."))]);
    fixture.add_session("parent");
    fixture.runner.ask("parent", "Check nothing.").expect("ask");
    let output = wait_parent_tool(&fixture, "check_task").await;
    assert_eq!(output, "check_task needs an id");
    fixture.wait_status("parent", Status::Idle).await;
}

#[tokio::test]
async fn check_task_ids_keep_the_order_of_id_then_ids() {
    let parent = vec![
        Canned::Json(tool_call(
            "check_task",
            serde_json::json!({
                "id": "missing-a",
                "ids": ["missing-b", "missing-a"],
                "timeout_sec": 0,
            }),
        )),
        Canned::Json(finish_reply("Looked.", "two misses")),
    ];
    let fixture = Fixture::new("id-order", parent, vec![Canned::Json(text_reply("Noted."))]);
    fixture.add_session("parent");
    fixture.runner.ask("parent", "Check both.").expect("ask");
    let output = wait_parent_tool(&fixture, "check_task").await;
    let value: serde_json::Value = serde_json::from_str(&output).expect("array");
    let rows = value.as_array().expect("two snapshots");
    assert_eq!(rows.len(), 2, "{output}");
    assert_eq!(rows[0], "no task missing-a in this session");
    assert_eq!(rows[1], "no task missing-b in this session");
    fixture.wait_status("parent", Status::Idle).await;
}

#[tokio::test]
async fn check_task_with_ids_returns_both_working_snapshots_at_once() {
    let (child, gate_a, gate_b) = held_pair();
    let fixture = Fixture::new("snap-now", vec![Canned::Json(text_reply("Noted."))], child);
    fixture.add_session("parent");
    let (id_a, id_b) = spawn_held_pair(&fixture).await;
    let started = Instant::now();
    let checked = tokio::time::timeout(
        Duration::from_secs(2),
        fixture
            .runner
            .check_task("parent", &[&id_b, &id_a], Some(0)),
    )
    .await
    .expect("timeout_sec 0 waited");
    assert!(started.elapsed() < Duration::from_secs(2));
    let value: serde_json::Value = serde_json::from_str(&checked).expect("array");
    let rows = value.as_array().expect("two snapshots");
    assert_eq!(rows.len(), 2, "{checked}");
    assert_eq!(rows[0]["id"], id_b, "{checked}");
    assert_eq!(rows[1]["id"], id_a, "{checked}");
    assert_eq!(rows[0]["state"], "working", "{checked}");
    assert_eq!(rows[1]["state"], "working", "{checked}");
    gate_a.release();
    gate_b.release();
    fixture.wait_status(&id_a, Status::Idle).await;
    fixture.wait_status(&id_b, Status::Idle).await;
}

#[tokio::test]
async fn check_task_waits_until_every_listed_child_is_idle() {
    let (child, gate_a, gate_b) = held_pair();
    let fixture = Fixture::new("wait-all", vec![Canned::Json(text_reply("Noted."))], child);
    fixture.add_session("parent");
    let (id_a, id_b) = spawn_held_pair(&fixture).await;
    let runner = Arc::clone(&fixture.runner);
    let left = id_a.clone();
    let right = id_b.clone();
    let waiting =
        tokio::spawn(async move { runner.check_task("parent", &[&right, &left], Some(8)).await });
    tokio::time::sleep(Duration::from_millis(40)).await;
    gate_b.release();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        !waiting.is_finished(),
        "check_task returned before both children were idle"
    );
    gate_a.release();
    let checked = tokio::time::timeout(Duration::from_secs(8), waiting)
        .await
        .expect("the wait timed out")
        .expect("the wait task");
    let value: serde_json::Value = serde_json::from_str(&checked).expect("array");
    let rows = value.as_array().expect("two snapshots");
    assert_eq!(rows[0]["id"], id_b, "{checked}");
    assert_eq!(rows[1]["id"], id_a, "{checked}");
    assert_eq!(rows[0]["state"], "idle", "{checked}");
    assert_eq!(rows[1]["state"], "idle", "{checked}");
    assert_eq!(rows[0]["result"], "beta result", "{checked}");
    assert_eq!(rows[0]["proof"], "beta proof", "{checked}");
    assert_eq!(rows[1]["result"], "alpha result", "{checked}");
    assert_eq!(rows[1]["proof"], "alpha proof", "{checked}");
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        parent_events(&fixture)
            .iter()
            .all(|event| event.kind != EventKind::UserAsk),
        "a waited check_task still woke the parent"
    );
}

#[tokio::test]
async fn two_background_children_wake_the_parent_once_in_spawn_order() {
    let (child, gate_a, gate_b) = held_pair();
    let fixture = Fixture::new("join", vec![Canned::Json(text_reply("Noted."))], child);
    fixture.add_session("parent");
    let (id_a, id_b) = spawn_held_pair(&fixture).await;
    gate_b.release();
    fixture.wait_status(&id_b, Status::Idle).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        parent_events(&fixture)
            .iter()
            .all(|event| event.kind != EventKind::UserAsk),
        "the first child to finish woke the parent"
    );
    gate_a.release();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let events = parent_events(&fixture);
        let asks: Vec<_> = events
            .iter()
            .filter(|event| event.kind == EventKind::UserAsk)
            .collect();
        if asks.len() == 1 {
            let ask = asks[0];
            assert_eq!(
                ask.body.get("text").and_then(|value| value.as_str()),
                Some("Subagents finished.")
            );
            assert_eq!(
                ask.body.get("silent").and_then(|value| value.as_bool()),
                Some(true)
            );
            let context = ask
                .body
                .get("context")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            assert!(
                context.contains(&format!(
                    "result: alpha result\nproof: alpha proof\nresume_from=\"{id_a}\"\n\nresult: beta result\nproof: beta proof\nresume_from=\"{id_b}\"\n"
                )),
                "{context}"
            );
            break;
        }
        assert!(asks.is_empty(), "more than one parent wake");
        assert!(Instant::now() < deadline, "the parent was not woken once");
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    fixture.wait_status("parent", Status::Idle).await;
    let asks = parent_events(&fixture)
        .into_iter()
        .filter(|event| event.kind == EventKind::UserAsk)
        .count();
    assert_eq!(asks, 1);
    assert_quiet(&fixture, &[&id_a, &id_b]);
    assert!(
        parent_transcript(
            &fixture,
            &[
                "alpha result",
                "alpha proof",
                "beta result",
                "beta proof",
                &id_a,
                &id_b
            ]
        ),
        "the parent transcript missed a sibling finish"
    );
}

#[tokio::test]
async fn a_timed_check_that_expires_still_wakes_when_the_children_finish() {
    let (child, gate_a, gate_b) = held_pair();
    let fixture = Fixture::new("expire", vec![Canned::Json(text_reply("Noted."))], child);
    fixture.add_session("parent");
    let (id_a, id_b) = spawn_held_pair(&fixture).await;
    let checked = fixture
        .runner
        .check_task("parent", &[&id_a, &id_b], Some(1))
        .await;
    let value: serde_json::Value = serde_json::from_str(&checked).expect("array");
    let rows = value.as_array().expect("two snapshots");
    assert_eq!(rows[0]["state"], "working", "{checked}");
    assert_eq!(rows[1]["state"], "working", "{checked}");
    gate_a.release();
    gate_b.release();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let events = parent_events(&fixture);
        let asks: Vec<_> = events
            .iter()
            .filter(|event| event.kind == EventKind::UserAsk)
            .collect();
        if asks.len() == 1 {
            assert_eq!(
                asks[0].body.get("text").and_then(|value| value.as_str()),
                Some("Subagents finished.")
            );
            assert_eq!(
                asks[0].body.get("silent").and_then(|value| value.as_bool()),
                Some(true)
            );
            break;
        }
        assert!(asks.is_empty(), "more than one parent wake");
        assert!(Instant::now() < deadline, "the parent was not woken once");
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

#[tokio::test]
async fn checking_one_child_still_delivers_the_sibling_that_already_finished() {
    let (child, gate_a, gate_b) = held_pair();
    let fixture = Fixture::new(
        "partial",
        vec![
            Canned::Json(text_reply("Noted.")),
            Canned::Json(text_reply("Noted again.")),
        ],
        child,
    );
    fixture.add_session("parent");
    let (id_a, id_b) = spawn_held_pair(&fixture).await;
    gate_a.release();
    fixture.wait_status(&id_a, Status::Idle).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        parent_events(&fixture)
            .iter()
            .all(|event| event.kind != EventKind::UserAsk),
        "the first child woke the parent before its sibling was checked"
    );
    let runner = Arc::clone(&fixture.runner);
    let waited = id_b.clone();
    let waiting =
        tokio::spawn(async move { runner.check_task("parent", &[&waited], Some(1)).await });
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let asks: Vec<_> = parent_events(&fixture)
            .into_iter()
            .filter(|event| event.kind == EventKind::UserAsk)
            .collect();
        if asks.len() == 1 {
            assert_eq!(
                asks[0].body.get("text").and_then(|value| value.as_str()),
                Some("Subagent finished.")
            );
            assert_eq!(
                asks[0].body.get("silent").and_then(|value| value.as_bool()),
                Some(true)
            );
            let context = asks[0]
                .body
                .get("context")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            assert!(context.contains("alpha result"), "{context}");
            assert!(!context.contains("beta result"), "{context}");
            break;
        }
        assert!(asks.is_empty(), "more than one parent wake");
        assert!(
            Instant::now() < deadline,
            "the finished sibling was not delivered"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    let checked = tokio::time::timeout(Duration::from_secs(3), waiting)
        .await
        .expect("the single-id wait hung")
        .expect("the wait task");
    let row: serde_json::Value = serde_json::from_str(&checked).expect("object");
    assert_eq!(row["id"], id_b, "{checked}");
    assert_eq!(row["state"], "working", "{checked}");
    gate_b.release();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let asks: Vec<_> = parent_events(&fixture)
            .into_iter()
            .filter(|event| event.kind == EventKind::UserAsk)
            .collect();
        if asks.len() == 2 {
            assert_eq!(
                asks[1].body.get("text").and_then(|value| value.as_str()),
                Some("Subagent finished.")
            );
            assert_eq!(
                asks[1].body.get("silent").and_then(|value| value.as_bool()),
                Some(true)
            );
            break;
        }
        assert!(asks.len() < 2, "more than two parent wakes");
        assert!(Instant::now() < deadline, "the checked child did not wake");
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn assert_quiet(fixture: &Fixture, ids: &[&str]) {
    let cards = serde_json::to_string(&fixture.runner.view("parent").expect("parent view").cards)
        .expect("cards serialize");
    assert!(!cards.contains("Subagent"), "{cards}");
    for id in ids {
        assert!(!cards.contains(id), "{cards}");
    }
}

fn parent_transcript(fixture: &Fixture, needles: &[&str]) -> bool {
    fixture.server.bodies().into_iter().any(|body| {
        body.contains("You are Kyoto Agent, a coding agent")
            && needles.iter().all(|needle| body.contains(needle))
    })
}

#[tokio::test]
async fn kill_task_closes_a_finished_child_and_refuses_resume() {
    let fixture = Fixture::new(
        "close",
        vec![Canned::Json(text_reply("Noted."))],
        vec![Canned::Json(finish_reply("alpha result", "alpha proof"))],
    );
    fixture.add_session("parent");
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent("parent", &spawn_args(serde_json::json!({})))
            .await,
    );
    fixture.wait_status(&id, Status::Idle).await;
    fixture.wait_status("parent", Status::Idle).await;
    let before = fixture.session_count();
    let missing = fixture.runner.kill_task("parent", "missing").await;
    assert_eq!(missing, "no task missing in this session");
    assert_eq!(fixture.session_count(), before);
    let closed = fixture.runner.kill_task("parent", &id).await;
    assert_eq!(closed, format!("closed {id}"));
    assert!(!fixture.root.join(&id).exists());
    assert!(fixture.runner.view(&id).is_err());
    assert_eq!(fixture.session_count(), before - 1);
    assert!(fixture.runner.view("parent").is_ok());
    let again = fixture
        .runner
        .spawn_subagent(
            "parent",
            &spawn_args(serde_json::json!({ "resume_from": id })),
        )
        .await;
    assert!(again.contains("resume_from needs"), "{again}");
    assert_eq!(fixture.session_count(), before - 1);
}

#[tokio::test]
async fn kill_task_drops_a_worktree_and_leaves_the_parent() {
    let fixture = Fixture::new(
        "close-tree",
        vec![Canned::Json(text_reply("Noted."))],
        vec![Canned::Json(finish_reply("tree result", "tree proof"))],
    );
    let workspace = fixture.root.join("repo");
    git_repo(&workspace);
    let session = Session::at(&fixture.root.join("session-parent"));
    session
        .create(&SessionMeta::new("parent", &workspace, "test/model", AT))
        .expect("session");
    fixture.runner.add_session(&session).expect("added");
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent(
                "parent",
                &spawn_args(serde_json::json!({ "isolation": "worktree" })),
            )
            .await,
    );
    fixture.wait_status(&id, Status::Idle).await;
    let child_workspace = Session::at(&fixture.root.join(&id))
        .meta()
        .expect("child meta")
        .workspace;
    assert!(Path::new(&child_workspace).exists());
    let closed = fixture.runner.kill_task("parent", &id).await;
    assert_eq!(closed, format!("closed {id}"));
    assert!(!Path::new(&child_workspace).exists());
    assert!(!fixture.root.join(&id).exists());
    assert!(workspace.join(".git").exists());
    assert!(fixture.runner.view("parent").is_ok());
}

#[tokio::test]
async fn a_child_meta_copies_the_parent_profile() {
    let fixture = Fixture::configured(
        "copy-profile",
        vec![Canned::Json(text_reply("Noted."))],
        vec![Canned::Json(finish_reply("child result", "child proof"))],
        "\n[profiles.review]\ntools = [\"finish\"]\n",
    );
    fixture.add_session("parent");
    Session::at(&fixture.root.join("session-parent"))
        .set_profile(Some("review"))
        .expect("profile");
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent("parent", &spawn_args(serde_json::json!({})))
            .await,
    );
    let meta = Session::at(&fixture.root.join(&id))
        .meta()
        .expect("child meta");
    assert_eq!(meta.profile.as_deref(), Some("review"));
    fixture.wait_status(&id, Status::Idle).await;
}

#[tokio::test]
async fn a_parent_with_no_profile_creates_a_child_with_no_profile() {
    let fixture = Fixture::new(
        "no-profile",
        vec![Canned::Json(text_reply("Noted."))],
        vec![Canned::Json(finish_reply("child result", "child proof"))],
    );
    fixture.add_session("parent");
    let id = child_id(
        &fixture
            .runner
            .spawn_subagent("parent", &spawn_args(serde_json::json!({})))
            .await,
    );
    let raw = fs::read_to_string(fixture.root.join(&id).join("meta.json")).expect("meta file");
    assert!(!raw.contains("\"profile\""));
    let meta = Session::at(&fixture.root.join(&id))
        .meta()
        .expect("child meta");
    assert!(meta.profile.is_none());
}

fn function_names(body: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(body).expect("request json");
    value
        .get("tools")
        .and_then(|tools| tools.as_array())
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            tool.get("function")
                .and_then(|function| function.get("name"))
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .collect()
}
