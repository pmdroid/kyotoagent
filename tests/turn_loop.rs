//! The turn loop, driven by a fake chat server and canned tool calls.
//!
//! Everything here goes through the public surface: a [`Runner`] owns the
//! config and the chat client, sessions are added to it, and turns are
//! started with [`Runner::ask`]. The fake server answers each completion with
//! a canned reply, so a test scripts a whole turn: the model calls a tool, the
//! gate asks, the test answers, and the turn finishes.
//!
//! What these tests are really about is the shape of the log and the view. A
//! write lands only after an allow, a question blocks until it is answered,
//! several tool calls then finish, a cancel kills the running command, and a
//! session waiting on a permission does not block its siblings.

use std::fs;
use std::io::ErrorKind;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kyotoagent::config::Config;
use kyotoagent::events::{AskBody, Event, EventKind, PermissionBody};
use kyotoagent::permit::Answer;
use kyotoagent::screen::{Phase, Status};
use kyotoagent::session::{Session, SessionMeta};
use kyotoagent::skills::{AGENTS_SKILLS, SKILL_FILE};
use kyotoagent::turn::{AskOutcome, Runner};
use kyotoagent::view::{self, CardKind};

mod pdf_fixture {
    include!("fixtures/pdf.rs");
}

const AT: &str = "2026-09-29T00:00:00.000Z";

async fn wait_for_proof_versions(
    fixture: &Fixture,
    id: &str,
    count: usize,
) -> Vec<kyotoagent::proof::ProofVersion> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let versions = kyotoagent::proof::versions(&fixture.events(id)).unwrap();
            if versions.len() >= count {
                return versions;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn artifact_optional_placeholders_do_not_require_closeout_or_commit_metadata() {
    let fixture = Fixture::new(
        "artifact-placeholders",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "attach_artifact",
                serde_json::json!({
                    "path": "cat.jpg", "file_id": "", "git_sha": "", "check": {"id": "", "attempt": 1}
                }),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "attach_artifact",
                serde_json::json!({
                    "path": "cat.jpg", "file_id": null, "git_sha": null, "check": null
                }),
            )])),
            Canned::Json(text_reply("Attached cat")),
        ],
    );
    let workspace = fixture.add_session("cat");
    image::RgbImage::new(2, 2)
        .save(workspace.join("cat.jpg"))
        .unwrap();
    fixture.ask("cat", "Attach cat");
    wait_for_proof_versions(&fixture, "cat", 1).await;
    fixture.wait_for_status("cat", Status::Idle).await;
    let events = fixture.events("cat");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == EventKind::Artifact)
            .count(),
        2,
        "{events:?}"
    );
    assert!(events
        .iter()
        .filter(|event| event.kind == EventKind::ToolResult)
        .all(|event| event.body["is_error"] != true));
}

#[tokio::test]
async fn repeated_tool_errors_remain_visible_while_the_model_thinks_and_clear_after_success() {
    let gate = HoldGate::new();
    let fixture = Fixture::new(
        "visible-tool-errors",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "attach_artifact",
                serde_json::json!({"path": "missing.jpg"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "attach_artifact",
                serde_json::json!({"path": "missing.jpg"}),
            )])),
            Canned::Hold {
                head: reasoning_event("recovering"),
                tail: text_reply("Done"),
                gate: Arc::clone(&gate),
            },
        ],
    );
    fixture.add_session("errors");
    fixture.ask("errors", "Attach missing image");
    fixture.wait_for_thinking("errors", "recovering").await;
    let view = fixture.view("errors");
    assert_eq!(view.phase, Some(Phase::Thinking));
    let action = view.action.unwrap();
    assert!(
        action.contains("attach_artifact failed (2 attempts)"),
        "{action}"
    );
    assert!(action.contains("missing.jpg"), "{action}");
    gate.release();
    fixture.wait_for_status("errors", Status::Idle).await;
    assert!(fixture.view("errors").action.is_none());
}

#[tokio::test]
async fn attachments_resolve_commit_metadata_and_reject_legacy_tools_and_bad_refs() {
    let fixture = Fixture::new(
        "artifact-sha",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "attach_proof",
                serde_json::json!({"path":"report.md"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "attach_artifact",
                serde_json::json!({"path":"report.md", "git_sha":"missing-commit"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "attach_artifact",
                serde_json::json!({"path":"report.md", "git_sha":"HEAD"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text":"Report attached"}),
            )])),
        ],
    );
    let workspace = fixture.add_session("sha");
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Kyoto",
            "-c",
            "user.email=kyoto@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "baseline",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .current_dir(&workspace)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    let sha = kyotoagent::proof::resolve_git_sha(&workspace, "HEAD").unwrap();
    fs::write(workspace.join("report.md"), "# Verification report").unwrap();
    fixture.ask("sha", "Attach report");
    wait_for_proof_versions(&fixture, "sha", 1).await;
    fixture.wait_for_status("sha", Status::Idle).await;
    let events = fixture.events("sha");
    let attachments: Vec<_> = events
        .iter()
        .filter(|event| event.kind == EventKind::Artifact)
        .collect();
    assert_eq!(attachments.len(), 1);
    let artifact: kyotoagent::events::ArtifactBody = attachments[0].body_as().unwrap();
    assert_eq!(artifact.file.git_sha.as_deref(), Some(sha.as_str()));
    assert!(events
        .iter()
        .any(|event| event.kind == EventKind::ToolResult
            && event.body["output"] == "unknown tool: attach_proof"));
    assert!(events
        .iter()
        .any(|event| event.kind == EventKind::ToolResult
            && event.body["output"]
                .as_str()
                .is_some_and(|output| output.contains("existing commit"))));
    fs::remove_dir_all(workspace).unwrap();
    let history = kyotoagent::proof::artifact_history(&events).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].proof.files[0].git_sha, Some(sha));
}

#[tokio::test]
async fn routine_failed_commands_do_not_create_artifact_versions() {
    let fixture = Fixture::new(
        "artifact-failures",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "run",
                serde_json::json!({"argv":["sh", "-c", "printf 'HTTP 504'; exit 1"]}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text":"GitHub request timed out"}),
            )])),
        ],
    );
    fixture.add_session("failure");
    fixture.ask("failure", "Look up PRs");
    fixture.wait_for_waiting_permission("failure").await;
    fixture.answer("failure", Answer::allow_once());
    fixture.wait_for_status("failure", Status::Idle).await;
    let events = fixture.events("failure");
    assert!(kyotoagent::proof::artifact_history(&events)
        .unwrap()
        .is_empty());
    assert!(!events.iter().any(|event| event.kind == EventKind::Artifact));
    assert_eq!(
        kyotoagent::proof::versions(&events).unwrap()[0]
            .proof
            .failures[0]
            .exit,
        1
    );
}

#[tokio::test]
async fn attached_proof_files_keep_original_bytes_and_the_response_stays_the_result() {
    let fixture = Fixture::new(
        "proof-files",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "attach_artifact",
                serde_json::json!({"path":"report.md"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text":"First response","proof":"Agent-written context"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "attach_artifact",
                serde_json::json!({"path":"report.md"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text":"Second response"}),
            )])),
        ],
    );
    let workspace = fixture.add_session("proof");
    fs::write(workspace.join("report.md"), b"# First report\n").unwrap();
    fixture.ask("proof", "first");
    let first = wait_for_proof_versions(&fixture, "proof", 1).await;
    fixture.wait_for_status("proof", Status::Idle).await;
    assert_eq!(first[0].response.as_ref().unwrap().text, "First response");
    assert_eq!(first[0].proof.text, "Agent-written context");
    assert!(first[0].proof.items.is_empty());
    let session = Session::at(&fixture.root.join("session-proof"));
    assert_eq!(
        fs::read(
            session
                .dir()
                .join("proof")
                .join(&first[0].proof.files[0].id)
        )
        .unwrap(),
        b"# First report\n"
    );
    fs::write(workspace.join("report.md"), b"# Second report\n").unwrap();
    fixture.ask("proof", "second");
    let versions = wait_for_proof_versions(&fixture, "proof", 2).await;
    assert_eq!(
        versions[1].response.as_ref().unwrap().text,
        "Second response"
    );
    assert_ne!(versions[0].proof.files[0].id, versions[1].proof.files[0].id);
    assert_eq!(versions[1].proof.files[0].media_type, "text/markdown");
    assert_eq!(
        fs::read(
            session
                .dir()
                .join("proof")
                .join(&versions[0].proof.files[0].id)
        )
        .unwrap(),
        b"# First report\n"
    );
    assert_eq!(
        fs::read(
            session
                .dir()
                .join("proof")
                .join(&versions[1].proof.files[0].id)
        )
        .unwrap(),
        b"# Second report\n"
    );
}

#[tokio::test]
async fn missing_proof_files_return_a_tool_error_and_the_agent_can_correct_the_path() {
    let fixture = Fixture::new(
        "proof-retry",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "attach_artifact",
                serde_json::json!({"path":"missing.bin"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "attach_artifact",
                serde_json::json!({"path":"actual.bin"}),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({"text":"A real file"}),
            )])),
        ],
    );
    let workspace = fixture.add_session("proof");
    fs::write(workspace.join("actual.bin"), b"\0\xff\x01").unwrap();
    fixture.ask("proof", "attach evidence");
    let versions = wait_for_proof_versions(&fixture, "proof", 1).await;
    assert_eq!(versions[0].proof.files.len(), 1);
    assert_eq!(versions[0].proof.files[0].name, "actual.bin");
    assert_eq!(
        versions[0].proof.files[0].media_type,
        "application/octet-stream"
    );
    let output = fixture
        .events("proof")
        .into_iter()
        .find(|event| {
            event.kind == EventKind::ToolResult && event.body["tool"] == "attach_artifact"
        })
        .unwrap();
    assert!(output.body["output"]
        .as_str()
        .unwrap()
        .contains("missing.bin"));
    assert!(!output.body["output"].as_str().unwrap().starts_with('{'));
}

#[tokio::test]
async fn reload_recovers_an_incomplete_event_tail_without_losing_complete_events() {
    let fixture = Fixture::new("partial-tail", vec![Canned::Json(text_reply("unused"))]);
    fixture.add_session("partial");
    let session = Session::at(&fixture.root.join("session-partial"));
    let event = Event::new("e1", AT, "t7", EventKind::UserAsk)
        .with_body(&serde_json::json!({"text":"keep this request"}))
        .unwrap();
    session.append(&event).unwrap();
    session
        .update(|meta| {
            meta.status = Status::Working;
            true
        })
        .unwrap();
    let incomplete = b"{\"id\":\"e2\",\"body\":";
    fs::OpenOptions::new()
        .append(true)
        .open(session.events_path())
        .unwrap()
        .write_all(incomplete)
        .unwrap();
    fixture
        .runner
        .reload(&[session.dir().to_path_buf()])
        .unwrap();
    let events = session.events().unwrap();
    assert_eq!(events[0], event);
    assert!(events
        .iter()
        .any(|event| event.kind == EventKind::Result && event.turn_id == "t7"));
    assert_eq!(session.meta().unwrap().status, Status::Idle);
    let archived = fs::read_dir(session.dir())
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("events.interrupted-")
        })
        .unwrap();
    assert_eq!(fs::read(archived.path()).unwrap(), incomplete);
    fixture
        .runner
        .reload(&[session.dir().to_path_buf()])
        .unwrap();
    assert_eq!(session.events().unwrap(), events);
}

#[tokio::test]
async fn reload_isolates_a_corrupt_log_and_loads_healthy_sessions() {
    let fixture = Fixture::new("corrupt-log", vec![Canned::Json(text_reply("unused"))]);
    fixture.add_session("bad");
    fixture.add_session("good");
    let bad = Session::at(&fixture.root.join("session-bad"));
    bad.update(|meta| {
        meta.status = Status::Working;
        true
    })
    .unwrap();
    fs::write(bad.events_path(), b"broken event\n").unwrap();
    fixture
        .runner
        .reload(&[bad.dir().to_path_buf(), fixture.root.join("session-good")])
        .unwrap();
    assert_eq!(fs::read(bad.events_path()).unwrap(), b"broken event\n");
    assert!(fixture.runner.view("good").is_ok());
    assert!(matches!(
        fixture.runner.view("bad"),
        Err(kyotoagent::session::SessionError::MissingMeta { .. })
    ));
}

#[tokio::test]
async fn interrupted_turn_reloads_its_collected_files_in_the_original_turn_version() {
    let fixture = Fixture::new(
        "proof-interrupted",
        vec![Canned::Json(text_reply("unused"))],
    );
    let workspace = fixture.add_session("proof");
    fs::write(workspace.join("report.md"), "# Real evidence\n").unwrap();
    let session = Session::at(&fixture.root.join("session-proof"));
    let file = kyotoagent::tools::Tools::at(&session)
        .unwrap()
        .attach_artifact("t7", "report.md")
        .unwrap()
        .unwrap();
    session
        .append(
            &Event::new("e1", AT, "t7", EventKind::UserAsk)
                .with_body(&serde_json::json!({"text":"collect proof"}))
                .unwrap(),
        )
        .unwrap();
    session
        .append(
            &Event::new("e2", AT, "t7", EventKind::ToolResult)
                .with_body(&kyotoagent::events::ToolResultBody {
                    is_error: false,
                    images: Vec::new(),
                    tool: "attach_proof".into(),
                    output: serde_json::to_string(&file).unwrap(),
                })
                .unwrap(),
        )
        .unwrap();
    session
        .append(
            &Event::new("e3", AT, "t7", EventKind::CloseoutRun)
                .with_body(&kyotoagent::events::CloseoutRunBody {
                    transcript: None,
                    argv: Vec::new(),
                    timed_out: false,
                    truncated: false,
                    id: "lint".into(),
                    attempt: 1,
                    exit: 1,
                    tail: "failed check".into(),
                })
                .unwrap(),
        )
        .unwrap();
    session
        .update(|meta| {
            meta.status = Status::Working;
            true
        })
        .unwrap();
    fixture
        .runner
        .reload(&[session.dir().to_path_buf()])
        .unwrap();
    let versions = kyotoagent::proof::versions(&session.events().unwrap()).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].turn_id, "t7");
    assert_eq!(
        versions[0].response.as_ref().unwrap().text,
        "The server stopped."
    );
    assert_eq!(versions[0].proof.files, vec![file.clone()]);
    assert_eq!(versions[0].proof.items[0].outcome, "failed");
    assert_eq!(
        fs::read(session.dir().join("proof").join(file.id)).unwrap(),
        b"# Real evidence\n"
    );
    fixture
        .runner
        .reload(&[session.dir().to_path_buf()])
        .unwrap();
    assert_eq!(
        kyotoagent::proof::versions(&session.events().unwrap())
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_queued_image_only_message_keeps_its_pixels_when_the_turn_starts() {
    let gate = HoldGate::new();
    let fixture = Fixture::new(
        "image-queue",
        vec![
            Canned::Hold {
                head: reasoning_event("hold"),
                tail: content_event("first done"),
                gate: Arc::clone(&gate),
            },
            Canned::Json(text_reply("image done")),
        ],
    );
    fixture.add_session("image-queue");
    fixture.ask("image-queue", "first");
    fixture.wait_for_thinking("image-queue", "hold").await;
    let image =
        kyotoagent::attachment::ImageAttachment::from_bytes("dog.png", kyotoagent::splash::PNG)
            .unwrap();
    assert!(matches!(
        fixture
            .runner
            .ask_with_images("image-queue", "", None, vec![image.clone()])
            .unwrap(),
        AskOutcome::Queued(_)
    ));
    gate.release();
    fixture.wait_for_cards("image-queue", 4).await;
    fixture.wait_for_status("image-queue", Status::Idle).await;
    let events = Session::at(&fixture.root.join("session-image-queue"))
        .events()
        .unwrap();
    let asks: Vec<_> = events
        .iter()
        .filter(|event| event.kind == EventKind::UserAsk)
        .map(|event| event.body_as::<AskBody>().unwrap())
        .collect();
    assert_eq!(asks.last().unwrap().images, vec![image.clone()]);
    let posted: serde_json::Value =
        serde_json::from_str(fixture._server.bodies.lock().unwrap().last().unwrap()).unwrap();
    assert!(posted["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["content"][1]["image_url"]["url"] == image.data_url()));
}

#[tokio::test]
async fn file_read_images_reach_the_next_request_and_survive_projection() {
    let fixture = Fixture::new(
        "read-image",
        vec![
            Canned::Json(tool_call_reply(vec![
                ("read_file", serde_json::json!({"path":"picture.txt"})),
                (
                    "read_file",
                    serde_json::json!({"path":"document.bin", "pages":"2"}),
                ),
                ("read_file", serde_json::json!({"path":"binary.txt"})),
                (
                    "read_file",
                    serde_json::json!({"path":"document.bin", "pages":2}),
                ),
            ])),
            Canned::Json(text_reply("Images inspected.")),
        ],
    );
    let workspace = fixture.add_session("read-image");
    fs::write(workspace.join("picture.txt"), kyotoagent::splash::PNG).unwrap();
    fs::write(
        workspace.join("document.bin"),
        pdf_fixture::document(&["Alpha document", "Beta document"]),
    )
    .unwrap();
    fs::write(workspace.join("binary.txt"), b"\0\xffhidden-binary-bytes").unwrap();
    fixture.ask("read-image", "Inspect the files.");
    fixture.wait_for_turn_to_start("read-image").await;
    fixture.wait_for_status("read-image", Status::Idle).await;
    let requests = fixture.chat_requests();
    let sent = &requests[1]["messages"];
    let parts = sent.as_array().unwrap().last().unwrap()["content"]
        .as_array()
        .unwrap();
    assert_eq!(
        parts
            .iter()
            .filter(|part| part["type"] == "image_url")
            .count(),
        2
    );
    assert!(!sent.to_string().contains("hidden-binary-bytes"));
    let events = fixture.events("read-image");
    let results: Vec<_> = events
        .iter()
        .filter(|event| event.kind == EventKind::ToolResult)
        .map(|event| {
            event
                .body_as::<kyotoagent::events::ToolResultBody>()
                .unwrap()
        })
        .collect();
    assert_eq!(
        results
            .iter()
            .map(|result| result.images.len())
            .sum::<usize>(),
        2
    );
    assert!(results[2].output.contains("binary"));
    assert!(results[3].output.contains("pages must be a string"));
    let restored = kyotoagent::compact::projected_messages("system", &events, "/w");
    let restored = serde_json::to_value(restored).unwrap();
    let restored_parts = restored
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flat_map(|parts| parts.iter())
        .filter(|part| part["type"] == "image_url")
        .count();
    assert_eq!(restored_parts, 2);
}

#[tokio::test]
async fn an_image_only_ask_persists_real_image_data_and_projects_it_to_the_provider() {
    let fixture = Fixture::new(
        "image-only",
        vec![Canned::Json(
            "{\"choices\":[{\"message\":{\"content\":\"A dog.\"}}]}".into(),
        )],
    );
    fixture.add_session("image-only");
    let image =
        kyotoagent::attachment::ImageAttachment::from_bytes("dog.png", kyotoagent::splash::PNG)
            .unwrap();
    let outcome = fixture
        .runner
        .ask_with_images("image-only", "", Some(true), vec![image.clone()])
        .unwrap();
    assert!(matches!(outcome, AskOutcome::Started(_)));
    fixture.wait_for_turn_to_start("image-only").await;
    fixture.wait_for_status("image-only", Status::Idle).await;
    let session = Session::at(&fixture.root.join("session-image-only"));
    let events = session.events().unwrap();
    let ask = events
        .iter()
        .find(|event| event.kind == EventKind::UserAsk)
        .unwrap()
        .body_as::<AskBody>()
        .unwrap();
    assert_eq!(ask.images, vec![image.clone()]);
    assert_eq!(
        fixture.view("image-only").cards[0].body["images"][0]["data"],
        image.data
    );
    let messages = kyotoagent::compact::projected_messages("system", &events, "/w");
    let sent = serde_json::to_value(&messages[1]).unwrap();
    assert_eq!(sent["content"][1]["image_url"]["url"], image.data_url());
    assert!(kyotoagent::compact::estimate_tokens(&messages) >= 16384);
    let compact =
        serde_json::to_value(kyotoagent::compact::compact_request_messages(&events)).unwrap();
    assert_eq!(
        compact[1]["content"][1]["image_url"]["url"],
        image.data_url()
    );
    let mut compacted = events.clone();
    compacted.push(
        Event::new("compact-image", AT, "t-image", EventKind::Compact)
            .with_body(&kyotoagent::events::CompactBody {
                summary: "A dog image was uploaded.".into(),
                through_event_id: events.last().unwrap().id.clone(),
            })
            .unwrap(),
    );
    let restored = serde_json::to_value(kyotoagent::compact::projected_messages(
        "system", &compacted, "/w",
    ))
    .unwrap();
    assert_eq!(
        restored[1]["content"][1]["image_url"]["url"],
        image.data_url()
    );
    let posted: serde_json::Value =
        serde_json::from_str(&fixture._server.bodies.lock().unwrap()[0]).unwrap();
    assert_eq!(
        posted["messages"][1]["content"][1]["image_url"]["url"],
        image.data_url()
    );
}

/// What the fake server answers with.
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

    fn is_released(&self) -> bool {
        self.released.load(Ordering::Relaxed)
    }
}

#[derive(Clone)]
enum Canned {
    Json(String),
    Status(u16, String),
    Hold {
        head: String,
        tail: String,
        gate: Arc<HoldGate>,
    },
}

/// A local HTTP server that answers each completion with a canned reply from a
/// queue. The last reply repeats, so a turn that asks more questions than the
/// test scripted still gets an answer.
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
                            serve_one(stream, &replies, &bodies, &stop);
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
    stop: &Arc<AtomicBool>,
) {
    let Some((path, body)) = read_request(&mut stream) else {
        return;
    };
    let request_body = body.clone();
    if path.contains("/models") {
        let catalog = serde_json::json!({
            "data": [{ "id": "test/model", "context_length": 200000 }]
        })
        .to_string();
        respond(&mut stream, 200, &catalog);
        return;
    }
    bodies
        .lock()
        .expect("the bodies are not poisoned")
        .push(body);
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
        Canned::Hold { head, tail, gate } => hold_stream(&mut stream, &head, &tail, &gate, stop),
    }
}

fn hold_stream(stream: &mut TcpStream, head: &str, tail: &str, gate: &HoldGate, stop: &AtomicBool) {
    let opening = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
    let _ = stream.write_all(opening.as_bytes());
    write_chunk(stream, head);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    loop {
        if stop.load(Ordering::Relaxed) || gate.is_released() {
            break;
        }
        let mut buf = [0_u8; 1];
        match stream.peek(&mut buf) {
            Ok(0) => return,
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(_) => return,
            Ok(_) => {}
        }
        if stop.load(Ordering::Relaxed) || gate.is_released() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if stop.load(Ordering::Relaxed) {
        return;
    }
    write_chunk(stream, tail);
    if !head.contains("[DONE]") && !tail.contains("[DONE]") {
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

fn sse_data(value: serde_json::Value) -> String {
    format!("data: {value}\n\n")
}

fn reasoning_event(text: &str) -> String {
    sse_data(serde_json::json!({
        "choices": [{ "index": 0, "delta": { "reasoning_content": text } }]
    }))
}

fn content_event(text: &str) -> String {
    sse_data(serde_json::json!({
        "choices": [{ "index": 0, "delta": { "content": text } }]
    }))
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

/// A text reply: the model finished with prose and no tool call.
fn text_reply(text: &str) -> String {
    serde_json::json!({
        "choices": [{
            "message": { "role": "assistant", "content": text }
        }]
    })
    .to_string()
}

fn tool_call_raw(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": name, "arguments": arguments }
                }]
            }
        }]
    })
    .to_string()
}

/// A tool call reply: the model wants tools run. The arguments are the JSON
/// string the server sends, so a value here is the arguments object.
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

/// A runner, a fake server, and a root for the sessions. The temporary
/// directories go when the fixture does, so a failed test leaves nothing
/// behind for the next run to trip over.
struct Fixture {
    runner: Arc<Runner>,
    _server: FakeServer,
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
            std::env::temp_dir().join(format!("kyotoagent-turn-{}-{name}", std::process::id()));
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
            _server: server,
            root,
        }
    }

    /// Add a session to the runner and return its workspace.
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
            .answer_question(id, text)
            .expect("the answer lands");
    }

    fn cancel(&self, id: &str) {
        self.runner.cancel(id);
    }

    /// Wait until the session's status is `status`, polling the view.
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

    /// Wait until the session has at least `count` cards, and return them.
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

    fn has_waiting_permission(&self, id: &str) -> bool {
        let view = self.view(id);
        view.status == Status::Waiting
            && view
                .cards
                .iter()
                .any(|card| card.kind == CardKind::Permission && card.body["decision"].is_null())
    }

    async fn wait_for_waiting_permission(&self, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.has_waiting_permission(id) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the session did not wait on a permission"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Wait until the turn has started: the ask card is on screen. A session
    /// starts `Idle`, so waiting for `Idle` alone would race the turn task.
    async fn wait_for_turn_to_start(&self, id: &str) {
        self.wait_for_cards(id, 1).await;
    }

    async fn wait_for_thinking(&self, id: &str, needle: &str) -> view::View {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let view = self.view(id);
            if view.phase == Some(Phase::Thinking)
                && view
                    .thinking
                    .as_deref()
                    .is_some_and(|text| text.contains(needle))
            {
                return view;
            }
            assert!(
                Instant::now() < deadline,
                "thoughts did not arrive: {:?}",
                view.thinking
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn chat_bodies(&self) -> Vec<String> {
        self._server
            .bodies
            .lock()
            .expect("the bodies are not poisoned")
            .clone()
    }

    fn chat_requests(&self) -> Vec<serde_json::Value> {
        self.chat_bodies()
            .iter()
            .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
            .filter(|request| request["messages"].is_array())
            .collect()
    }

    async fn wait_for_permission_count(&self, id: &str, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let permissions = self
                .events(id)
                .iter()
                .filter(|event| event.kind == EventKind::Permission)
                .count();
            if permissions == count && self.has_waiting_permission(id) {
                return;
            }
            assert!(Instant::now() < deadline, "permission {count} did not open");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn events(&self, id: &str) -> Vec<Event> {
        Session::at(&self.root.join(format!("session-{id}")))
            .events()
            .expect("the log reads")
    }

    async fn wait_for_tool_output(&self, id: &str, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let found = self.events(id).iter().any(|event| {
                event.kind == EventKind::ToolResult
                    && event.body["output"]
                        .as_str()
                        .is_some_and(|output| output.contains(needle))
            });
            if found {
                return;
            }
            assert!(Instant::now() < deadline, "the log never carried {needle}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// A write lands only after an allow, and the cards show a result and no tool
/// name.
#[tokio::test]
async fn a_write_lands_only_after_an_allow_and_the_cards_show_no_tool_name() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "notes.md", "contents": "hello" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Created notes.md.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("allow-write", replies);
    let workspace = fixture.add_session("91bc");

    fixture.ask("91bc", "Create notes.md.");

    // The permission is on screen: an ask and a permission card.
    let cards = fixture.wait_for_cards("91bc", 2).await;
    assert_eq!(cards[1].kind, CardKind::Permission);

    // The file is not there yet.
    assert!(
        !workspace.join("notes.md").exists(),
        "the write waits for the allow"
    );

    // Allow the write.
    fixture.answer("91bc", Answer::allow_once());

    // The turn finishes.
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    // The file is there, with the bytes the model sent.
    assert_eq!(
        fs::read_to_string(workspace.join("notes.md")).expect("the file reads"),
        "hello"
    );

    let view = fixture.view("91bc");
    let kinds: Vec<CardKind> = view.cards.iter().map(|card| card.kind).collect();
    assert_eq!(
        kinds,
        vec![CardKind::Ask, CardKind::Result, CardKind::Proof]
    );
    let json = serde_json::to_string(&view.cards).expect("cards serialize");
    assert!(!json.contains("write_file"), "no tool name: {json}");
    assert!(!json.contains("read_file"), "no tool name: {json}");
    assert!(!json.contains("run"), "no tool name: {json}");
    assert!(!json.contains("finish"), "no tool name: {json}");
}

/// A model response with text and no tool call becomes the result.
#[tokio::test]
async fn a_text_response_with_no_tool_call_becomes_the_result() {
    let replies = vec![Canned::Json(text_reply("The readme names the binary."))];
    let fixture = Fixture::new("text-result", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "What does the readme say?");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    assert_eq!(view.cards.len(), 2, "an ask and a result");
    assert_eq!(view.cards[1].kind, CardKind::Result);
    assert_eq!(
        view.cards[1].body["text"],
        serde_json::Value::from("The readme names the binary.")
    );
    assert!(view.cards.iter().all(|card| card.kind != CardKind::Proof));
    assert!(fixture
        .events("91bc")
        .iter()
        .all(|event| event.kind != EventKind::Proof));
}

/// `ask` with two choices blocks. The answer is in the log, and the card leaves.
#[tokio::test]
async fn ask_with_two_choices_blocks_and_the_answer_shows_on_the_card() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({
                "text": "Which title?",
                "choices": ["Kyoto Agent", "Kyoto Agent CLI"]
            }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("question", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "Name the binary.");

    // The question is on screen with its two choices and no answer yet.
    let cards = fixture.wait_for_cards("91bc", 2).await;
    assert_eq!(cards[1].kind, CardKind::Question);
    assert_eq!(
        cards[1].body["choices"].as_array().expect("choices").len(),
        2
    );
    assert!(cards[1].body["answer"].is_null(), "no answer yet");

    // Answer the question.
    fixture.answer_question("91bc", "Kyoto Agent");

    // The turn finishes.
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    let question = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Question)
        .expect("the question stays");
    assert_eq!(
        question.body["text"],
        serde_json::Value::from("Which title?")
    );
    assert_eq!(
        question.body["answer"],
        serde_json::Value::from("Kyoto Agent")
    );
    assert!(question.body["choices"]
        .as_array()
        .expect("choices")
        .is_empty());
    let answer = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Answer)
        .expect("the answer card");
    assert_eq!(answer.body["text"], serde_json::Value::from("Kyoto Agent"));
    assert_ne!(answer.body["text"], serde_json::Value::from("1"));
    assert!(view.cards.iter().any(|card| card.kind == CardKind::Result));
    assert!(fixture.events("91bc").iter().any(|event| {
        event.kind == EventKind::QuestionAnswer && event.body["answer"] == "Kyoto Agent"
    }));
}

#[tokio::test]
async fn five_reads_then_finish_goes_idle_with_the_finish_text() {
    let read = || {
        Canned::Json(tool_call_reply(vec![(
            "read_file",
            serde_json::json!({ "path": "notes.md" }),
        )]))
    };
    let replies = vec![
        read(),
        read(),
        read(),
        read(),
        read(),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Read notes.md five times.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("five-reads", replies);
    let workspace = fixture.add_session("91bc");
    fs::write(workspace.join("notes.md"), "hello").expect("the file writes");

    fixture.ask("91bc", "Read notes.md a few times.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let reads = fixture
        .events("91bc")
        .iter()
        .filter(|event| event.kind == EventKind::ToolCall && event.body["tool"] == "read_file")
        .count();
    assert_eq!(reads, 5);

    let view = fixture.view("91bc");
    assert_eq!(view.status, Status::Idle);
    let result = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Result)
        .expect("a result card");
    assert_eq!(
        result.body["text"],
        serde_json::Value::from("Read notes.md five times.")
    );
}

/// Cancel during `run` kills the process and ends that session with a stopped
/// result.
#[tokio::test]
async fn cancel_during_run_kills_the_process_and_ends_that_session() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run",
            serde_json::json!({ "argv": ["sh", "-c", "sleep 30 & wait"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({"text": "Recovered."}),
        )])),
    ];
    let fixture = Fixture::new("cancel", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "Sleep for a while.");

    // The command asks for permission.
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());

    // Give the command a moment to start, then cancel the turn.
    tokio::time::sleep(Duration::from_millis(300)).await;
    fixture.cancel("91bc");

    // The turn ends quickly: the process was killed, not left to time out.
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    let result = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Result)
        .expect("a result card");
    assert_eq!(result.body["text"], serde_json::Value::from("Stopped."));
    assert!(view.cards.iter().all(|card| card.kind != CardKind::Proof));
    assert!(fixture
        .events("91bc")
        .iter()
        .any(|event| event.kind == EventKind::Proof));
    fixture.ask("91bc", "Continue after cancellation.");
    fixture.wait_for_status("91bc", Status::Idle).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(fixture
        .events("91bc")
        .iter()
        .any(|event| event.body["text"] == "Recovered."));
}

/// A second message to a busy session is refused.
#[tokio::test]
async fn a_second_message_to_a_busy_session_is_refused() {
    // The first reply is a question, the second is a finish, so the turn can
    // complete once the question is answered.
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({ "text": "Which?", "choices": ["a", "b"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("busy", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "First ask.");
    fixture.wait_for_cards("91bc", 2).await;

    // The session is busy on the question, so a second ask is refused.
    let error = fixture
        .runner
        .ask("91bc", "Second ask.")
        .expect_err("a busy session refuses a second message");
    assert!(
        error.to_string().contains("already working"),
        "the error says the session is busy: {error}"
    );

    // The first turn is still waiting on the question.
    assert_eq!(fixture.view("91bc").status, Status::Waiting);

    // Answer the question, and the turn finishes.
    fixture.answer_question("91bc", "a");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
}

/// Reload with a pending permission leaves that session waiting, and a second
/// session can finish a turn before the first is answered.
#[tokio::test]
async fn reload_with_a_pending_permission_leaves_it_waiting_and_a_sibling_finishes() {
    // Build a session that is waiting on a permission, as a server that was
    // interrupted mid-turn would leave it.
    let root = std::env::temp_dir().join(format!("kyotoagent-turn-reload-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let workspace_a = root.join("w-91bc");
    fs::create_dir_all(&workspace_a).expect("the workspace exists");
    let session_a = Session::at(&root.join("session-91bc"));
    session_a
        .create(&SessionMeta::new("91bc", &workspace_a, "test/model", AT))
        .expect("the session is created");
    let ask = Event::new("e1", AT, "t1", EventKind::UserAsk)
        .with_body(&AskBody {
            images: Vec::new(),
            text: "Create notes.md.".into(),
            context: String::new(),
            skill: String::new(),
            silent: false,
        })
        .expect("an ask body");
    session_a.append(&ask).expect("the ask is appended");
    let permission = Event::new("e2", AT, "t1", EventKind::Permission)
        .with_body(&PermissionBody::write(
            "Create notes.md",
            workspace_a.join("notes.md").to_str().expect("a path"),
            &["+hello"],
        ))
        .expect("a permission body");
    session_a
        .append(&permission)
        .expect("the permission is appended");
    let mut meta = session_a.meta().expect("meta reads");
    meta.status = Status::Waiting;
    session_a.write_meta(&meta).expect("the meta is written");

    // A second session, clean.
    let workspace_b = root.join("w-3f2a");
    fs::create_dir_all(&workspace_b).expect("the workspace exists");
    let session_b = Session::at(&root.join("session-3f2a"));
    session_b
        .create(&SessionMeta::new("3f2a", &workspace_b, "test/model", AT))
        .expect("the session is created");

    // Reload both into a runner. The first is still waiting on its permission.
    let replies = vec![Canned::Json(tool_call_reply(vec![(
        "finish",
        serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
    )]))];
    let server = FakeServer::start(replies);
    let config = Config::from_toml(&format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
        server.base_url()
    ))
    .expect("the config parses");
    let runner = Runner::new(&config).expect("the runner is built");
    runner
        .reload(&[root.join("session-91bc"), root.join("session-3f2a")])
        .expect("the sessions reload");

    // The first session is still waiting, with its permission card.
    let view_a = runner.view("91bc").expect("the view reads");
    assert_eq!(view_a.status, Status::Waiting);
    assert_eq!(view_a.cards.len(), 2, "an ask and the open permission");
    assert_eq!(view_a.cards[1].kind, CardKind::Permission);
    assert!(view_a.cards[1].body["decision"].is_null());

    // The second session finishes a turn before the first is answered.
    runner.ask("3f2a", "Finish this.").expect("the turn starts");
    // Wait for the turn to start, then for it to finish.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !runner
            .view("3f2a")
            .expect("the view reads")
            .cards
            .is_empty()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the second session did not start"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if runner.view("3f2a").expect("the view reads").status == Status::Idle {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the second session did not finish"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The first session is still waiting, and its permission is still open.
    assert_eq!(
        runner.view("91bc").expect("the view reads").status,
        Status::Waiting
    );
    assert_eq!(
        runner.view("91bc").expect("the view reads").cards[1].kind,
        CardKind::Permission
    );

    let _ = server;
    let _ = fs::remove_dir_all(&root);
}

/// Two sessions run at the same time: one waits on a permission while the other
/// finishes a turn.
#[tokio::test]
async fn two_sessions_run_at_the_same_time() {
    // The first reply is a write (which asks for permission), the second is a
    // finish. The first session gets the write, the second gets the finish.
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "notes.md", "contents": "hello" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("two-sessions", replies);
    fixture.add_session("91bc");
    fixture.add_session("3f2a");

    // The first session starts and blocks on a write permission.
    fixture.ask("91bc", "Create notes.md.");
    fixture.wait_for_cards("91bc", 2).await;
    assert_eq!(fixture.view("91bc").status, Status::Waiting);

    // The second session starts and finishes while the first is still waiting.
    fixture.ask("3f2a", "Finish this.");
    fixture.wait_for_turn_to_start("3f2a").await;
    fixture.wait_for_status("3f2a", Status::Idle).await;

    // The first session is still waiting.
    assert_eq!(fixture.view("91bc").status, Status::Waiting);

    // Answer the first session, and it finishes too.
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
}

/// A failed model request ends the turn with a one-sentence result.
#[tokio::test]
async fn a_failed_model_request_ends_the_turn_with_a_one_sentence_result() {
    let replies = vec![Canned::Status(
        500,
        "{\"error\":{\"message\":\"model is overloaded\"}}".into(),
    )];
    let fixture = Fixture::new("model-error", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "Do something.");
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
        text.contains("500"),
        "the sentence names the status: {text}"
    );
    assert!(
        text.contains("model is overloaded"),
        "the sentence names the error: {text}"
    );
    assert!(!text.contains('\n'), "one sentence: {text}");
    assert!(view.cards.iter().all(|card| card.kind != CardKind::Proof));
    assert!(fixture
        .events("91bc")
        .iter()
        .all(|event| event.kind != EventKind::Proof));
}

/// The log keeps every write and run tool result, including exit code and
/// output, for the proof issue.
#[tokio::test]
async fn the_log_keeps_every_write_and_run_tool_result() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "notes.md", "contents": "hello" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "run",
            serde_json::json!({ "argv": ["sh", "-c", "echo out; exit 3"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("log-results", replies);
    let workspace = fixture.add_session("91bc");

    fixture.ask("91bc", "Write and run.");
    fixture.wait_for_waiting_permission("91bc").await;
    fixture.answer("91bc", Answer::allow_once());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let waiting_run = fixture.view("91bc").cards.iter().any(|card| {
            card.kind == CardKind::Permission
                && card.body["decision"].is_null()
                && card
                    .body
                    .get("argv")
                    .map(|value| !value.is_null())
                    .unwrap_or(false)
        });
        if waiting_run {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the run permission did not appear"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    // The log has the write tool result and the run tool result.
    let events = fixture
        .runner
        .view("91bc")
        .expect("the view reads")
        .revision;
    assert!(events >= 8, "the log has every event: {events}");

    let log = fs::read_to_string(fixture.root.join("session-91bc").join("events.jsonl"))
        .expect("the log reads");
    assert!(log.contains("write_file"), "the write is in the log");
    assert!(log.contains("Created"), "the write result is in the log");
    assert!(log.contains("run"), "the run is in the log");
    assert!(log.contains("exited 3"), "the run exit code is in the log");
    assert!(log.contains("out"), "the run output is in the log");

    // The file was written.
    assert_eq!(
        fs::read_to_string(workspace.join("notes.md")).expect("the file reads"),
        "hello"
    );
}

/// A turn that writes `README.md` and finishes lists that path on the proof card.
#[tokio::test]
async fn a_turn_that_writes_readme_lists_it_on_the_proof_card() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "README.md", "contents": "# Kyoto Agent" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Created README.md.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("proof-write", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "Create README.md.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    let proof = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Proof)
        .expect("a proof card");
    assert_eq!(
        proof.body["text"],
        serde_json::json!("cargo test passed."),
        "the proof card is the agent's sentence"
    );
    assert!(proof.body.get("wrote").is_none());
    let event = fixture
        .events("91bc")
        .into_iter()
        .find(|event| event.kind == EventKind::Proof)
        .expect("a proof event");
    assert_eq!(event.body["wrote"], serde_json::json!(["README.md"]));
}

/// In a git repo, the proof event keeps `git status --short` and
/// `git diff --stat`. The card shows the agent's proof.
#[tokio::test]
async fn in_a_git_repo_the_proof_event_keeps_git_status_and_diffstat() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "README.md", "contents": "# Kyoto Agent" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Created README.md.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("proof-git", replies);
    let workspace = fixture.add_session("91bc");

    // Make the workspace a git repo with a tracked README.md, so the agent's
    // write is a modification git can see.
    fs::write(workspace.join("README.md"), "before").expect("a file");
    git(&workspace, &["init"]);
    git(&workspace, &["add", "."]);
    git(
        &workspace,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-m",
            "init",
        ],
    );

    fixture.ask("91bc", "Update README.md.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    let card = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Proof)
        .expect("a proof card");
    assert_eq!(card.body["text"], serde_json::json!("cargo test passed."));
    assert!(card.body.get("status").is_none());
    assert!(card.body.get("diffStat").is_none());
    let proof = fixture
        .events("91bc")
        .into_iter()
        .find(|event| event.kind == EventKind::Proof)
        .expect("a proof event");
    let status = proof.body["status"].as_str().expect("git status");
    let diff_stat = proof.body["diffStat"].as_str().expect("git diff stat");
    assert!(
        status.contains("README.md"),
        "git status names the file: {status}"
    );
    assert!(
        diff_stat.contains("README.md"),
        "git diff stat names the file: {diff_stat}"
    );
}

/// A command that exits non-zero adds its argv, exit code, and output tail to
/// `failures`.
#[tokio::test]
async fn a_failing_command_is_on_the_proof_card_with_its_exit_and_tail() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run",
            serde_json::json!({ "argv": ["sh", "-c", "echo oops; exit 3"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("proof-failure", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "Run a failing command.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    let card = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Proof)
        .expect("a proof card");
    assert!(card.body.get("failures").is_none());
    let proof = fixture
        .events("91bc")
        .into_iter()
        .find(|event| event.kind == EventKind::Proof)
        .expect("a proof event");
    let failures = proof.body["failures"].as_array().expect("failures");
    assert_eq!(failures.len(), 1, "one failure: {failures:?}");
    assert_eq!(
        failures[0]["argv"],
        serde_json::json!(["sh", "-c", "echo oops; exit 3"])
    );
    assert_eq!(failures[0]["exit"], serde_json::json!(3));
    let tail = failures[0]["tail"].as_str().expect("the tail");
    assert!(tail.contains("oops"), "the tail has the output: {tail}");
}

/// A command that exits 0 does not appear on the proof card. Its output remains
/// in `events.jsonl`.
#[tokio::test]
async fn a_successful_command_is_absent_from_the_proof_card() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "run",
            serde_json::json!({ "argv": ["sh", "-c", "echo hello"] }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("proof-success", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "Run a successful command.");
    fixture.wait_for_cards("91bc", 2).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    assert!(view.cards.iter().all(|card| card.kind != CardKind::Proof));
    assert!(fixture
        .events("91bc")
        .iter()
        .all(|event| event.kind != EventKind::Proof));
}

#[tokio::test]
async fn a_turn_that_writes_nothing_has_an_empty_wrote_list() {
    let replies = vec![Canned::Json(text_reply("Just reading."))];
    let fixture = Fixture::new("proof-empty", replies);
    fixture.add_session("91bc");

    fixture.ask("91bc", "Say something.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    assert!(view.cards.iter().all(|card| card.kind != CardKind::Proof));
    assert!(fixture
        .events("91bc")
        .iter()
        .all(|event| event.kind != EventKind::Proof));
}

#[tokio::test]
async fn finish_stores_the_result_and_the_proof() {
    let replies = vec![Canned::Json(tool_call_reply(vec![(
        "finish",
        serde_json::json!({
            "text": "The readme names the binary.",
            "proof": "cargo test passed."
        }),
    )]))];
    let fixture = Fixture::new("finish-proof", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Name the binary.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    let result = view
        .cards
        .iter()
        .find(|card| card.kind == CardKind::Result)
        .expect("a result card");
    assert_eq!(
        result.body["text"],
        serde_json::json!("The readme names the binary.")
    );
    assert!(fixture
        .events("91bc")
        .iter()
        .all(|event| event.kind != EventKind::Proof));
    let results = fixture
        .events("91bc")
        .into_iter()
        .filter(|event| event.kind == EventKind::Result)
        .count();
    assert_eq!(results, 1);
}

#[tokio::test]
async fn finish_without_proof_stays_working() {
    let cases = [
        (
            "finish-blank-proof",
            serde_json::json!({
                "text": "The readme names the binary.",
                "proof": "   "
            }),
        ),
        (
            "finish-omitted-proof",
            serde_json::json!({ "text": "The readme names the binary." }),
        ),
    ];
    for (name, args) in cases {
        let replies = vec![Canned::Json(tool_call_reply(vec![("finish", args)]))];
        let fixture = Fixture::new(name, replies);
        fixture.add_session("91bc");
        fixture.ask("91bc", "Name the binary.");
        fixture.wait_for_turn_to_start("91bc").await;
        fixture.wait_for_status("91bc", Status::Idle).await;

        let view = fixture.view("91bc");
        let results: Vec<_> = view
            .cards
            .iter()
            .filter(|card| card.kind == CardKind::Result)
            .collect();
        assert_eq!(results.len(), 1, "{name}");
        assert_eq!(
            results[0].body["text"],
            serde_json::json!("The readme names the binary."),
            "{name}"
        );
        assert!(
            view.cards.iter().all(|card| card.kind != CardKind::Proof),
            "{name}"
        );
        let proofs: Vec<_> = fixture
            .events("91bc")
            .into_iter()
            .filter(|event| event.kind == EventKind::Proof)
            .collect();
        assert!(proofs.is_empty(), "{name}");
    }
}

#[tokio::test]
async fn switching_provider_is_picked_up_on_the_next_turn() {
    let office = FakeServer::start(vec![Canned::Json(text_reply("from office"))]);
    let local = FakeServer::start(vec![Canned::Json(text_reply("from local"))]);
    let root =
        std::env::temp_dir().join(format!("kyotoagent-turn-provider-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let path = root.join("config.toml");
    fs::write(
        &path,
        format!(
            "provider = \"office\"\ntitle_model = \"\"\n\n[providers.office]\nbase_url = \"{office}\"\nmodel = \"office-model\"\n\n[providers.local]\nbase_url = \"{local}\"\nmodel = \"local-model\"\n",
            office = office.base_url(),
            local = local.base_url(),
        ),
    )
    .expect("the file writes");
    let config = Config::load(&path).expect("the file loads");
    let runner = Runner::with_config_file(&config, &path).expect("the runner is built");
    let workspace = root.join("w-91bc");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    let session = Session::at(&root.join("session-91bc"));
    session
        .create(&SessionMeta::new("91bc", &workspace, "office-model", AT))
        .expect("the session is created");
    runner.add_session(&session).expect("the session is added");

    runner.ask("91bc", "First.").expect("the first turn starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = runner.view("91bc").expect("the view reads");
        if view.status == Status::Idle
            && view.cards.iter().any(|card| card.kind == CardKind::Result)
        {
            let result = view
                .cards
                .iter()
                .find(|card| card.kind == CardKind::Result)
                .expect("a result card");
            assert_eq!(result.body["text"], serde_json::Value::from("from office"));
            break;
        }
        assert!(Instant::now() < deadline, "the first turn did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Config::use_provider(&path, "local").expect("local is in the file");
    runner
        .ask("91bc", "Second.")
        .expect("the second turn starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = runner.view("91bc").expect("the view reads");
        let results: Vec<_> = view
            .cards
            .iter()
            .filter(|card| card.kind == CardKind::Result)
            .collect();
        if view.status == Status::Idle && results.len() >= 2 {
            assert_eq!(
                results[1].body["text"],
                serde_json::Value::from("from local")
            );
            break;
        }
        assert!(Instant::now() < deadline, "the second turn did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_grok_turn_without_auth_json_tells_you_to_log_in() {
    let server = FakeServer::start(vec![Canned::Json(text_reply("should not run"))]);
    let root =
        std::env::temp_dir().join(format!("kyotoagent-turn-grok-login-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let path = root.join("config.toml");
    fs::write(
        &path,
        format!(
            "provider = \"grok\"\n\n[providers.grok]\nbase_url = \"{}\"\nmodel = \"grok-4.6\"\n",
            server.base_url()
        ),
    )
    .expect("the file writes");
    let decoy = root.join(".grok");
    fs::create_dir_all(&decoy).expect("the decoy dir exists");
    fs::write(
        decoy.join("auth.json"),
        "{\"access_token\":\"from-grok-home\"}",
    )
    .expect("the decoy writes");
    let config = Config::load(&path).expect("the file loads");
    let runner = Runner::with_config_file(&config, &path).expect("the runner is built");
    let workspace = root.join("w-91bc");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    let session = Session::at(&root.join("session-91bc"));
    session
        .create(&SessionMeta::new("91bc", &workspace, "grok-4.6", AT))
        .expect("the session is created");
    runner.add_session(&session).expect("the session is added");
    runner.ask("91bc", "Hello.").expect("the turn starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = runner.view("91bc").expect("the view reads");
        if view.status == Status::Idle
            && view.cards.iter().any(|card| card.kind == CardKind::Result)
        {
            let result = view
                .cards
                .iter()
                .find(|card| card.kind == CardKind::Result)
                .expect("a result card");
            let text = result.body["text"].as_str().unwrap_or("");
            assert_eq!(
                text,
                "Open Providers in Kyoto Agent to sign in or enter an API key."
            );
            assert!(
                !text.contains("from-grok-home"),
                "the Grok CLI session is unused: {text}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "the turn did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = fs::remove_dir_all(&root);
}

fn plant_hooks(workspace: &std::path::Path, body: serde_json::Value) {
    let dir = workspace.join(".agents");
    fs::create_dir_all(&dir).expect("the hooks directory exists");
    fs::write(dir.join("hooks.json"), body.to_string()).expect("the hooks file writes");
}

fn plant_hook_script(workspace: &std::path::Path, name: &str, body: &str) {
    fs::write(workspace.join(name), body).expect("the hook script writes");
}

fn hook_file(event: &str, matcher: &str, command: &str) -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            event: [{
                "matcher": matcher,
                "hooks": [{ "type": "command", "command": command }]
            }]
        }
    })
}

#[tokio::test]
async fn the_next_turn_loads_agents_md_and_leaves_the_cards_quiet() {
    let replies = vec![
        Canned::Json(text_reply("First.")),
        Canned::Json(text_reply("Second.")),
    ];
    let fixture = Fixture::new("agents-md", replies);
    let workspace = fixture.add_session("91bc");
    git(&workspace, &["init"]);
    fs::write(workspace.join("AGENTS.md"), "pnpm test\n").expect("agents writes");
    plant_skill(
        &workspace,
        "preflight",
        "Ship checks",
        "SKILL BODY STAYS OUT",
    );

    fixture.ask("91bc", "Go.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let first = chat_prompt(&fixture.chat_bodies());
    assert!(
        first.contains("Project instructions (AGENTS.md):"),
        "the section: {first}"
    );
    assert!(first.contains("pnpm test"), "the file: {first}");
    assert!(
        !first.contains("SKILL BODY STAYS OUT"),
        "the skill body stays out: {first}"
    );
    let cards = serde_json::to_string(&fixture.view("91bc").cards).expect("cards");
    assert!(!cards.contains("AGENTS.md"), "quiet cards: {cards}");
    assert!(!cards.contains("pnpm test"), "quiet cards: {cards}");

    fs::write(workspace.join("AGENTS.md"), "cargo test --offline\n").expect("agents rewrites");
    fixture.ask("91bc", "Again.");
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let bodies = fixture.chat_bodies();
    let prompts: Vec<&str> = bodies
        .iter()
        .filter(|body| body.contains("You are Kyoto Agent"))
        .map(String::as_str)
        .collect();
    assert!(prompts.len() >= 2, "a second turn: {prompts:?}");
    let second = prompts.last().expect("the second prompt");
    assert!(
        second.contains("cargo test --offline"),
        "the edit is on the next turn: {second}"
    );
    assert!(
        !second.contains("pnpm test"),
        "the old file is gone: {second}"
    );
}

fn chat_prompt(bodies: &[String]) -> String {
    bodies
        .iter()
        .find(|body| body.contains("You are Kyoto Agent"))
        .expect("the model was called")
        .clone()
}

fn plant_skill(workspace: &std::path::Path, name: &str, description: &str, body: &str) {
    let dir = workspace.join(AGENTS_SKILLS).join(name);
    fs::create_dir_all(&dir).expect("the skill directory exists");
    fs::write(
        dir.join(SKILL_FILE),
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    )
    .expect("the skill is written");
}

#[tokio::test]
async fn submitting_a_slash_skill_loads_the_body_into_the_prompt() {
    let replies = vec![Canned::Json(text_reply("Shipped."))];
    let fixture = Fixture::new("slash-skill", replies);
    let workspace = fixture.add_session("91bc");
    plant_skill(
        &workspace,
        "preflight",
        "Ship checks",
        "PREFLIGHT BODY UNIQUE",
    );

    fixture.ask("91bc", "/preflight ship this");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    assert_eq!(view.cards[0].kind, CardKind::Ask);
    assert_eq!(
        view.cards[0].body["text"],
        serde_json::Value::from("ship this")
    );
    let bodies = fixture.chat_bodies();
    assert!(!bodies.is_empty(), "the model was called");
    let value: serde_json::Value = serde_json::from_str(&bodies[0]).expect("the chat body is json");
    let messages = value["messages"].as_array().expect("messages");
    let system = messages
        .iter()
        .find(|message| message["role"] == "system")
        .and_then(|message| message["content"].as_str())
        .expect("a system message");
    let user = messages
        .iter()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .expect("a user message");
    assert!(
        !system.contains("PREFLIGHT BODY UNIQUE"),
        "the system prompt stays free of the body: {system}"
    );
    assert!(
        system.contains("<skills_instructions>"),
        "the catalog stays on the system prompt: {system}"
    );
    assert!(!system.contains("<skill>\n<name>"), "{system}");
    let query_at = user.find("</user_query>").expect("query close");
    let skill_at = user
        .find("<skill>\n<name>preflight</name>\n")
        .expect("skill");
    assert!(query_at < skill_at, "{user}");
    assert!(user.contains("<path>"), "{user}");
    assert!(user.contains("SKILL.md</path>"), "{user}");
    assert!(user.contains("PREFLIGHT BODY UNIQUE"), "{user}");
    assert!(!user.contains("<skill_information>"), "{user}");
    assert!(!user.contains("name=\""), "{user}");
    assert!(
        user.contains("<user_query>\nship this\n</user_query>"),
        "the user message is the text after the name: {user}"
    );
}

#[tokio::test]
async fn a_second_ask_sends_the_first_ask_and_its_tool_result() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "list_dir",
            serde_json::json!({ "path": "." }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Listed.", "proof": "The directory lists MARKER.txt." }),
        )])),
        Canned::Json(text_reply("Second turn.")),
    ];
    let fixture = Fixture::new("two-asks", replies);
    let workspace = fixture.add_session("91bc");
    fs::write(workspace.join("MARKER.txt"), "x").expect("the marker exists");

    fixture.ask("91bc", "first ask");
    fixture.wait_for_status("91bc", Status::Idle).await;
    fixture.ask("91bc", "second ask");
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let bodies = fixture.chat_bodies();
    let second = bodies
        .iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .rfind(|value| {
            value["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["role"] == "user"
                        && message["content"]
                            .as_str()
                            .is_some_and(|text| text.contains("second ask"))
                })
            })
        })
        .expect("the second ask reached the model");
    let messages = second["messages"].as_array().expect("messages");
    let users: Vec<&str> = messages
        .iter()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .collect();
    assert!(
        users
            .iter()
            .any(|text| text.contains("<user_query>\nfirst ask\n</user_query>")),
        "{users:?}"
    );
    assert!(
        users
            .iter()
            .any(|text| text.contains("<user_query>\nsecond ask\n</user_query>")),
        "{users:?}"
    );
    assert!(
        users[0].starts_with("<user_info>\n"),
        "the first user message carries the prefix: {}",
        users[0]
    );
    // The prompt carries the workspace the session tools resolved, which is the
    // canonical path (macOS resolves the temp dir through `/private`).
    let resolved = fs::canonicalize(&workspace).expect("the workspace resolves");
    assert!(
        users[0].contains(&format!("Workspace Path: {}", resolved.display())),
        "{}",
        users[0]
    );
    let date = users[0]
        .lines()
        .find(|line| line.starts_with("Today's date: "))
        .expect("a date");
    let date = date.trim_start_matches("Today's date: ");
    assert_eq!(date.len(), 10, "{date}");
    assert!(messages.iter().any(|message| {
        message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|text| text.contains("MARKER.txt"))
    }));
}

#[tokio::test]
async fn an_unknown_slash_name_starts_an_ordinary_ask() {
    let replies = vec![Canned::Json(text_reply("Hello."))];
    let fixture = Fixture::new("slash-unknown", replies);
    let workspace = fixture.add_session("91bc");
    plant_skill(
        &workspace,
        "preflight",
        "Ship checks",
        "PREFLIGHT BODY UNIQUE",
    );

    fixture.ask("91bc", "/unknown hi");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    let view = fixture.view("91bc");
    assert_eq!(view.cards[0].kind, CardKind::Ask);
    assert_eq!(
        view.cards[0].body["text"],
        serde_json::Value::from("/unknown hi")
    );
    let bodies = fixture.chat_bodies();
    assert!(!bodies.is_empty(), "the model was called");
    let prompt = &bodies[0];
    assert!(
        !prompt.contains("PREFLIGHT BODY UNIQUE"),
        "no skill loaded: {prompt}"
    );
    assert!(
        prompt.contains("/unknown hi"),
        "the ask is unchanged: {prompt}"
    );
}

fn git(workspace: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} succeeds");
}

#[tokio::test]
async fn a_pre_tool_use_hook_that_exits_two_leaves_the_file_unchanged() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "notes.md", "contents": "hello" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Created notes.md.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("hook-pre-deny", replies);
    let workspace = fixture.add_session("91bc");
    plant_hook_script(
        &workspace,
        "pre.sh",
        "cat > pre-stdin.json\necho hook-denied-write >&2\nexit 2\n",
    );
    plant_hooks(
        &workspace,
        hook_file("PreToolUse", "write_file", "sh ./pre.sh"),
    );

    fixture.ask("91bc", "Create notes.md.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;

    assert!(
        !workspace.join("notes.md").exists(),
        "the denied write leaves the file absent"
    );
    let bodies = fixture.chat_bodies();
    assert!(
        bodies.iter().any(|body| body.contains("hook-denied-write")),
        "the model reads the stderr: {bodies:?}"
    );
    let kinds: Vec<CardKind> = fixture
        .view("91bc")
        .cards
        .iter()
        .map(|card| card.kind)
        .collect();
    assert_eq!(kinds, vec![CardKind::Ask, CardKind::Result]);
    let stdin = fs::read_to_string(workspace.join("pre-stdin.json")).expect("stdin was written");
    assert!(
        stdin.contains("write_file"),
        "the hook saw the tool: {stdin}"
    );
}

#[tokio::test]
async fn a_post_tool_use_hook_that_exits_two_still_wrote_the_file() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "notes.md", "contents": "hello" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Created notes.md.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("hook-post", replies);
    let workspace = fixture.add_session("91bc");
    plant_hook_script(
        &workspace,
        "post.sh",
        "cat > post-stdin.json\necho hook-write-feedback >&2\nexit 2\n",
    );
    plant_hooks(
        &workspace,
        hook_file("PostToolUse", "write_file", "sh ./post.sh"),
    );

    fixture.ask("91bc", "Create notes.md.");
    let cards = fixture.wait_for_cards("91bc", 2).await;
    assert_eq!(cards[1].kind, CardKind::Permission);
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;

    assert_eq!(
        fs::read_to_string(workspace.join("notes.md")).expect("the file reads"),
        "hello"
    );
    let bodies = fixture.chat_bodies();
    assert!(
        bodies
            .iter()
            .any(|body| body.contains("hook-write-feedback")),
        "the model reads the stderr: {bodies:?}"
    );
    let stdin = fs::read_to_string(workspace.join("post-stdin.json")).expect("stdin was written");
    assert!(stdin.contains("tool_result"), "post stdin: {stdin}");
}

#[tokio::test]
async fn a_stop_hook_that_exits_two_keeps_the_session_working() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done.", "proof": "cargo test passed." }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Done after the hook.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("hook-stop", replies);
    let workspace = fixture.add_session("91bc");
    plant_hook_script(
        &workspace,
        "stop.sh",
        "cat > stop-stdin.json\nif [ -f hook-allow ]; then exit 0; fi\necho hook-blocked-finish >&2\nexit 2\n",
    );
    plant_hooks(&workspace, hook_file("Stop", "", "sh ./stop.sh"));

    fixture.ask("91bc", "Finish.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture
        .wait_for_tool_output("91bc", "hook-blocked-finish")
        .await;

    let view = fixture.view("91bc");
    assert_eq!(view.status, Status::Working);
    assert!(
        fixture
            .events("91bc")
            .iter()
            .all(|event| event.kind != EventKind::Proof),
        "there is no proof event yet"
    );
    assert!(
        view.cards.iter().all(|card| card.kind != CardKind::Proof),
        "hooks draw no proof card"
    );

    fs::write(workspace.join("hook-allow"), "").expect("the allow file writes");
    fixture.wait_for_status("91bc", Status::Idle).await;
    let view = fixture.view("91bc");
    assert!(
        view.cards.iter().all(|card| card.kind != CardKind::Proof),
        "a finish without recorded evidence creates no proof card"
    );
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
async fn a_run_matcher_does_not_deny_a_write() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "notes.md", "contents": "hello" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Created notes.md.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("hook-miss", replies);
    let workspace = fixture.add_session("91bc");
    plant_hook_script(&workspace, "pre.sh", "echo hook-run-denied >&2\nexit 2\n");
    plant_hooks(&workspace, hook_file("PreToolUse", "run", "sh ./pre.sh"));

    fixture.ask("91bc", "Create notes.md.");
    let cards = fixture.wait_for_cards("91bc", 2).await;
    assert_eq!(cards[1].kind, CardKind::Permission);
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;

    assert_eq!(
        fs::read_to_string(workspace.join("notes.md")).expect("the file reads"),
        "hello"
    );
    let bodies = fixture.chat_bodies();
    assert!(
        bodies.iter().all(|body| !body.contains("hook-run-denied")),
        "a run matcher misses a write: {bodies:?}"
    );
}

#[tokio::test]
async fn a_workspace_with_no_hook_files_writes_as_it_does_today() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "write_file",
            serde_json::json!({ "path": "notes.md", "contents": "hello" }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Created notes.md.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("hook-none", replies);
    let workspace = fixture.add_session("91bc");
    assert!(!workspace.join(".agents").exists());

    fixture.ask("91bc", "Create notes.md.");
    let cards = fixture.wait_for_cards("91bc", 2).await;
    assert_eq!(cards[1].kind, CardKind::Permission);
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;

    assert_eq!(
        fs::read_to_string(workspace.join("notes.md")).expect("the file reads"),
        "hello"
    );
}

fn pending(id: &str, title: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "title": title, "status": "pending" })
}

fn in_progress(id: &str, title: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "title": title, "status": "in_progress" })
}

#[tokio::test]
async fn a_todo_call_writes_the_list_and_no_card() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![("todo", {
            let mut write = in_progress("write", "Write the tool");
            write["description"] = serde_json::Value::from("Replace content with title.");
            write["files"] = serde_json::json!(["src/events.rs"]);
            let mut link = String::from("https:");
            link.push('/');
            link.push('/');
            link.push_str("docs.rs/serde");
            write["links"] = serde_json::json!([link]);
            serde_json::json!({
                "items": [
                    pending("read", "Read the crate"),
                    pending("draw", "Draw the pane"),
                    write,
                ]
            })
        })])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Listed the work.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("todo-list", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Track the work.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let view = fixture.view("91bc");
    assert_eq!(view.todos.len(), 3);
    assert_eq!(view.todos[2].id, "write");
    assert_eq!(view.todos[2].title, "Write the tool");
    assert_eq!(
        view.todos[2].description.as_deref(),
        Some("Replace content with title.")
    );
    assert_eq!(view.todos[2].files, vec!["src/events.rs"]);
    assert_eq!(view.todos[2].links.len(), 1);
    assert!(fixture
        .events("91bc")
        .iter()
        .any(|event| event.kind == EventKind::Todos));
    let json = serde_json::to_string(&view.cards).expect("cards");
    assert!(!json.contains("\"kind\":\"todo\""), "{json}");
    assert_eq!(
        view.cards.iter().map(|card| card.kind).collect::<Vec<_>>(),
        vec![CardKind::Ask, CardKind::Result]
    );
}

#[tokio::test]
async fn a_second_todo_call_replaces_the_list() {
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "todo",
            serde_json::json!({
                "items": [pending("a", "first"), pending("b", "second")]
            }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "todo",
            serde_json::json!({
                "items": [in_progress("a", "now")]
            }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Updated.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("todo-replace", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Track the work.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let view = fixture.view("91bc");
    assert_eq!(view.todos.len(), 1);
    assert_eq!(view.todos[0].title, "now");
}

#[tokio::test]
async fn a_bad_todo_call_leaves_the_list() {
    let items: Vec<serde_json::Value> =
        (0..21).map(|i| pending(&format!("n{i}"), "step")).collect();
    let replies = vec![
        Canned::Json(tool_call_reply(vec![(
            "todo",
            serde_json::json!({
                "items": [pending("a", "keep"), in_progress("b", "now")]
            }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "todo",
            serde_json::json!({ "items": items }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "todo",
            serde_json::json!({
                "items": [{ "id": "x", "content": "nope", "status": "working" }]
            }),
        )])),
        Canned::Json(tool_call_reply(vec![(
            "todo",
            serde_json::json!({
                "items": [
                    in_progress("a", "one"),
                    in_progress("b", "two"),
                ]
            }),
        )])),
        Canned::Json(tool_call_reply(vec![("todo", {
            let mut item = pending("a", "step");
            item["files"] = serde_json::json!(["src/../secret"]);
            serde_json::json!({ "items": [item] })
        })])),
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Kept.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("todo-bad", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Track the work.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let view = fixture.view("91bc");
    assert_eq!(view.todos.len(), 2);
    assert_eq!(view.todos[0].id, "a");
    let errors = fixture
        .events("91bc")
        .into_iter()
        .filter(|event| event.kind == EventKind::ToolResult)
        .filter(|event| {
            event.body["output"].as_str().is_some_and(|output| {
                output.contains("20")
                    || output.contains("status")
                    || output.contains("in_progress")
                    || output.contains("path")
                    || output.contains("title")
            })
        })
        .count();
    assert_eq!(errors, 4);
}

#[tokio::test]
async fn a_streamed_thought_shows_on_the_view_then_the_prose_is_the_result() {
    let gate = HoldGate::new();
    let replies = vec![Canned::Hold {
        head: reasoning_event("ponder the answer"),
        tail: content_event("The readme names the binary."),
        gate: Arc::clone(&gate),
    }];
    let fixture = Fixture::new("stream-thought", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Name the binary.");
    let view = fixture.wait_for_thinking("91bc", "ponder the answer").await;
    assert_eq!(view.status, Status::Working);
    let during = serde_json::to_string(&fixture.events("91bc")).expect("the log encodes");
    assert!(!during.contains("ponder the answer"), "{during}");
    gate.release();
    fixture.wait_for_status("91bc", Status::Idle).await;
    let view = fixture.view("91bc");
    assert!(view.phase.is_none());
    assert!(view.thinking.is_none());
    let events = fixture.events("91bc");
    let result = events
        .iter()
        .find(|event| event.kind == EventKind::Result)
        .expect("a result");
    assert_eq!(result.body["text"], "The readme names the binary.");
    let message = events
        .iter()
        .find(|event| event.kind == EventKind::ModelMessage)
        .expect("a model message");
    assert_eq!(message.body["text"], "The readme names the binary.");
    let log = serde_json::to_string(&events).expect("the log encodes");
    assert!(!log.contains("ponder the answer"), "{log}");
}

#[tokio::test]
async fn ctrl_x_during_the_stream_stops_without_waiting_for_done() {
    let gate = HoldGate::new();
    let replies = vec![Canned::Hold {
        head: reasoning_event("ponder the answer"),
        tail: content_event("The readme names the binary."),
        gate: Arc::clone(&gate),
    }];
    let fixture = Fixture::new("cancel-stream", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Name the binary.");
    let view = fixture.wait_for_thinking("91bc", "ponder the answer").await;
    assert_eq!(view.status, Status::Working);
    let started = Instant::now();
    fixture.cancel("91bc");
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(started.elapsed() < Duration::from_secs(3));
    let events = fixture.events("91bc");
    let result = events
        .iter()
        .find(|event| event.kind == EventKind::Result)
        .expect("a result");
    assert_eq!(result.body["text"], "Stopped.");
    let log = serde_json::to_string(&events).expect("the log encodes");
    assert!(!log.contains("ponder the answer"), "{log}");
}

#[tokio::test]
async fn tools_after_a_thought_leave_the_phase_on_tool() {
    let gate = HoldGate::new();
    let replies = vec![
        Canned::Hold {
            head: reasoning_event("ponder the answer"),
            tail: kyotoagent::chat::completion_as_sse(&tool_call_reply(vec![(
                "write_file",
                serde_json::json!({ "path": "notes.md", "contents": "hello" }),
            )])),
            gate: Arc::clone(&gate),
        },
        Canned::Json(tool_call_reply(vec![(
            "finish",
            serde_json::json!({ "text": "Created notes.md.", "proof": "cargo test passed." }),
        )])),
    ];
    let fixture = Fixture::new("thought-then-tool", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Create notes.md.");
    fixture.wait_for_thinking("91bc", "ponder the answer").await;
    gate.release();
    fixture.wait_for_waiting_permission("91bc").await;
    let view = fixture.view("91bc");
    assert_eq!(view.phase, Some(Phase::Tool));
    assert_eq!(view.action.as_deref(), Some("Editing"));
    assert!(view.thinking.is_none());
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;
    let result = fixture
        .events("91bc")
        .into_iter()
        .find(|event| event.kind == EventKind::Result)
        .expect("a result");
    assert_eq!(result.body["text"], "Created notes.md.");
}

fn ask_texts(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter(|event| event.kind == EventKind::UserAsk)
        .filter_map(|event| event.body["text"].as_str().map(str::to_string))
        .collect()
}

fn result_texts(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter(|event| event.kind == EventKind::Result)
        .filter_map(|event| event.body["text"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn an_ask_during_a_turn_is_queued_and_a_ninth_is_refused() {
    let gate = HoldGate::new();
    let replies = vec![Canned::Hold {
        head: reasoning_event("ponder the answer"),
        tail: content_event("done"),
        gate: Arc::clone(&gate),
    }];
    let fixture = Fixture::new("queue-cap", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "live");
    fixture.wait_for_thinking("91bc", "ponder the answer").await;

    assert_eq!(
        fixture.runner.ask("91bc", "   ").expect("empty is ignored"),
        AskOutcome::Ignored
    );
    assert!(fixture.view("91bc").queue.is_empty());

    for index in 1..=8 {
        assert!(matches!(
            fixture
                .runner
                .ask("91bc", &format!("q{index}"))
                .expect("queued"),
            AskOutcome::Queued(_)
        ));
    }
    let queued: Vec<String> = (1..=8).map(|index| format!("q{index}")).collect();
    assert_eq!(fixture.view("91bc").queue, queued);
    let error = fixture
        .runner
        .ask("91bc", "q9")
        .expect_err("a ninth ask is refused");
    assert!(
        error.to_string().contains("queue is full"),
        "the error names the queue: {error}"
    );
    assert_eq!(fixture.view("91bc").queue, queued);
    assert_eq!(fixture.view("91bc").status, Status::Working);
    gate.release();
}

#[tokio::test]
async fn cancel_starts_the_first_queued_ask_and_leaves_the_second() {
    let first = HoldGate::new();
    let second = HoldGate::new();
    let replies = vec![
        Canned::Hold {
            head: reasoning_event("ponder the answer"),
            tail: content_event("should not land"),
            gate: Arc::clone(&first),
        },
        Canned::Hold {
            head: reasoning_event("next thought"),
            tail: content_event("first queued done"),
            gate: Arc::clone(&second),
        },
        Canned::Json(text_reply("second queued done")),
    ];
    let fixture = Fixture::new("queue-cancel", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "live");
    fixture.wait_for_thinking("91bc", "ponder the answer").await;
    assert!(matches!(
        fixture.runner.ask("91bc", "one").expect("queued"),
        AskOutcome::Queued(_)
    ));
    assert!(matches!(
        fixture.runner.ask("91bc", "two").expect("queued"),
        AskOutcome::Queued(_)
    ));
    assert_eq!(
        fixture.view("91bc").queue,
        vec!["one".to_string(), "two".to_string()]
    );

    fixture.cancel("91bc");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = fixture.view("91bc");
        let asks = ask_texts(&fixture.events("91bc"));
        if view.queue == ["two"] && asks == ["live", "one"] && view.status == Status::Working {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the first queued ask did not start: queue {:?} asks {asks:?} status {:?}",
            view.queue,
            view.status
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        result_texts(&fixture.events("91bc")),
        vec!["Stopped.".to_string()]
    );

    second.release();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = fixture.view("91bc");
        let events = fixture.events("91bc");
        if view.status == Status::Idle
            && view.queue.is_empty()
            && ask_texts(&events) == ["live", "one", "two"]
            && result_texts(&events) == ["Stopped.", "first queued done", "second queued done"]
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the second queued ask did not finish: {:?}",
            result_texts(&events)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_finished_turn_starts_the_queued_ask() {
    let gate = HoldGate::new();
    let replies = vec![
        Canned::Hold {
            head: reasoning_event("ponder the answer"),
            tail: content_event("first done"),
            gate: Arc::clone(&gate),
        },
        Canned::Json(text_reply("second done")),
    ];
    let fixture = Fixture::new("queue-finish", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "live");
    fixture.wait_for_thinking("91bc", "ponder the answer").await;
    assert!(matches!(
        fixture.runner.ask("91bc", "then this").expect("queued"),
        AskOutcome::Queued(_)
    ));
    assert_eq!(fixture.view("91bc").queue, vec!["then this".to_string()]);
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = fixture.view("91bc");
        let events = fixture.events("91bc");
        if view.status == Status::Idle
            && view.queue.is_empty()
            && ask_texts(&events) == ["live", "then this"]
            && result_texts(&events) == ["first done", "second done"]
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the queued ask did not finish: {:?}",
            result_texts(&events)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn chat_posts(bodies: &[String]) -> Vec<&String> {
    bodies
        .iter()
        .filter(|body| body.contains("You are Kyoto Agent"))
        .collect()
}

#[tokio::test]
async fn an_empty_reply_with_no_tool_call_is_the_result() {
    let replies = vec![Canned::Json(
        serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": null }
            }]
        })
        .to_string(),
    )];
    let fixture = Fixture::new("empty-reply", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Say nothing.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let events = fixture.events("91bc");
    assert!(events.iter().all(|event| event.kind != EventKind::ToolCall));
    let result = events
        .iter()
        .find(|event| event.kind == EventKind::Result)
        .expect("a result");
    assert_eq!(result.body["text"], "");
}

#[tokio::test]
async fn truncated_tool_arguments_are_a_tool_error_the_model_retries() {
    let raw = "{\"cmd\":\"echo hello";
    let gate = HoldGate::new();
    let replies = vec![
        Canned::Json(tool_call_raw("run", raw)),
        Canned::Hold {
            head: reasoning_event("retry the call"),
            tail: kyotoagent::chat::completion_as_sse(&tool_call_reply(vec![(
                "run",
                serde_json::json!({ "argv": ["echo", "hello"] }),
            )])),
            gate: Arc::clone(&gate),
        },
        Canned::Json(text_reply("echoed")),
    ];
    let fixture = Fixture::new("bad-args", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Run echo.");
    let view = fixture.wait_for_thinking("91bc", "retry the call").await;
    assert_eq!(view.status, Status::Working);
    let events = fixture.events("91bc");
    let kinds: Vec<EventKind> = events.iter().map(|event| event.kind).collect();
    let message = kinds
        .iter()
        .position(|kind| *kind == EventKind::ModelMessage)
        .expect("a model message");
    let call = kinds
        .iter()
        .position(|kind| *kind == EventKind::ToolCall)
        .expect("a tool call");
    let result = kinds
        .iter()
        .position(|kind| *kind == EventKind::ToolResult)
        .expect("a tool result");
    assert!(message < call && call < result, "{kinds:?}");
    assert_eq!(events[message].body["text"], "");
    assert_eq!(events[call].body["tool"], "run");
    assert_eq!(events[call].body["args"], serde_json::json!({}));
    let output = events[result].body["output"].as_str().expect("output");
    assert!(output.contains(raw), "{output}");
    assert!(
        output.contains("Please fix the syntax and retry."),
        "{output}"
    );
    assert!(
        output.starts_with("the tool arguments were not JSON:"),
        "{output}"
    );
    let bodies = fixture.chat_bodies();
    let posts = chat_posts(&bodies);
    assert!(posts.len() >= 2, "the model was asked again: {posts:?}");
    let retry: serde_json::Value = serde_json::from_str(posts[1]).expect("the retry is json");
    let messages = retry["messages"].as_array().expect("messages");
    assert!(messages
        .iter()
        .any(|message| { message["role"] == "assistant" && message.get("tool_calls").is_some() }));
    assert!(messages.iter().any(|message| {
        message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains(raw))
    }));
    gate.release();
    fixture.wait_for_waiting_permission("91bc").await;
    assert_ne!(fixture.view("91bc").status, Status::Idle);
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;
    let events = fixture.events("91bc");
    let ran = events.iter().any(|event| {
        event.kind == EventKind::ToolResult
            && event.body["output"]
                .as_str()
                .is_some_and(|output| output.contains("hello") && output.contains("exited 0"))
    });
    assert!(ran, "the retried run finished");
    let result = events
        .iter()
        .find(|event| event.kind == EventKind::Result)
        .expect("a result");
    assert_eq!(result.body["text"], "echoed");
}

#[tokio::test]
async fn empty_tool_arguments_omit_the_original_block_and_the_turn_continues() {
    let replies = vec![
        Canned::Json(tool_call_raw("run", "")),
        Canned::Json(text_reply("recovered")),
    ];
    let fixture = Fixture::new("empty-args", replies);
    fixture.add_session("91bc");
    fixture.ask("91bc", "Run something.");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let events = fixture.events("91bc");
    let output = events
        .iter()
        .find(|event| event.kind == EventKind::ToolResult)
        .expect("a tool result")
        .body["output"]
        .as_str()
        .expect("output");
    assert!(
        output.starts_with("the tool arguments were not JSON:"),
        "{output}"
    );
    assert!(!output.contains("Your original arguments"), "{output}");
    let bodies = fixture.chat_bodies();
    let posts = chat_posts(&bodies);
    assert!(posts.len() >= 2, "the model was asked again");
    let retry: serde_json::Value = serde_json::from_str(posts[1]).expect("the retry is json");
    let messages = retry["messages"].as_array().expect("messages");
    assert!(messages.iter().any(|message| message["role"] == "tool"));
    let result = events
        .iter()
        .find(|event| event.kind == EventKind::Result)
        .expect("a result");
    assert_eq!(result.body["text"], "recovered");
}

#[tokio::test]
async fn reload_during_an_enhance_rewrite_goes_idle_without_a_stopped_result() {
    let root = std::env::temp_dir().join(format!(
        "kyotoagent-turn-enhance-reload-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let workspace = root.join("w-91bc");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    let session = Session::at(&root.join("session-91bc"));
    session
        .create(&SessionMeta::new("91bc", &workspace, "test/model", AT))
        .expect("the session is created");
    let request = Event::new("e1", AT, "t1", EventKind::EnhanceRequest)
        .with_body(&kyotoagent::events::EnhanceRequestBody {
            text: "ship it".into(),
            model: "test/model".into(),
        })
        .expect("the request body");
    session.append(&request).expect("the request is appended");
    let mut meta = session.meta().expect("meta reads");
    meta.status = Status::Working;
    session.write_meta(&meta).expect("the meta is written");
    let server = FakeServer::start(Vec::new());
    let config = Config::from_toml(&format!(
        "base_url = \"{}\"\ntitle_model = \"\"\nmodel = \"test/model\"\n",
        server.base_url()
    ))
    .expect("the config parses");
    let runner = Runner::new(&config).expect("the runner is built");
    runner
        .reload(&[root.join("session-91bc")])
        .expect("the session reloads");
    let view = runner.view("91bc").expect("the view reads");
    assert_eq!(view.status, Status::Idle);
    let events = session.events().expect("the log reads");
    assert!(events.iter().all(|event| event.kind != EventKind::Result));
    assert!(events.iter().all(|event| event.kind != EventKind::UserAsk));
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_goal_requires_an_independent_successful_command() {
    let fixture = Fixture::new(
        "goal-verified",
        vec![
            Canned::Json(text_reply("The file is ready.")),
            Canned::Json(tool_call_reply(vec![(
                "run",
                serde_json::json!({ "argv": ["sh", "-c", "test \"$(cat result.txt)\" = expected && printf verified"] }),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "verified": true, "text": "Read and checked the file." }),
            )])),
            Canned::Json(text_reply("Follow-up.")),
        ],
    );
    let workspace = fixture.add_session("91bc");
    fs::write(workspace.join("result.txt"), "expected").expect("fixture writes");
    fixture.ask("91bc", "/goal Ensure result.txt contains expected");
    fixture.wait_for_status("91bc", Status::Waiting).await;
    assert_eq!(
        Session::at(&fixture.root.join("session-91bc"))
            .meta()
            .expect("meta")
            .goal
            .expect("goal")
            .status,
        kyotoagent::goal::GoalStatus::Active
    );
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;
    let goal = Session::at(&fixture.root.join("session-91bc"))
        .meta()
        .expect("meta")
        .goal
        .expect("goal");
    assert_eq!(goal.status, kyotoagent::goal::GoalStatus::Complete);
    assert_eq!(goal.evidence.len(), 1);
    assert!(goal.evidence[0].output.contains("verified"));
    assert_eq!(goal.rounds, 1);
    assert!(goal.tokens_used > 0);
    let bodies = fixture.chat_bodies();
    let verifier: serde_json::Value = serde_json::from_str(
        bodies
            .iter()
            .find(|body| body.contains("Independently verify"))
            .expect("verifier"),
    )
    .expect("request");
    assert_eq!(verifier["messages"].as_array().expect("messages").len(), 2);
    fixture.ask("91bc", "Another question.");
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(fixture
        .view("91bc")
        .cards
        .iter()
        .any(|card| card.body["text"] == "Follow-up."));
}

#[tokio::test]
async fn a_goal_rejects_a_verdict_without_executed_evidence_and_continues() {
    let fixture = Fixture::new(
        "goal-no-evidence",
        vec![
            Canned::Json(text_reply("Done.")),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "verified": true, "text": "Looks fine." }),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "ask",
                serde_json::json!({ "text": "Need a check?" }),
            )])),
            Canned::Json(text_reply("Ready.")),
            Canned::Json(tool_call_reply(vec![(
                "run",
                serde_json::json!({ "argv": ["printf", "evidence"] }),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "verified": true, "text": "Reproduced." }),
            )])),
        ],
    );
    fixture.add_session("91bc");
    fixture.ask("91bc", "/goal Print evidence");
    fixture.wait_for_status("91bc", Status::Waiting).await;
    let goal = Session::at(&fixture.root.join("session-91bc"))
        .meta()
        .expect("meta")
        .goal
        .expect("goal");
    assert_eq!(goal.status, kyotoagent::goal::GoalStatus::Active);
    assert!(goal
        .verification
        .contains("No successful executable verification"));
    fixture.answer_question("91bc", "yes");
    fixture.wait_for_cards("91bc", 4).await;
    fixture.wait_for_status("91bc", Status::Waiting).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;
    let goal = Session::at(&fixture.root.join("session-91bc"))
        .meta()
        .expect("meta")
        .goal
        .expect("goal");
    assert_eq!(goal.status, kyotoagent::goal::GoalStatus::Complete);
    assert_eq!(goal.rounds, 2);
}

#[tokio::test]
async fn a_goal_budget_stops_before_executing_more_tools() {
    let fixture = Fixture::new("goal-budget", vec![Canned::Json(text_reply("Done."))]);
    fixture.add_session("91bc");
    fixture.ask("91bc", "/goal Complete a task --budget 1");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let goal = Session::at(&fixture.root.join("session-91bc"))
        .meta()
        .expect("meta")
        .goal
        .expect("goal");
    assert_eq!(goal.status, kyotoagent::goal::GoalStatus::BudgetExhausted);
    assert_eq!(goal.rounds, 0);
    assert_eq!(
        fixture
            .chat_bodies()
            .iter()
            .filter(|body| body.contains("messages"))
            .count(),
        1
    );
}

#[tokio::test]
async fn a_goal_can_pause_clear_and_report_status_while_waiting() {
    let fixture = Fixture::new(
        "goal-controls",
        vec![Canned::Json(tool_call_reply(vec![(
            "ask",
            serde_json::json!({ "text": "Continue?" }),
        )]))],
    );
    fixture.add_session("91bc");
    fixture.ask("91bc", "/goal Do something");
    fixture.wait_for_status("91bc", Status::Waiting).await;
    assert_eq!(
        fixture.runner.ask("91bc", "/goal pause").expect("pause"),
        AskOutcome::Ignored
    );
    fixture.wait_for_status("91bc", Status::Idle).await;
    let session = Session::at(&fixture.root.join("session-91bc"));
    assert_eq!(
        session.meta().expect("meta").goal.expect("goal").status,
        kyotoagent::goal::GoalStatus::Paused
    );
    assert!(fixture.view("91bc").goal.is_some());
    assert_eq!(
        fixture.runner.ask("91bc", "/goal status").expect("status"),
        AskOutcome::Ignored
    );
    assert_eq!(
        fixture.runner.ask("91bc", "/goal clear").expect("clear"),
        AskOutcome::Ignored
    );
    assert!(session.meta().expect("meta").goal.is_none());
}

#[tokio::test]
async fn a_goal_counts_model_reported_input_and_output_tokens() {
    let mut reply: serde_json::Value = serde_json::from_str(&text_reply("Done.")).expect("reply");
    reply["usage"] = serde_json::json!({ "prompt_tokens": 7, "completion_tokens": 5 });
    let fixture = Fixture::new("goal-tokens", vec![Canned::Json(reply.to_string())]);
    fixture.add_session("91bc");
    fixture.ask("91bc", "/goal Count tokens --budget 12");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let goal = Session::at(&fixture.root.join("session-91bc"))
        .meta()
        .expect("meta")
        .goal
        .expect("goal");
    assert_eq!(goal.tokens_used, 12);
    assert_eq!(goal.status, kyotoagent::goal::GoalStatus::BudgetExhausted);
}

#[tokio::test]
async fn a_goal_resumes_from_persisted_state_after_reloading_the_session() {
    let fixture = Fixture::new(
        "goal-resume",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "ask",
                serde_json::json!({ "text": "Continue?" }),
            )])),
            Canned::Json(text_reply("Ready.")),
            Canned::Json(tool_call_reply(vec![(
                "run",
                serde_json::json!({ "argv": ["printf", "done"] }),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "finish",
                serde_json::json!({ "verified": true, "text": "Checked output." }),
            )])),
        ],
    );
    fixture.add_session("91bc");
    fixture.ask("91bc", "/goal Print done");
    fixture.wait_for_status("91bc", Status::Waiting).await;
    fixture.runner.ask("91bc", "/goal pause").expect("pause");
    fixture.wait_for_status("91bc", Status::Idle).await;
    let session = Session::at(&fixture.root.join("session-91bc"));
    fixture
        .runner
        .reload(&[session.dir().to_path_buf()])
        .expect("reload");
    assert!(matches!(
        fixture.runner.ask("91bc", "/goal resume").expect("resume"),
        AskOutcome::Started(_)
    ));
    fixture.wait_for_cards("91bc", 5).await;
    fixture.wait_for_status("91bc", Status::Waiting).await;
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(
        session.meta().expect("meta").goal.expect("goal").status,
        kyotoagent::goal::GoalStatus::Complete
    );
}

#[tokio::test]
async fn a_goal_pauses_when_the_independent_verifier_cannot_run() {
    let fixture = Fixture::new(
        "goal-verifier-error",
        vec![
            Canned::Json(text_reply("Ready.")),
            Canned::Status(400, "verifier unavailable".to_string()),
        ],
    );
    fixture.add_session("91bc");
    fixture.ask("91bc", "/goal Check the result");
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let goal = Session::at(&fixture.root.join("session-91bc"))
        .meta()
        .expect("meta")
        .goal
        .expect("goal");
    assert_eq!(goal.status, kyotoagent::goal::GoalStatus::Paused);
    assert!(goal.verification.contains("verifier unavailable"));
    assert!(goal.verification.contains("/goal resume"));
    assert!(goal.evidence.is_empty());
}

#[tokio::test]
async fn a_goal_is_preserved_and_paused_after_a_server_restart() {
    let fixture = Fixture::new("goal-restart", vec![Canned::Json(text_reply("unused"))]);
    fixture.add_session("91bc");
    let session = Session::at(&fixture.root.join("session-91bc"));
    session
        .update(|meta| {
            meta.goal = Some(kyotoagent::goal::Goal::new("Finish the task", Some(100)));
            true
        })
        .expect("persist goal");
    fixture
        .runner
        .reload(&[session.dir().to_path_buf()])
        .expect("reload");
    let goal = session.meta().expect("meta").goal.expect("goal");
    assert_eq!(goal.objective, "Finish the task");
    assert_eq!(goal.status, kyotoagent::goal::GoalStatus::Paused);
    assert_eq!(goal.token_budget, Some(100));
    assert!(goal.verification.contains("/goal resume"));
}

#[tokio::test]
async fn a_goal_keeps_attached_images_in_its_initial_model_request() {
    let fixture = Fixture::new("goal-image", vec![Canned::Json(text_reply("Done."))]);
    fixture.add_session("91bc");
    let image =
        kyotoagent::attachment::ImageAttachment::from_bytes("dog.png", kyotoagent::splash::PNG)
            .unwrap();
    fixture
        .runner
        .ask_with_images(
            "91bc",
            "/goal Identify the picture --budget 1",
            None,
            vec![image.clone()],
        )
        .unwrap();
    fixture.wait_for_turn_to_start("91bc").await;
    fixture.wait_for_status("91bc", Status::Idle).await;
    let bodies = fixture.chat_bodies();
    let posts = chat_posts(&bodies);
    let request: serde_json::Value = serde_json::from_str(posts[0]).unwrap();
    let messages = request["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message| message["role"] == "user"
        && message["content"][1]["image_url"]["url"] == image.data_url()));
    assert_eq!(
        fixture.view("91bc").cards[0].body["images"][0]["data"],
        image.data
    );
}

#[tokio::test]
async fn agents_md_hot_reloads_during_a_turn() {
    let question = tool_call_reply(vec![("ask", serde_json::json!({ "text": "Continue?" }))]);
    let fixture = Fixture::new(
        "agents-hot-reload",
        vec![
            Canned::Json(question.clone()),
            Canned::Json(question.clone()),
            Canned::Json(question),
            Canned::Json(text_reply("Done.")),
        ],
    );
    let workspace = fixture.add_session("91bc");
    fixture.ask("91bc", "Go.");
    for (index, rule) in ["FIRST_LIVE_RULE", "SECOND_LIVE_RULE", ""]
        .into_iter()
        .enumerate()
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let questions = fixture
                .events("91bc")
                .iter()
                .filter(|event| event.kind == EventKind::Question)
                .count();
            if questions == index + 1 && fixture.view("91bc").status == Status::Waiting {
                break;
            }
            assert!(Instant::now() < deadline, "the next question did not open");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if rule.is_empty() {
            fs::remove_file(workspace.join("AGENTS.md")).expect("agents removes");
        } else {
            fs::write(workspace.join("AGENTS.md"), rule).expect("agents writes");
        }
        fixture.answer_question("91bc", "yes");
    }
    fixture.wait_for_status("91bc", Status::Idle).await;
    let prompts: Vec<String> = fixture
        .chat_bodies()
        .iter()
        .filter_map(|body| {
            let request: serde_json::Value = serde_json::from_str(body).ok()?;
            request["messages"][0]["content"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(prompts.len(), 4);
    assert!(!prompts[0].contains("FIRST_LIVE_RULE"));
    assert!(prompts[1].contains("FIRST_LIVE_RULE"));
    assert!(prompts[2].contains("SECOND_LIVE_RULE"));
    assert!(!prompts[2].contains("FIRST_LIVE_RULE"));
    assert!(!prompts[3].contains("SECOND_LIVE_RULE"));
}

#[tokio::test]
async fn pre_tool_hooks_hot_reload_during_a_turn() {
    let fixture = Fixture::new(
        "pre-hot-reload",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "ask",
                serde_json::json!({ "text": "Continue?" }),
            )])),
            Canned::Json(tool_call_reply(vec![(
                "write_file",
                serde_json::json!({ "path": "denied.txt", "contents": "no" }),
            )])),
            Canned::Json(text_reply("Done.")),
        ],
    );
    let workspace = fixture.add_session("91bc");
    plant_hooks(&workspace, hook_file("PreToolUse", "write_file", "true"));
    fixture.ask("91bc", "Go.");
    fixture.wait_for_status("91bc", Status::Waiting).await;
    plant_hooks(
        &workspace,
        hook_file(
            "PreToolUse",
            "write_file",
            "echo LIVE_PRE_DENIAL >&2; exit 2",
        ),
    );
    fixture.answer_question("91bc", "yes");
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(!workspace.join("denied.txt").exists());
    assert!(fixture
        .chat_bodies()
        .iter()
        .any(|body| body.contains("LIVE_PRE_DENIAL")));
}

#[tokio::test]
async fn post_tool_hooks_hot_reload_while_permission_is_pending() {
    let fixture = Fixture::new(
        "post-hot-reload",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "write_file",
                serde_json::json!({ "path": "allowed.txt", "contents": "yes" }),
            )])),
            Canned::Json(text_reply("Done.")),
        ],
    );
    let workspace = fixture.add_session("91bc");
    fixture.ask("91bc", "Go.");
    fixture.wait_for_status("91bc", Status::Waiting).await;
    plant_hooks(
        &workspace,
        hook_file(
            "PostToolUse",
            "write_file",
            "echo LIVE_POST_FEEDBACK >&2; exit 2",
        ),
    );
    fixture.answer("91bc", Answer::allow_once());
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert_eq!(
        fs::read_to_string(workspace.join("allowed.txt")).expect("written file"),
        "yes"
    );
    assert!(fixture
        .chat_bodies()
        .iter()
        .any(|body| body.contains("LIVE_POST_FEEDBACK")));
}

#[tokio::test]
async fn stop_hooks_hot_reload_during_a_turn() {
    let finish = tool_call_reply(vec![("finish", serde_json::json!({ "text": "Done." }))]);
    let fixture = Fixture::new(
        "stop-hot-reload",
        vec![
            Canned::Json(tool_call_reply(vec![(
                "ask",
                serde_json::json!({ "text": "Continue?" }),
            )])),
            Canned::Json(finish.clone()),
            Canned::Json(finish),
        ],
    );
    let workspace = fixture.add_session("91bc");
    fixture.ask("91bc", "Go.");
    fixture.wait_for_status("91bc", Status::Waiting).await;
    plant_hooks(&workspace, hook_file("Stop", "", "if [ -f stop-seen ]; then exit 0; fi; touch stop-seen; echo LIVE_STOP_DENIAL >&2; exit 2"));
    fixture.answer_question("91bc", "yes");
    fixture.wait_for_status("91bc", Status::Idle).await;
    assert!(workspace.join("stop-seen").exists());
    assert!(fixture
        .chat_bodies()
        .iter()
        .any(|body| body.contains("LIVE_STOP_DENIAL")));
}

#[tokio::test]
async fn removing_a_dequeued_id_does_not_cancel_the_started_turn() {
    let live = HoldGate::new();
    let next = HoldGate::new();
    let fixture = Fixture::new(
        "remove-after-dequeue",
        vec![
            Canned::Hold {
                head: reasoning_event("live thought"),
                tail: content_event("live done"),
                gate: Arc::clone(&live),
            },
            Canned::Hold {
                head: reasoning_event("queued thought"),
                tail: content_event("queued done"),
                gate: Arc::clone(&next),
            },
            Canned::Json(text_reply("last done")),
        ],
    );
    fixture.add_session("s");
    fixture.ask("s", "live");
    fixture.wait_for_thinking("s", "live thought").await;
    let AskOutcome::Queued(id) = fixture.runner.ask("s", "same text").unwrap() else {
        panic!("expected queued ask")
    };
    fixture.runner.ask("s", "same text").unwrap();
    let view = fixture.view("s");
    assert_eq!(view.queue_items[0].id, id);
    assert_ne!(view.queue_items[1].id, id);
    live.release();
    fixture.wait_for_thinking("s", "queued thought").await;
    assert!(!fixture.runner.remove_queued("s", &id).unwrap());
    assert_eq!(fixture.view("s").status, Status::Working);
    assert_eq!(fixture.view("s").queue, ["same text"]);
    assert_eq!(ask_texts(&fixture.events("s")), ["live", "same text"]);
    next.release();
    fixture.wait_for_cards("s", 6).await;
    fixture.wait_for_status("s", Status::Idle).await;
    assert_eq!(
        ask_texts(&fixture.events("s")),
        ["live", "same text", "same text"]
    );
    assert_eq!(
        result_texts(&fixture.events("s")),
        ["live done", "queued done", "last done"]
    );
}

fn assert_tool_exchange(messages: &[serde_json::Value], expected: &[(&str, &str)]) {
    assert_eq!(messages.len(), expected.len() + 1);
    assert_eq!(messages[0]["role"], "assistant");
    let calls = messages[0]["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), expected.len());
    let mut ids = std::collections::HashSet::new();
    for (call, (name, output)) in calls.iter().zip(expected) {
        let id = call["id"].as_str().unwrap();
        assert!(!id.is_empty() && ids.insert(id));
        assert_eq!(call["function"]["name"], *name);
        let results: Vec<_> = messages[1..]
            .iter()
            .filter(|message| message["tool_call_id"] == id)
            .collect();
        assert_eq!(results.len(), 1, "{messages:?}");
        assert_eq!(results[0]["role"], "tool");
        assert!(
            results[0]["content"].as_str().unwrap().contains(output),
            "{results:?} does not contain {output}"
        );
    }
}

#[tokio::test]
async fn tool_batches_prepare_permissions_then_run_commands_concurrently() {
    let first = "touch first.started; for i in $(seq 1 100); do if [ -f second.started ]; then printf first-output; exit 0; fi; sleep 0.02; done; exit 1";
    let second = "touch second.started; for i in $(seq 1 100); do if [ -f first.started ]; then printf second-output; exit 0; fi; sleep 0.02; done; exit 1";
    let batch = tool_call_reply(vec![
        ("run", serde_json::json!({"argv": ["sh", "-c", first]})),
        ("run", serde_json::json!({"argv": ["sh", "-c", second]})),
    ]);
    let fixture = Fixture::new(
        "parallel-commands",
        vec![
            Canned::Json(batch.clone()),
            Canned::Json(text_reply("Done.")),
        ],
    );
    let workspace = fixture.add_session("s");
    fixture.ask("s", "Run both commands.");
    fixture.wait_for_permission_count("s", 1).await;
    fixture.answer("s", Answer::allow_once());
    fixture.wait_for_permission_count("s", 2).await;
    assert!(!workspace.join("first.started").exists());
    assert!(!workspace.join("second.started").exists());
    assert_eq!(fixture.chat_requests().len(), 1);
    fixture.answer("s", Answer::allow_once());
    fixture.wait_for_status("s", Status::Idle).await;

    let requests = fixture.chat_requests();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_tool_exchange(
        &messages[2..],
        &[("run", "first-output"), ("run", "second-output")],
    );
    for message in &messages[3..] {
        assert!(message["content"].as_str().unwrap().contains("exited 0"));
    }
    let reply: serde_json::Value = serde_json::from_str(&batch).unwrap();
    assert_eq!(
        messages[2]["tool_calls"],
        reply["choices"][0]["message"]["tool_calls"]
    );
    assert_eq!(result_texts(&fixture.events("s")), ["Done."]);
}

#[tokio::test]
async fn tool_batches_user_rejection_runs_prior_approvals_and_skips_the_rest() {
    let fixture = Fixture::new(
        "batch-rejection",
        vec![
            Canned::Json(tool_call_reply(vec![
                (
                    "run",
                    serde_json::json!({"argv": ["sh", "-c", "printf approved > approved.txt"]}),
                ),
                (
                    "write_file",
                    serde_json::json!({"path": "denied.txt", "contents": "denied"}),
                ),
                (
                    "write_file",
                    serde_json::json!({"path": "skipped.txt", "contents": "skipped"}),
                ),
            ])),
            Canned::Json(text_reply("Next turn.")),
        ],
    );
    let workspace = fixture.add_session("s");
    fixture.ask("s", "Run and write.");
    fixture.wait_for_permission_count("s", 1).await;
    fixture.answer("s", Answer::allow_once());
    fixture.wait_for_permission_count("s", 2).await;
    fixture.answer("s", Answer::deny());
    let completed = tokio::time::timeout(
        Duration::from_secs(2),
        fixture.wait_for_status("s", Status::Idle),
    )
    .await;
    if completed.is_err() {
        fixture.cancel("s");
        fixture.wait_for_status("s", Status::Idle).await;
    }
    assert!(
        completed.is_ok(),
        "a rejection must end the turn without another permission"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("approved.txt")).unwrap(),
        "approved"
    );
    assert!(!workspace.join("denied.txt").exists());
    assert!(!workspace.join("skipped.txt").exists());
    assert_eq!(fixture.chat_requests().len(), 1);
    assert_eq!(result_texts(&fixture.events("s")), ["Stopped."]);

    fixture.ask("s", "Start again.");
    fixture.wait_for_cards("s", 4).await;
    fixture.wait_for_status("s", Status::Idle).await;
    let requests = fixture.chat_requests();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_tool_exchange(
        &messages[2..6],
        &[
            ("run", "exited 0"),
            ("write_file", "Not allowed"),
            ("write_file", "Skipped"),
        ],
    );
    assert_eq!(messages[6]["role"], "user");
}

#[tokio::test]
async fn tool_batches_assemble_streams_and_serialize_file_aliases_after_stale_approval() {
    let gate = HoldGate::new();
    let head = sse_data(serde_json::json!({"choices": [{"delta": {
        "content": "Writing and reading.",
        "tool_calls": [
            {"index": 1, "id": "read-alias", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"alias.txt\"}"}},
            {"index": 0, "id": "write-target", "type": "function", "function": {"name": "write_file", "arguments": "{\"path\":\"data.txt\",\"contents\":"}}
        ]
    }}]}))
        + &reasoning_event("arguments pending");
    let tail = sse_data(serde_json::json!({"choices": [{"delta": {"tool_calls": [
        {"index": 0, "function": {"arguments": "\"after\"}"}}
    ]}, "finish_reason": "tool_calls"}]}));
    let fixture = Fixture::new(
        "streamed-aliases",
        vec![
            Canned::Hold {
                head,
                tail,
                gate: gate.clone(),
            },
            Canned::Json(text_reply("Read after.")),
        ],
    );
    let workspace = fixture.add_session("s");
    fs::write(workspace.join("data.txt"), "before").unwrap();
    std::os::unix::fs::symlink("data.txt", workspace.join("alias.txt")).unwrap();
    fixture.ask("s", "Write then read the alias.");
    fixture.wait_for_thinking("s", "arguments pending").await;
    assert!(fixture
        .events("s")
        .iter()
        .all(|event| event.kind != EventKind::ToolCall));
    gate.release();
    fixture.wait_for_permission_count("s", 1).await;
    fs::write(workspace.join("data.txt"), "external change").unwrap();
    fixture.answer("s", Answer::allow_once());
    fixture.wait_for_permission_count("s", 2).await;
    assert_eq!(
        fs::read_to_string(workspace.join("data.txt")).unwrap(),
        "external change"
    );
    assert_eq!(fixture.chat_requests().len(), 1);
    fixture.answer("s", Answer::allow_once());
    fixture.wait_for_status("s", Status::Idle).await;
    let requests = fixture.chat_requests();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_tool_exchange(
        &messages[2..],
        &[("write_file", "Replaced"), ("read_file", "after")],
    );
    assert_eq!(messages[2]["content"], "Writing and reading.");
    assert_eq!(messages[2]["tool_calls"][0]["id"], "write-target");
    assert_eq!(messages[2]["tool_calls"][1]["id"], "read-alias");
    assert_eq!(
        fs::read_to_string(workspace.join("data.txt")).unwrap(),
        "after"
    );
}

#[tokio::test]
async fn tool_batches_keep_successes_and_errors_for_the_next_model_step() {
    let mut batch: serde_json::Value = serde_json::from_str(&tool_call_reply(vec![
        ("unknown_tool", serde_json::json!({})),
        ("read_file", serde_json::json!({})),
        ("read_file", serde_json::json!({"path": "missing.txt"})),
        ("read_file", serde_json::json!({"path": "present.txt"})),
        (
            "run",
            serde_json::json!({"argv": ["sh", "-c", "printf failed-command; exit 7"]}),
        ),
    ]))
    .unwrap();
    batch["choices"][0]["message"]["tool_calls"][1]["function"]["arguments"] = "{broken".into();
    let fixture = Fixture::new(
        "mixed-batch",
        vec![
            Canned::Json(batch.to_string()),
            Canned::Json(tool_call_reply(vec![(
                "read_file",
                serde_json::json!({"path": "present.txt"}),
            )])),
            Canned::Json(text_reply("Recovered.")),
        ],
    );
    let workspace = fixture.add_session("s");
    fs::write(workspace.join("present.txt"), "surviving-result").unwrap();
    fixture.ask("s", "Read and run.");
    fixture.wait_for_waiting_permission("s").await;
    fixture.answer("s", Answer::allow_once());
    fixture.wait_for_status("s", Status::Idle).await;
    let requests = fixture.chat_requests();
    assert_eq!(requests.len(), 3);
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_tool_exchange(
        &messages[2..],
        &[
            ("unknown_tool", "unknown tool"),
            ("read_file", "{broken"),
            ("read_file", "missing.txt"),
            ("read_file", "surviving-result"),
            ("run", "exited 7"),
        ],
    );
    assert_eq!(
        messages[2]["tool_calls"],
        batch["choices"][0]["message"]["tool_calls"]
    );
    let final_messages = requests[2]["messages"].as_array().unwrap();
    assert_eq!(&final_messages[..messages.len()], messages);
    assert_tool_exchange(
        &final_messages[messages.len()..],
        &[("read_file", "surviving-result")],
    );
    assert_eq!(result_texts(&fixture.events("s")), ["Recovered."]);
}

#[tokio::test]
async fn tool_batches_retry_the_model_request_without_repeating_a_command() {
    let fixture = Fixture::new("retry-after-tool", vec![
        Canned::Json(tool_call_reply(vec![("run", serde_json::json!({"argv": ["sh", "-c", "printf x >> once.txt"]}))])),
        Canned::Status(429, serde_json::json!({"error": {"message": "busy", "metadata": {"headers": {"Retry-After": "0"}}}}).to_string()),
        Canned::Json(text_reply("Done.")),
    ]);
    let workspace = fixture.add_session("s");
    fixture.ask("s", "Run once.");
    fixture.wait_for_waiting_permission("s").await;
    fixture.answer("s", Answer::allow_once());
    fixture.wait_for_status("s", Status::Idle).await;
    assert_eq!(fs::read_to_string(workspace.join("once.txt")).unwrap(), "x");
    let requests = fixture.chat_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1], requests[2]);
    assert_tool_exchange(
        &requests[2]["messages"].as_array().unwrap()[2..],
        &[("run", "exited 0")],
    );
    assert_eq!(
        fixture
            .events("s")
            .iter()
            .filter(|event| event.kind == EventKind::ToolCall)
            .count(),
        1
    );
}

#[tokio::test]
async fn tool_batches_cancel_every_running_command_and_replay_the_results() {
    let fixture = Fixture::new(
        "cancel-batch",
        vec![
            Canned::Json(tool_call_reply(vec![
                (
                    "run",
                    serde_json::json!({"argv": ["sh", "-c", "printf '%s' $$ > first.pid; sleep 30; touch late.txt"]}),
                ),
                (
                    "run",
                    serde_json::json!({"argv": ["sh", "-c", "printf '%s' $$ > second.pid; sleep 30; touch late.txt"]}),
                ),
            ])),
            Canned::Json(text_reply("Resumed.")),
        ],
    );
    let workspace = fixture.add_session("s");
    Session::at(&fixture.root.join("session-s"))
        .set_yolo(true)
        .unwrap();
    fixture.ask("s", "Start both.");
    let deadline = Instant::now() + Duration::from_secs(10);
    while ["first.pid", "second.pid"].iter().any(|name| {
        fs::read_to_string(workspace.join(name))
            .ok()
            .and_then(|text| text.parse::<i32>().ok())
            .is_none()
    }) {
        assert!(Instant::now() < deadline, "both commands must start");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fixture.cancel("s");
    fixture.wait_for_status("s", Status::Idle).await;
    assert_eq!(result_texts(&fixture.events("s")), ["Stopped."]);
    for name in ["first.pid", "second.pid"] {
        let pid: i32 = fs::read_to_string(workspace.join(name))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    assert!(!workspace.join("late.txt").exists());
    assert_eq!(fixture.chat_requests().len(), 1);
    fixture.ask("s", "Continue.");
    fixture.wait_for_cards("s", 4).await;
    fixture.wait_for_status("s", Status::Idle).await;
    let requests = fixture.chat_requests();
    assert_eq!(requests.len(), 2);
    assert_tool_exchange(
        &requests[1]["messages"].as_array().unwrap()[2..5],
        &[
            ("run", "was killed by a signal"),
            ("run", "was killed by a signal"),
        ],
    );
    assert_eq!(result_texts(&fixture.events("s")), ["Stopped.", "Resumed."]);
}

#[tokio::test]
async fn tool_batches_apply_dependent_edits_to_the_written_file() {
    let fixture = Fixture::new(
        "dependent-edits",
        vec![
            Canned::Json(tool_call_reply(vec![
                (
                    "write_file",
                    serde_json::json!({"path": "data.txt", "contents": "first"}),
                ),
                (
                    "search_replace",
                    serde_json::json!({"path": "./data.txt", "old_string": "first", "new_string": "second"}),
                ),
                ("read_file", serde_json::json!({"path": "data.txt"})),
            ])),
            Canned::Json(text_reply("Edited.")),
        ],
    );
    let workspace = fixture.add_session("s");
    fixture.ask("s", "Create, edit, and read.");
    fixture.wait_for_permission_count("s", 1).await;
    fixture.answer("s", Answer::allow_once());
    fixture.wait_for_permission_count("s", 2).await;
    let permission = fixture
        .events("s")
        .into_iter()
        .rfind(|event| event.kind == EventKind::Permission)
        .unwrap();
    assert_eq!(permission.body["contents"], "second");
    assert_eq!(
        fs::read_to_string(workspace.join("data.txt")).unwrap(),
        "first"
    );
    fixture.answer("s", Answer::allow_once());
    fixture.wait_for_status("s", Status::Idle).await;
    assert_eq!(
        fs::read_to_string(workspace.join("data.txt")).unwrap(),
        "second"
    );
    let requests = fixture.chat_requests();
    assert_eq!(requests.len(), 2);
    assert_tool_exchange(
        &requests[1]["messages"].as_array().unwrap()[2..],
        &[
            ("write_file", "Created"),
            ("search_replace", "Replaced"),
            ("read_file", "second"),
        ],
    );
}

#[tokio::test]
async fn tool_batches_finish_pairs_skipped_calls_without_executing_them() {
    let fixture = Fixture::new(
        "finish-batch",
        vec![
            Canned::Json(tool_call_reply(vec![
                ("finish", serde_json::json!({"text": "Finished."})),
                (
                    "write_file",
                    serde_json::json!({"path": "skipped.txt", "contents": "no"}),
                ),
            ])),
            Canned::Json(text_reply("Next turn.")),
        ],
    );
    let workspace = fixture.add_session("s");
    fixture.ask("s", "Finish.");
    fixture.wait_for_turn_to_start("s").await;
    fixture.wait_for_status("s", Status::Idle).await;
    assert_eq!(result_texts(&fixture.events("s")), ["Finished."]);
    assert!(!workspace.join("skipped.txt").exists());
    assert!(fixture
        .events("s")
        .iter()
        .all(|event| event.kind != EventKind::Permission));
    assert_eq!(fixture.chat_requests().len(), 1);
    fixture.ask("s", "Start again.");
    fixture.wait_for_cards("s", 4).await;
    fixture.wait_for_status("s", Status::Idle).await;
    let requests = fixture.chat_requests();
    assert_tool_exchange(
        &requests[1]["messages"].as_array().unwrap()[2..5],
        &[("finish", "Finished."), ("write_file", "Skipped")],
    );
}

#[tokio::test]
async fn tool_batches_invalid_typed_arguments_return_errors_without_permission() {
    let fixture = Fixture::new(
        "invalid-batch-arguments",
        vec![
            Canned::Json(tool_call_reply(vec![
                (
                    "read_file",
                    serde_json::json!({"path": std::env::temp_dir(), "limit": -1}),
                ),
                (
                    "web_search",
                    serde_json::json!({"query": "test", "num_results": -1}),
                ),
                (
                    "search_replace",
                    serde_json::json!({"path": "data.txt", "old_string": "before", "new_string": "after", "replace_all": "yes"}),
                ),
            ])),
            Canned::Json(text_reply("Recovered.")),
        ],
    );
    let workspace = fixture.add_session("s");
    fs::write(workspace.join("data.txt"), "before").unwrap();
    fixture.ask("s", "Handle invalid calls.");
    fixture.wait_for_turn_to_start("s").await;
    let completed = tokio::time::timeout(
        Duration::from_secs(2),
        fixture.wait_for_status("s", Status::Idle),
    )
    .await;
    if completed.is_err() {
        fixture.cancel("s");
        fixture.wait_for_status("s", Status::Idle).await;
    }
    assert!(
        completed.is_ok(),
        "invalid arguments must return feedback without a permission prompt"
    );
    assert!(fixture
        .events("s")
        .iter()
        .all(|event| event.kind != EventKind::Permission));
    assert_eq!(
        fs::read_to_string(workspace.join("data.txt")).unwrap(),
        "before"
    );
    let requests = fixture.chat_requests();
    assert_eq!(requests.len(), 2);
    assert_tool_exchange(
        &requests[1]["messages"].as_array().unwrap()[2..],
        &[
            ("read_file", "limit must"),
            ("web_search", "num_results must"),
            ("search_replace", "replace_all must"),
        ],
    );
}
