//! The tools, and the gate every write and every command stops at.
//!
//! Everything here goes through the public surface: a session directory, a
//! workspace on disk, and the tools held against it. The model is not here.
//! A "canned call" is a tool invoked directly, and a canned answer is one put
//! in the gate before the card goes up, which is the same path the server's
//! answer route takes later.
//!
//! What these tests are really about is the order of events. A file is not on
//! disk until the answer says it may be, a read leaves nothing behind, a deny
//! changes nothing, and the one thing a write puts on screen is the permission
//! card.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kyotoagent::events::{
    AskBody, Decision, Event, EventKind, ModelMessageBody, PermissionBody, ResultBody,
    ToolCallBody, ToolResultBody,
};
use kyotoagent::permit::{self, Answer};
use kyotoagent::screen::Status;
use kyotoagent::session::{Session, SessionMeta};
use kyotoagent::tools::{Tools, DEFAULT_TIMEOUT_SEC};
use kyotoagent::view;

const AT: &str = "2026-09-29T00:00:00.000Z";

#[test]
fn question_visuals_read_files_through_the_permission_gate() {
    let f = fixture("question-visuals");
    let source = "flowchart LR\n A[Plan] --> B[Decide]";
    fs::write(f.workspace.join("flow.mmd"), source).unwrap();
    fs::write(f.workspace.join("flow.svg"), r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="80" height="40" fill="blue"/></svg>"#).unwrap();
    image::RgbImage::new(20, 10)
        .save(f.workspace.join("preview.png"))
        .unwrap();
    for path in ["flow.mmd", "flow.svg", "preview.png"] {
        let visuals = kyotoagent::question::prepare(&f.tools, "t1", &serde_json::json!({"visuals":[{"title":"Route","alt":"Plan then decide","path":path}]})).unwrap();
        visuals[0].image.validate().unwrap();
    }
    let outside = f.root.join("private.svg");
    fs::write(&outside, "private").unwrap();
    f.tools.gate().queue(Answer::deny());
    assert!(kyotoagent::question::prepare(
        &f.tools,
        "t1",
        &serde_json::json!({"visuals":[{"title":"Route","alt":"Private","path":outside}]})
    )
    .unwrap_err()
    .contains("denied"));
    assert_eq!(f.permissions().len(), 1);
    assert!(kyotoagent::question::prepare(
        &f.tools,
        "t1",
        &serde_json::json!({"visuals":[{},{},{}]})
    )
    .is_err());
    assert!(kyotoagent::question::prepare(&f.tools, "t1", &serde_json::json!({"visuals":[{"title":"Route","alt":"Plan","path":"flow.svg","mermaid":source}]})).is_err());
}

#[test]
fn outside_proof_files_require_read_permission_and_allow_session_remembers_that_path() {
    let f = fixture("proof-outside-allow");
    let path = f.root.join("evidence.bin");
    fs::write(&path, b"\0\xffverified").unwrap();
    f.tools.gate().queue(Answer::allow_session());
    let file = f
        .tools
        .attach_artifact("t1", path.to_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        fs::read(f.session.dir().join("proof").join(&file.id)).unwrap(),
        b"\0\xffverified"
    );
    let body: PermissionBody = f.permissions()[0].body_as().unwrap();
    assert_eq!(body.path.as_deref(), path.to_str());
    assert!(f
        .session
        .meta()
        .unwrap()
        .allow
        .allows_outside_read(path.to_str().unwrap()));
    assert!(f
        .tools
        .attach_artifact("t1", path.to_str().unwrap())
        .unwrap()
        .is_some());
    assert_eq!(f.permissions().len(), 1);
}

#[test]
fn denied_outside_proof_and_unsafe_workspace_paths_do_not_create_artifacts() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let f = fixture("proof-outside-deny");
    let path = f.root.join("evidence.txt");
    fs::write(&path, "private evidence").unwrap();
    symlink(&path, f.workspace.join("escape")).unwrap();
    assert!(f.tools.attach_artifact("t1", "escape").is_err());
    assert!(f.tools.attach_artifact("t1", ".").is_err());
    let unreadable = f.workspace.join("unreadable");
    fs::write(&unreadable, "private").unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(f.tools.attach_artifact("t1", "unreadable").is_err());
    assert!(f.permissions().is_empty());
    f.tools.gate().queue(Answer::deny());
    assert!(f
        .tools
        .attach_artifact("t1", path.to_str().unwrap())
        .unwrap()
        .is_none());
    assert!(!f.session.dir().join("proof").exists());
}

/// A workspace, a session pointed at it, and the tools. The temporary
/// directories go when the fixture does, so a failed test leaves nothing
/// behind for the next run to trip over.
struct Fixture {
    tools: Tools,
    workspace: PathBuf,
    session: Session,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A fixture of this test's own, named after the test so two runs, or two tests
/// on one machine, cannot tread on each other.
fn fixture(name: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("kyotoagent-gate-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let workspace = root.join("w");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    // The tools canonicalize every path they touch, so a path the test builds
    // from `root` has to be canonical too. On macOS `temp_dir` is a symlink
    // (`/var` into `/private/var`), and the two spellings would not compare.
    let root = fs::canonicalize(&root).expect("the root resolves");
    let workspace = root.join("w");
    let session = Session::at(&root.join("session"));
    session
        .create(&SessionMeta::new("91bc", &workspace, "gpt", AT))
        .expect("the session is created");
    let tools = Tools::at(&session).expect("the tools are built");
    let workspace = tools.workspace().to_path_buf();
    Fixture {
        tools,
        workspace,
        session,
        root,
    }
}

impl Fixture {
    /// The cards a screen would draw right now.
    fn cards(&self) -> Vec<view::Card> {
        view::read(self.session.dir())
            .expect("the session projects to cards")
            .cards
    }

    fn status(&self) -> Status {
        view::read(self.session.dir())
            .expect("the session projects to cards")
            .status
    }

    /// The cards as the JSON a screen is handed.
    fn cards_json(&self) -> String {
        serde_json::to_string(&self.cards()).expect("cards serialize")
    }

    /// The permission events in the log, oldest first.
    fn permissions(&self) -> Vec<Event> {
        self.session
            .events()
            .expect("the log reads")
            .into_iter()
            .filter(|event| event.kind == EventKind::Permission)
            .collect()
    }

    /// Wait until the log holds `count` permission cards, and return their
    /// bodies. A turn that is stopped is stopped on a card, so this is how a
    /// test knows the question is on screen.
    fn wait_for_permissions(&self, count: usize) -> Vec<kyotoagent::events::PermissionBody> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let cards: Vec<kyotoagent::events::PermissionBody> = self
                .permissions()
                .iter()
                .map(|event| event.body_as().expect("a permission body parses"))
                .collect();
            if cards.len() >= count && self.tools.gate().open_permission().is_some() {
                return cards;
            }
            assert!(
                Instant::now() < deadline,
                "only {} of {count} permissions came up in ten seconds",
                cards.len()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The body of the one card a blocked turn put up.
    fn wait_for_permission(&self) -> kyotoagent::events::PermissionBody {
        self.wait_for_permissions(1).remove(0)
    }

    /// Append an event of this test's own, standing in for the turn loop.
    fn append(&self, kind: EventKind, body: &serde_json::Value) {
        let id = self.session.next_event_id().expect("an id");
        let event = Event::new(&id, AT, "t1", kind)
            .with_body(body)
            .expect("a body serializes");
        self.session.append(&event).expect("the event is appended");
    }

    fn ask(&self, text: &str) {
        self.append(
            EventKind::UserAsk,
            &serde_json::to_value(AskBody {
                images: Vec::new(),
                text: text.into(),
                context: String::new(),
                skill: String::new(),
                silent: false,
            })
            .expect("an ask body"),
        );
    }

    fn result(&self, text: &str) {
        self.append(
            EventKind::Result,
            &serde_json::to_value(ResultBody {
                text: text.into(),
                note: String::new(),
            })
            .expect("a result body"),
        );
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("the file is readable")
}

#[test]
fn a_write_lands_only_after_the_answer_and_allow_once_remembers_nothing() {
    let f = fixture("allow-once");
    let path = f.workspace.join("README.md");
    let gate = f.tools.gate().clone();
    let tools = f.tools.clone();

    // The turn calls the tool and stops on the card. The file must not be on
    // disk while the question is open: that is the whole promise.
    let turn =
        thread::spawn(move || tools.write_file("t1", "README.md", "kyotoagent is the binary.\n"));

    let card = f.wait_for_permission();
    assert_eq!(card.action, "Create README.md");
    assert_eq!(card.path.as_deref(), Some(path.to_str().expect("a path")));
    assert_eq!(
        card.diff.as_deref(),
        Some(
            [
                "@@ -1 +1 @@".to_string(),
                "+kyotoagent is the binary.".to_string()
            ]
            .as_slice()
        ),
        "the card carries the change it is asking about"
    );
    assert!(!path.exists(), "the file is not there before the answer");
    assert_eq!(f.status(), Status::Waiting, "and the session is waiting");

    gate.answer(Answer::allow_once()).expect("the card is open");
    let written = turn
        .join()
        .expect("the write finished")
        .expect("the write was allowed");

    assert!(written.created);
    assert_eq!(read(&path), "kyotoagent is the binary.\n");
    assert_eq!(f.status(), Status::Working, "the turn carries on");
    let allow = f.tools.gate().meta().expect("meta reads").allow;
    assert!(
        allow.write_paths.is_empty(),
        "allow_once is this once: {allow:?}"
    );
    // The card the user answered is still in the log, with its diff.
    assert_eq!(f.permissions().len(), 1);
}

#[test]
fn a_read_inside_the_workspace_returns_bytes_and_asks_nothing() {
    let f = fixture("quiet-read");
    fs::write(f.workspace.join("Cargo.toml"), "name = \"kyotoagent\"\n").expect("a file");
    f.ask("What is the package name?");
    f.append(
        EventKind::ToolCall,
        &serde_json::to_value(ToolCallBody {
            tool: "read_file".into(),
            args: serde_json::json!({ "path": "Cargo.toml" }),
        })
        .expect("a tool call body"),
    );

    let read = f
        .tools
        .read_file("t1", "Cargo.toml", Some(0), None, None)
        .expect("a read inside the workspace runs");
    f.append(
        EventKind::ToolResult,
        &serde_json::to_value(ToolResultBody {
            is_error: false,
            images: Vec::new(),
            tool: "read_file".into(),
            output: read.text.clone(),
        })
        .expect("a tool result body"),
    );
    f.result("The package name is kyotoagent.");

    assert_eq!(read.text, "name = \"kyotoagent\"\n");
    assert_eq!(read.next_offset, None, "and it is all of it");
    assert!(f.permissions().is_empty(), "a read asks for nothing");
    // The ask and the result are on screen, and the read is not.
    let kinds: Vec<&str> = f.cards().iter().map(|card| card.kind.label()).collect();
    assert_eq!(kinds, vec!["ask", "result"]);
    assert!(
        !f.cards_json().contains("read_file"),
        "and the tool's name is nowhere in the cards"
    );
}

#[test]
fn a_symlink_that_leaves_the_workspace_is_refused() {
    let f = fixture("symlink");
    let outside = f.root.join("outside");
    fs::create_dir_all(&outside).expect("somewhere else");
    fs::write(outside.join("secret.txt"), "not for the agent\n").expect("a file elsewhere");
    // The shape of the trick this refuses: a link in the repository, pointing
    // at somewhere the workspace is not.
    std::os::unix::fs::symlink(&outside, f.workspace.join("away")).expect("a link out");

    let error = f
        .tools
        .read_file("t1", "away/secret.txt", None, None, None)
        .expect_err("a link out of the workspace is not read through");
    assert!(
        error.to_string().contains("outside the workspace"),
        "{error}"
    );
    // A write through it is the same answer, and the file out there is as it
    // was.
    let error = f
        .tools
        .write_file("t1", "away/secret.txt", "mine now\n")
        .expect_err("and a write through it is refused too");
    assert!(
        error.to_string().contains("outside the workspace"),
        "{error}"
    );
    assert_eq!(read(&outside.join("secret.txt")), "not for the agent\n");
    assert!(f.permissions().is_empty(), "and nothing was asked");
}

#[test]
fn an_allowed_path_stays_allowed_and_another_one_asks_again() {
    let f = fixture("session-allow");
    let gate = f.tools.gate().clone();
    gate.queue(Answer::allow_session());

    let first = f
        .tools
        .write_file("t1", "a.md", "# a\n")
        .expect("the first write runs");
    assert!(first.created);
    assert_eq!(f.permissions().len(), 1, "the first write asked");
    assert_eq!(
        f.tools.gate().meta().expect("meta reads").allow.write_paths,
        vec![first.path.clone()],
        "and the answer was remembered for that one path"
    );

    // The same path again: no card, no question, straight through.
    let second = f
        .tools
        .write_file("t1", "a.md", "# a\n\nmore\n")
        .expect("the second write runs");
    assert!(second.replaced);
    assert_eq!(f.permissions().len(), 1, "and nothing was asked");

    // A different path is a different question, however alike the first was.
    gate.queue(Answer::deny());
    let third = f
        .tools
        .write_file("t1", "b.md", "# b\n")
        .expect("the third write is answered");
    assert!(third.denied);
    assert_eq!(f.permissions().len(), 2, "so it asked again");
    assert!(
        !f.workspace.join("b.md").exists(),
        "and the file was not written"
    );
    assert_eq!(
        f.tools.gate().meta().expect("meta reads").allow.write_paths,
        vec![first.path.clone()],
        "and a deny remembers nothing"
    );
}

#[test]
fn a_deny_is_the_tool_result_and_the_file_is_exactly_as_it_was() {
    let f = fixture("deny");
    let path = f.workspace.join("README.md");
    fs::write(&path, "# the old readme\n").expect("a file to protect");
    f.tools.gate().queue(Answer::deny());

    let written = f
        .tools
        .write_file("t1", "README.md", "# the new readme\n")
        .expect("a denied write is a result, not a failure");

    assert!(written.denied);
    assert!(!written.created && !written.replaced);
    assert_eq!(read(&path), "# the old readme\n", "the file did not move");
    assert_eq!(
        written.summary(),
        format!("Not allowed, so {} was not written.", path.display())
    );

    let cards = f.cards();
    assert!(cards
        .iter()
        .all(|card| card.kind != view::CardKind::Permission));
    assert!(cards.is_empty());
    let log = fs::read_to_string(f.session.events_path()).expect("the log is readable");
    assert!(log.contains("\"+# the new readme"), "{log}");
    assert!(log.contains("\"decision\":\"deny\""), "{log}");
}

#[test]
fn a_turn_that_reads_and_then_is_allowed_to_write_shows_only_the_permission() {
    let f = fixture("quiet-view");
    f.ask("Add a readme line that names the binary.");
    fs::write(f.workspace.join("README.md"), "# kyotoagent\n").expect("a file");
    f.append(
        EventKind::ModelMessage,
        &serde_json::to_value(ModelMessageBody {
            text: "Reading the readme.".into(),
        })
        .expect("a message body"),
    );
    f.append(
        EventKind::ToolCall,
        &serde_json::to_value(ToolCallBody {
            tool: "read_file".into(),
            args: serde_json::json!({ "path": "README.md" }),
        })
        .expect("a tool call body"),
    );
    let read = f
        .tools
        .read_file("t1", "README.md", None, None, None)
        .expect("the read runs");
    f.append(
        EventKind::ToolResult,
        &serde_json::to_value(ToolResultBody {
            is_error: false,
            images: Vec::new(),
            tool: "read_file".into(),
            output: read.text.clone(),
        })
        .expect("a tool result body"),
    );

    f.tools.gate().queue(Answer::allow_once());
    f.append(
        EventKind::ToolCall,
        &serde_json::to_value(ToolCallBody {
            tool: "write_file".into(),
            args: serde_json::json!({ "path": "README.md", "contents": "..." }),
        })
        .expect("a tool call body"),
    );
    let written = f
        .tools
        .write_file(
            "t1",
            "README.md",
            "# kyotoagent\n\nkyotoagent is the binary.\n",
        )
        .expect("the write is allowed");
    f.append(
        EventKind::ToolResult,
        &serde_json::to_value(ToolResultBody {
            is_error: false,
            images: Vec::new(),
            tool: "write_file".into(),
            output: written.summary(),
        })
        .expect("a tool result body"),
    );
    f.result("The readme now names the binary.");

    let kinds: Vec<&str> = f.cards().iter().map(|card| card.kind.label()).collect();
    assert_eq!(kinds, vec!["ask", "result"]);
    let json = f.cards_json();
    for name in ["read_file", "write_file", "list_dir", "run"] {
        assert!(!json.contains(name), "no card names {name}: {json}");
    }
    // The log is the other half of the promise: both tool calls are in it, in
    // the order the turn made them.
    let log = fs::read_to_string(f.session.events_path()).expect("the log is readable");
    assert!(log.contains("read_file"), "{log}");
    assert!(log.contains("write_file"), "{log}");
}

#[test]
fn a_read_outside_the_workspace_names_its_absolute_path_on_the_card() {
    let f = fixture("outside-read");
    let outside = f.root.join("outside");
    fs::create_dir_all(&outside).expect("somewhere else");
    let secret = outside.join("notes.txt");
    fs::write(&secret, "a note for the agent\n").expect("a file elsewhere");

    f.tools.gate().queue(Answer::allow_session());
    let read = f
        .tools
        .read_file("t1", &secret.display().to_string(), Some(0), None, None)
        .expect("the read is allowed");

    assert_eq!(read.text, "a note for the agent\n");
    let card = f.permissions().remove(0);
    let body: kyotoagent::events::PermissionBody = card.body_as().expect("a permission body");
    assert_eq!(
        body.path.as_deref(),
        Some(secret.display().to_string().as_str()),
        "the card names the absolute path"
    );
    assert!(body.action.starts_with("Read "), "{}", body.action);
    // The exact path is remembered, and a different one is not.
    assert_eq!(
        f.tools
            .gate()
            .meta()
            .expect("meta reads")
            .allow
            .outside_read_paths,
        vec![secret.display().to_string()]
    );
    let other = outside.join("other.txt");
    fs::write(&other, "another\n").expect("another file elsewhere");
    f.tools.gate().queue(Answer::deny());
    let refused = f
        .tools
        .read_file("t1", &other.display().to_string(), None, None, None)
        .expect("the other read is answered");
    assert!(refused.denied);
    assert_eq!(f.permissions().len(), 2, "and it asked again");
}

#[test]
fn a_listing_outside_the_workspace_asks_like_a_read() {
    let f = fixture("outside-list");
    let outside = f.root.join("outside");
    fs::create_dir_all(outside.join("sub")).expect("somewhere else");
    fs::write(outside.join("one.txt"), "1").expect("a file elsewhere");

    f.tools.gate().queue(Answer::allow_session());
    let listed = f
        .tools
        .list_dir("t1", &outside.display().to_string())
        .expect("the listing is allowed");
    assert_eq!(
        listed
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<&str>>(),
        vec!["one.txt", "sub"]
    );
    assert_eq!(f.permissions().len(), 1, "it asked, even for a listing");
}

#[test]
fn a_command_asks_for_the_argv_it_would_run_and_the_default_timeout_is_not_on_the_card() {
    let f = fixture("command");
    f.tools.gate().queue(Answer::allow_session());
    let argv = vec!["sh".to_string(), "-c".to_string(), "echo hello".to_string()];

    let output = f.tools.run("t1", &argv, None).expect("the command runs");
    assert_eq!(output.exit, Some(0));
    assert!(output.stdout.contains("hello"));

    let body: kyotoagent::events::PermissionBody = f
        .permissions()
        .remove(0)
        .body_as()
        .expect("a permission body");
    assert_eq!(
        body.argv.as_deref(),
        Some(argv.as_slice()),
        "the card has the argv"
    );
    assert_eq!(
        body.timeout_sec, None,
        "a timeout nobody changed is not on the card"
    );

    // The remembered argv is the whole command, word for word.
    let allow = f.tools.gate().meta().expect("meta reads").allow;
    assert_eq!(allow.argv, vec![argv.clone()]);
    let other = vec!["sh".to_string(), "-c".to_string(), "echo other".to_string()];
    f.tools.gate().queue(Answer::deny());
    let refused = f
        .tools
        .run("t1", &other, None)
        .expect("the other command is answered");
    assert!(refused.denied, "a different argv is a different question");
    assert_eq!(f.permissions().len(), 2);
}

#[test]
fn a_timeout_that_is_not_the_default_is_part_of_the_question() {
    let f = fixture("timeout");
    f.tools.gate().queue(Answer::allow_once());

    f.tools
        .run("t1", &["true".to_string()], Some(1))
        .expect("the command runs");

    let body: kyotoagent::events::PermissionBody = f
        .permissions()
        .remove(0)
        .body_as()
        .expect("a permission body");
    assert_eq!(body.timeout_sec, Some(1));
    assert_eq!(
        DEFAULT_TIMEOUT_SEC, 120,
        "and 120 is what a card leaves alone"
    );
}

#[test]
fn a_denial_starts_no_command() {
    let f = fixture("no-command");
    let marker = f.workspace.join("ran.txt");
    f.tools.gate().queue(Answer::deny());

    let output = f
        .tools
        .run(
            "t1",
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("touch {}", marker.display()),
            ],
            None,
        )
        .expect("a denied command is a result, not a failure");

    assert!(output.denied);
    assert!(output.exit.is_none(), "there is no exit to report");
    assert!(!marker.exists(), "the command never started");
    assert_eq!(
        output.summary(),
        format!(
            "Not allowed, so sh -c touch {} did not run.",
            marker.display()
        )
    );
}

#[test]
fn a_file_that_changes_while_the_card_is_up_asks_again() {
    let f = fixture("stale");
    let path = f.workspace.join("README.md");
    fs::write(&path, "one\n").expect("a file to protect");
    let gate = f.tools.gate().clone();
    let tools = f.tools.clone();

    let turn = thread::spawn(move || tools.write_file("t1", "README.md", "two\n"));

    // The first card is about `one`. While it is up, somebody else writes the
    // file, and the answer that comes back is about a file that is no longer
    // there.
    let first = f.wait_for_permissions(1);
    assert_eq!(
        first[0].diff.as_deref(),
        Some(
            [
                "@@ -1 +1 @@".to_string(),
                "-one".to_string(),
                "+two".to_string()
            ]
            .as_slice()
        )
    );
    fs::write(&path, "one\nand a half\n").expect("somebody else writes");
    gate.answer(Answer::allow_session())
        .expect("the first card is open");

    // So the write asks again, with the diff the file has now, and the
    // allow_session it was given is spent with it.
    let second = f.wait_for_permissions(2);
    assert_eq!(
        second[1].diff.as_deref(),
        Some(
            [
                "@@ -1,2 +1 @@".to_string(),
                "-one".to_string(),
                "-and a half".to_string(),
                "+two".to_string(),
            ]
            .as_slice()
        ),
        "and the new card carries the diff the file has now"
    );
    assert_eq!(
        second[1].bytes,
        Some(4),
        "with the size of what it proposes"
    );
    assert_eq!(
        second[1].old_hash.as_deref(),
        Some(permit::fingerprint(b"one\nand a half\n").as_str()),
        "and the digest of what is on disk now"
    );
    gate.answer(Answer::allow_once())
        .expect("the second card is open");

    let written = turn
        .join()
        .expect("the write finished")
        .expect("the write was allowed");
    assert!(written.replaced);
    assert_eq!(read(&path), "two\n");
    assert_eq!(
        f.permissions().len(),
        2,
        "two cards, and the first answer was spent"
    );
    assert!(
        f.tools
            .gate()
            .meta()
            .expect("meta reads")
            .allow
            .write_paths
            .is_empty(),
        "an allow_session about a file that moved remembers nothing"
    );
}

#[test]
fn an_allow_on_one_session_is_invisible_to_another() {
    let f = fixture("two-sessions");
    let other = Session::at(&f.root.join("other"));
    other
        .create(&SessionMeta::new("3f2a", &f.workspace, "gpt", AT))
        .expect("the second session is created");
    let other_tools = Tools::at(&other).expect("the second session has tools");

    f.tools.gate().queue(Answer::allow_session());
    f.tools
        .write_file("t1", "a.md", "# a\n")
        .expect("the first session writes");

    // The file is on disk, so the second session can read it quietly. What it
    // cannot do is write to that path without asking: the allow was the first
    // session's own.
    other_tools.gate().queue(Answer::deny());
    let written = other_tools
        .write_file("t1", "a.md", "# a, mine\n")
        .expect("the second session is answered");
    assert!(written.denied);
    assert_eq!(read(&f.workspace.join("a.md")), "# a\n");
    assert!(
        other
            .meta()
            .expect("meta reads")
            .allow
            .write_paths
            .is_empty(),
        "and it remembered nothing of its own"
    );
    // A read inside the workspace is still quiet for the second session: the
    // denied write is the only card its log holds.
    let read = other_tools
        .read_file("t1", "a.md", Some(0), None, None)
        .expect("a read runs");
    assert_eq!(read.text, "# a\n");
    let kinds: Vec<EventKind> = other
        .events()
        .expect("the log reads")
        .iter()
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![EventKind::Permission, EventKind::PermissionAnswer],
        "the quiet read asked nothing on the second session either"
    );
}

#[test]
fn a_turn_that_only_wrote_says_that_much_in_the_log() {
    let f = fixture("log");
    f.ask("Rename the binary.");
    f.tools.gate().queue(Answer::allow_once());
    f.tools
        .write_file("t1", "kyotoagent.txt", "kyotoagent\n")
        .expect("the write runs");
    f.result("Done.");

    let events = f.session.events().expect("the log reads");
    let kinds: Vec<EventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![
            EventKind::UserAsk,
            EventKind::Permission,
            EventKind::PermissionAnswer,
            EventKind::Result,
        ],
        "a turn that only wrote says that much and nothing else: {kinds:?}"
    );
    let answer: kyotoagent::events::PermissionAnswerBody =
        events[2].body_as().expect("an answer body");
    assert_eq!(answer.decision, Decision::AllowOnce);
    assert_eq!(
        answer.permission_id.as_deref(),
        Some("e2"),
        "and it names the card"
    );
}

#[test]
fn yolo_allows_a_write_without_waiting() {
    let f = fixture("yolo-write");
    f.session.set_yolo(true).expect("yolo is stored");
    f.tools
        .write_file("t1", "README.md", "hello\n")
        .expect("the write goes through");
    assert_eq!(read(&f.workspace.join("README.md")), "hello\n");
    let events = f.session.events().expect("the log reads");
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::Permission),
        "the permission is in the log"
    );
    let decisions: Vec<Decision> = events
        .iter()
        .filter(|event| event.kind == EventKind::PermissionAnswer)
        .map(|event| {
            event
                .body_as::<kyotoagent::events::PermissionAnswerBody>()
                .expect("an answer body")
                .decision
        })
        .collect();
    assert_eq!(decisions, vec![Decision::AllowOnce]);
}

#[test]
fn yolo_still_waits_on_a_question() {
    let f = fixture("yolo-question");
    f.session.set_yolo(true).expect("yolo is stored");
    let gate = f.tools.gate().clone();
    let turn = thread::spawn(move || {
        gate.ask_question("t1", "Which way?", &["continue".into(), "stop".into()])
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if f.status() == Status::Waiting {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the question did not wait under yolo"
        );
        thread::sleep(Duration::from_millis(10));
    }
    f.tools
        .gate()
        .answer_question_for(
            Some(&f.tools.gate().open_question().unwrap()),
            "continue".into(),
        )
        .expect("the question is open");
    let answer = turn.join().expect("the ask finished").expect("the answer");
    assert_eq!(answer, "continue");
}

#[test]
fn grep_finds_a_line_and_asks_nothing() {
    let f = fixture("grep-quiet");
    fs::write(
        f.workspace.join("notes.rs"),
        "fn main() {}\nlet needle = \"kyotoagent-grep-unique\";\n",
    )
    .expect("a file");
    let found = f
        .tools
        .grep("kyotoagent-grep-unique", None, None, None)
        .expect("the search runs");
    let summary = found.summary();
    assert!(summary.contains("notes.rs:2:"), "{summary}");
    assert!(summary.contains("kyotoagent-grep-unique"), "{summary}");
    assert!(f.permissions().is_empty(), "a search asks for nothing");
}

#[test]
fn grep_glob_skips_other_suffixes() {
    let f = fixture("grep-glob");
    fs::create_dir(f.workspace.join("src")).expect("a directory");
    fs::write(f.workspace.join("src/lib.rs"), "kyotoagent-grep-glob\n").expect("a rust file");
    fs::write(f.workspace.join("notes.md"), "kyotoagent-grep-glob\n").expect("a markdown file");
    let found = f
        .tools
        .grep("kyotoagent-grep-glob", None, Some("*.rs"), None)
        .expect("the search runs");
    let summary = found.summary();
    assert!(summary.contains("src/lib.rs:1:"), "{summary}");
    assert!(!summary.contains("notes.md"), "{summary}");
    assert!(f.permissions().is_empty(), "a search asks for nothing");
}

#[test]
fn search_replace_waits_for_allow_and_replaces_once() {
    let f = fixture("replace-once");
    let path = f.workspace.join("note.txt");
    fs::write(&path, "hello world\n").expect("a file");
    let tools = f.tools.clone();
    let turn =
        thread::spawn(move || tools.search_replace("t1", "note.txt", "world", "there", false));
    let _card = f.wait_for_permission();
    assert_eq!(
        read(&path),
        "hello world\n",
        "the file waits for the answer"
    );
    f.tools
        .gate()
        .answer(Answer::allow_once())
        .expect("the card is open");
    let written = turn
        .join()
        .expect("the replace finished")
        .expect("the replace was allowed");
    assert!(!written.denied);
    assert_eq!(read(&path), "hello there\n");
    assert_eq!(f.permissions().len(), 1);
}

#[test]
fn search_replace_of_two_matches_asks_nothing() {
    let f = fixture("replace-ambiguous");
    let path = f.workspace.join("note.txt");
    fs::write(&path, "one one\n").expect("a file");
    let tools = f.tools.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(tools.search_replace("t1", "note.txt", "one", "two", false));
    });
    let error = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("two matches return without asking")
        .expect_err("two matches are an error");
    assert!(error.to_string().contains('2'), "{error}");
    assert!(
        f.permissions().is_empty(),
        "an ambiguous replace asks for nothing"
    );
    assert_eq!(read(&path), "one one\n");
}

#[test]
fn search_replace_all_uses_one_allow() {
    let f = fixture("replace-all");
    let path = f.workspace.join("note.txt");
    fs::write(&path, "one one\n").expect("a file");
    f.tools.gate().queue(Answer::allow_once());
    let written = f
        .tools
        .search_replace("t1", "note.txt", "one", "two", true)
        .expect("the replace runs");
    assert!(!written.denied);
    assert_eq!(read(&path), "two two\n");
    assert_eq!(f.permissions().len(), 1, "one allow covers every match");
}

#[test]
fn a_line_read_names_the_line_and_where_to_continue() {
    let f = fixture("line-read");
    fs::write(f.workspace.join("lines.txt"), "one\ntwo\nthree\n").expect("a file");
    let read = f
        .tools
        .read_file("t1", "lines.txt", Some(99), Some(2), Some(1))
        .expect("the line read runs");
    assert!(read.text.starts_with("2→"), "{}", read.text);
    assert!(read.text.contains("two"), "{}", read.text);
    assert!(
        !read.text.contains("one\n") && !read.text.contains("three"),
        "{}",
        read.text
    );
    assert_eq!(read.next_line, Some(3));
    assert!(read.summary().contains("line 3"), "{}", read.summary());
    assert!(read.next_offset.is_none());
    assert!(f.permissions().is_empty(), "a read asks for nothing");
}
