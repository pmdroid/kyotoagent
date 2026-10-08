//! The session log projected to the quiet cards.
//!
//! Everything here goes through the public surface: a session directory is
//! built with [`kyotoagent::session`], events are appended one at a time, and
//! [`kyotoagent::view`] reads the result back. No frame is drawn, because the point
//! of the projection is the JSON a screen would be handed.

use std::fs;
use std::path::{Path, PathBuf};

use kyotoagent::events::{
    AskBody, CompactBody, Event, EventKind, ModelMessageBody, PermissionAnswerBody, PermissionBody,
    ProofBody, ProofFailure, ProofItem, QuestionBody, ResultBody, TaskDoneBody, TaskStartBody,
    TodosBody, ToolCallBody, ToolResultBody,
};
use kyotoagent::screen::Status;
use kyotoagent::session::{Session, SessionMeta};
use kyotoagent::view::{self, CardKind};
use serde_json::Value;

const AT: &str = "2026-09-29T00:00:00.000Z";
const WORKSPACE: &str = "/w";

/// A directory of this test's own, named after the test so two runs cannot
/// tread on each other.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kyotoagent-view-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn event(id: &str, kind: EventKind, body: Value) -> Event {
    Event::new(id, AT, "t1", kind)
        .with_body(&body)
        .expect("a body serializes")
}

/// The turn from the issue: an ask, a quiet read, a write that had to be
/// allowed, a result, and the proof. A `tool_call` and its `tool_result` sit in
/// the middle, where the reader of the log will see them and the reader of the
/// screen will not.
fn fixture_turn(session: &Session) {
    let events = vec![
        event(
            "e1",
            EventKind::UserAsk,
            serde_json::to_value(AskBody {
                images: Vec::new(),
                text: "Add a readme line that names the binary.".into(),
                context: String::new(),
                skill: String::new(),
                silent: false,
            })
            .expect("an ask body"),
        ),
        event(
            "e2",
            EventKind::ModelMessage,
            serde_json::to_value(ModelMessageBody {
                text: "Reading Cargo.toml first.".into(),
            })
            .expect("a message body"),
        ),
        event(
            "e3",
            EventKind::ToolCall,
            serde_json::to_value(ToolCallBody {
                tool: "read_file".into(),
                args: serde_json::json!({ "path": "Cargo.toml" }),
            })
            .expect("a tool call body"),
        ),
        event(
            "e4",
            EventKind::ToolResult,
            serde_json::to_value(ToolResultBody {
                is_error: false,
                images: Vec::new(),
                tool: "read_file".into(),
                output: "name = \"kyotoagent\"".into(),
            })
            .expect("a tool result body"),
        ),
        event(
            "e5",
            EventKind::Permission,
            serde_json::to_value(PermissionBody::write(
                "Replace README.md",
                "/w/README.md",
                &["@@ -1 +1,2 @@", "+kyotoagent is the binary."],
            ))
            .expect("a permission body"),
        ),
    ];
    for event in events {
        session.append(&event).expect("the event is appended");
    }
}

fn proof_event() -> Event {
    event(
        "e8",
        EventKind::Proof,
        serde_json::to_value(ProofBody {
            files: Vec::new(),
            text: "cargo test passed.".into(),
            wrote: vec!["README.md".into()],
            status: "failed".into(),
            head: String::new(),
            workspace_fingerprint: String::new(),
            diff_stat: "README.md | 1 +".into(),
            note: "The readme names the binary; the lint item did not pass.".into(),
            failures: vec![ProofFailure {
                argv: vec!["cargo".into(), "clippy".into()],
                exit: 1,
                tail: "warning: unused variable".into(),
            }],
            items: vec![
                ProofItem {
                    id: "test".into(),
                    kind: "command".into(),
                    outcome: "passed".into(),
                    argv: Vec::new(),
                    exit: None,
                    tail: String::new(),
                },
                ProofItem {
                    id: "lint".into(),
                    kind: "command".into(),
                    outcome: "failed".into(),
                    argv: vec!["cargo".into(), "clippy".into()],
                    exit: Some(1),
                    tail: "warning: unused variable".into(),
                },
            ],
        })
        .expect("a proof body"),
    )
}

fn allow(eid: &str) -> Event {
    event(
        "e6",
        EventKind::PermissionAnswer,
        serde_json::to_value(PermissionAnswerBody {
            permission_id: Some(eid.into()),
            decision: kyotoagent::events::Decision::AllowOnce,
        })
        .expect("an answer body"),
    )
}

fn deny(eid: &str) -> Event {
    event(
        "e6",
        EventKind::PermissionAnswer,
        serde_json::to_value(PermissionAnswerBody {
            permission_id: Some(eid.into()),
            decision: kyotoagent::events::Decision::Deny,
        })
        .expect("an answer body"),
    )
}

fn finished_session(dir: &Path) -> Session {
    let session = Session::at(dir);
    session
        .create(&SessionMeta::new(
            "91bc7a1d",
            Path::new(WORKSPACE),
            "openai/gpt-4o",
            AT,
        ))
        .expect("the session is created");
    fixture_turn(&session);
    session
        .append(&allow("e5"))
        .expect("the answer is appended");
    session
        .append(&event(
            "e7",
            EventKind::Result,
            serde_json::to_value(ResultBody {
                text: "The readme now names the binary.".into(),
                note: String::new(),
            })
            .expect("a result body"),
        ))
        .expect("the result is appended");
    session
        .append(&proof_event())
        .expect("the proof is appended");
    session
}

/// The whole turn reads as three cards: ask, result, proof. The read is not
/// one of them, and the answered permission is not either.
#[test]
fn a_finished_turn_reads_as_three_cards() {
    let dir = temp_dir("three-cards");
    finished_session(&dir);

    let view = view::read(&dir).expect("the session reads");

    assert_eq!(view.status, Status::Idle, "the status is the meta's");
    assert_eq!(view.revision, 8, "every event counts, quiet ones too");
    let kinds: Vec<CardKind> = view.cards.iter().map(|card| card.kind).collect();
    assert_eq!(
        kinds,
        vec![CardKind::Ask, CardKind::Result, CardKind::Proof]
    );
    assert_eq!(
        view.cards
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>(),
        vec!["c1", "c2", "c3"]
    );
    assert_eq!(
        view.cards[0].body["text"],
        Value::from("Add a readme line that names the binary.")
    );
    assert_eq!(
        view.cards[1].body["text"],
        Value::from("The readme now names the binary.")
    );
    assert_eq!(
        view.cards[2].body["text"],
        Value::from("cargo test passed.")
    );
    assert!(view.cards[2].body.get("status").is_none());
    assert!(view.cards[2].body.get("diffStat").is_none());
    assert!(view.cards[2].body.get("wrote").is_none());
    assert!(view.cards[2].body.get("failures").is_none());
    let items = &view.cards[2].body["items"];
    assert_eq!(items[0]["id"], Value::from("test"));
    assert_eq!(items[0]["kind"], Value::from("command"));
    assert_eq!(items[0]["outcome"], Value::from("passed"));
    assert_eq!(items[1]["id"], Value::from("lint"));
    assert_eq!(items[1]["kind"], Value::from("command"));
    assert_eq!(items[1]["outcome"], Value::from("failed"));
    assert_eq!(items[1]["argv"][0], Value::from("cargo"));
    assert_eq!(items[1]["exit"], Value::from(1));
    assert_eq!(items[1]["tail"], Value::from("warning: unused variable"));

    let log = fs::read_to_string(Session::at(&dir).events_path()).expect("the log reads");
    assert!(
        log.contains("\"kind\":\"permission\""),
        "the log kept the permission"
    );
    assert!(
        log.contains("\"decision\":\"allow_once\""),
        "and the answer"
    );

    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_silent_ask_leaves_no_card() {
    let dir = temp_dir("silent-ask");
    let session = Session::at(&dir);
    session
        .create(&SessionMeta::new("91bc", Path::new(WORKSPACE), "gpt", AT))
        .expect("the session is created");
    session
        .append(&event(
            "e1",
            EventKind::UserAsk,
            serde_json::to_value(AskBody {
                images: Vec::new(),
                text: "Add a readme line.".into(),
                context: String::new(),
                skill: String::new(),
                silent: false,
            })
            .expect("an ask body"),
        ))
        .expect("the ask is appended");
    session
        .append(&event(
            "e2",
            EventKind::UserAsk,
            serde_json::to_value(AskBody {
                images: Vec::new(),
                text: "Subagent finished.".into(),
                context: "result: found 3 files\nproof: grep exit 0\nresume_from=\"b5469db3\"\n"
                    .into(),
                skill: String::new(),
                silent: true,
            })
            .expect("a silent ask body"),
        ))
        .expect("the wake is appended");
    session
        .append(&event(
            "e3",
            EventKind::Result,
            serde_json::to_value(ResultBody {
                text: "The readme now names the binary.".into(),
                note: String::new(),
            })
            .expect("a result body"),
        ))
        .expect("the result is appended");

    let view = view::read(&dir).expect("the session reads");
    let kinds: Vec<CardKind> = view.cards.iter().map(|card| card.kind).collect();
    assert_eq!(kinds, vec![CardKind::Ask, CardKind::Result]);
    let cards = serde_json::to_string(&view.cards).expect("cards serialize");
    assert!(!cards.contains("Subagent"), "{cards}");
    assert!(!cards.contains("b5469db3"), "{cards}");
    assert_eq!(
        view.cards[0].body["text"],
        Value::from("Add a readme line.")
    );
    let log = fs::read_to_string(session.events_path()).expect("the log reads");
    assert!(log.contains("Subagent finished."));
    assert!(log.contains("resume_from=\\\"b5469db3\\\""));
    assert!(log.contains("found 3 files"));
    assert!(log.contains("grep exit 0"));

    fs::remove_dir_all(&dir).expect("clean up");
}

/// The read happened, the log knows, and no card does.
#[test]
fn the_read_leaves_no_trace_in_the_cards() {
    let dir = temp_dir("quiet-read");
    let session = finished_session(&dir);

    let cards = serde_json::to_string(&view::read(&dir).expect("the session reads").cards)
        .expect("cards serialize");
    assert!(
        !cards.contains("read_file"),
        "no card names the tool: {cards}"
    );
    assert!(
        !cards.contains("Cargo.toml"),
        "no card names the path: {cards}"
    );

    // The log is the other half of the promise: the read is in it.
    let log = fs::read_to_string(session.events_path()).expect("the log reads");
    assert!(
        log.contains("\"kind\":\"tool_call\""),
        "the log has the call"
    );
    assert!(log.contains("read_file"), "and the tool name");

    fs::remove_dir_all(&dir).expect("clean up");
}

/// While the answer is open the card still carries the change it is asking
/// about, and a command carries its argv. Once the answer is in, the card
/// leaves the list, and the log still has both.
#[test]
fn a_waiting_permission_stays_until_it_is_answered() {
    let dir = temp_dir("collapse");
    let session = finished_session(&dir);

    let up_to_the_permission = &session.events().expect("the log reads")[..5];
    let waiting = view::cards(up_to_the_permission);
    let permission = &waiting[1];
    assert_eq!(permission.kind, CardKind::Permission);
    assert_eq!(permission.body["action"], Value::from("Replace README.md"));
    assert_eq!(
        permission.body["diff"][1],
        Value::from("+kyotoagent is the binary.")
    );
    assert!(permission.body["decision"].is_null(), "no decision yet");
    assert_eq!(permission.body["eventId"], Value::from("e5"));

    let mut meta = session.meta().expect("meta reads");
    meta.status = Status::Waiting;
    session.write_meta(&meta).expect("the meta is written");
    assert_eq!(
        view::read(&dir).expect("the session reads").status,
        Status::Waiting
    );

    let after = view::read(&dir).expect("the session reads");
    assert!(
        after
            .cards
            .iter()
            .all(|card| card.kind != CardKind::Permission),
        "the answered permission left: {:?}",
        after.cards.iter().map(|card| card.kind).collect::<Vec<_>>()
    );
    let json = serde_json::to_string(&after.cards).expect("cards serialize");
    assert!(!json.contains("@@"), "no hunk survives in the view: {json}");

    let log = fs::read_to_string(session.events_path()).expect("the log reads");
    assert!(log.contains("@@ -1 +1,2 @@"), "the log kept the diff");
    assert!(
        log.contains("\"decision\":\"allow_once\""),
        "and the answer"
    );

    fs::remove_dir_all(&dir).expect("clean up");
}

/// A command permission asks with its argv and leaves once it is answered.
#[test]
fn a_command_permission_leaves_when_it_is_answered() {
    let dir = temp_dir("command");
    let session = Session::at(&dir);
    session
        .create(&SessionMeta::new(
            "3f2ae04c",
            Path::new(WORKSPACE),
            "qwen",
            AT,
        ))
        .expect("the session is created");
    session
        .append(&event(
            "e1",
            EventKind::Permission,
            serde_json::to_value(PermissionBody::run(
                "Run cargo test",
                &["cargo", "test"],
                Some(600),
            ))
            .expect("a permission body"),
        ))
        .expect("the permission is appended");

    let open = view::read(&dir).expect("the session reads");
    assert_eq!(open.cards[0].body["argv"][1], Value::from("test"));
    assert_eq!(open.cards[0].body["timeoutSec"], Value::from(600));
    assert!(
        open.cards[0].body["diff"].is_null(),
        "a command has no diff to ask about"
    );

    session
        .append(&allow("e1"))
        .expect("the answer is appended");
    let settled = view::read(&dir).expect("the session reads");
    assert!(
        settled.cards.is_empty(),
        "the answered command left the list"
    );
    let log = fs::read_to_string(session.events_path()).expect("the log reads");
    assert!(log.contains("\"cargo\""), "the log kept the argv");
    assert!(
        log.contains("\"decision\":\"allow_once\""),
        "and the answer"
    );

    fs::remove_dir_all(&dir).expect("clean up");
}

/// Every kind the log knows survives being appended and read back, with its
/// body byte for byte.
#[test]
fn every_kind_round_trips_through_the_log() {
    let dir = temp_dir("round-trip");
    let session = Session::at(&dir);
    session
        .create(&SessionMeta::new(
            "ab1029f6",
            Path::new(WORKSPACE),
            "gpt",
            AT,
        ))
        .expect("the session is created");

    let kinds = [
        EventKind::UserAsk,
        EventKind::ModelMessage,
        EventKind::ToolCall,
        EventKind::ToolResult,
        EventKind::Question,
        EventKind::QuestionAnswer,
        EventKind::Permission,
        EventKind::PermissionAnswer,
        EventKind::Result,
        EventKind::Proof,
        EventKind::Compact,
        EventKind::Todos,
        EventKind::CloseoutRun,
        EventKind::TaskStart,
        EventKind::TaskDone,
        EventKind::Schedule,
        EventKind::ScheduleCancel,
        EventKind::EnhanceRequest,
        EventKind::Enhance,
        EventKind::EnhanceAnswer,
    ];
    let written: Vec<Event> = kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            let id = format!("e{}", index + 1);
            let body = match kind {
                EventKind::UserAsk => serde_json::to_value(AskBody {
                    images: Vec::new(),
                    text: "ask".into(),
                    context: String::new(),
                    skill: String::new(),
                    silent: false,
                }),
                EventKind::ModelMessage => serde_json::to_value(ModelMessageBody {
                    text: "thinking".into(),
                }),
                EventKind::ToolCall => serde_json::to_value(ToolCallBody {
                    tool: "list_dir".into(),
                    args: serde_json::json!({ "path": "." }),
                }),
                EventKind::ToolResult => serde_json::to_value(ToolResultBody {
                    is_error: false,
                    images: Vec::new(),
                    tool: "list_dir".into(),
                    output: "README.md".into(),
                }),
                EventKind::Question => serde_json::to_value(QuestionBody {
                    text: "Which title?".into(),
                    choices: vec!["Kyoto Agent".into()],
                    visuals: Vec::new(),
                }),
                EventKind::QuestionAnswer => {
                    serde_json::to_value(kyotoagent::events::QuestionAnswerBody {
                        question_id: Some("e5".into()),
                        answer: "Kyoto Agent".into(),
                    })
                }
                EventKind::Permission => serde_json::to_value(PermissionBody::write(
                    "Create notes.md",
                    "/w/notes.md",
                    &["+one"],
                )),
                EventKind::PermissionAnswer => serde_json::to_value(PermissionAnswerBody {
                    permission_id: Some("e7".into()),
                    decision: kyotoagent::events::Decision::Deny,
                }),
                EventKind::Result => serde_json::to_value(ResultBody {
                    text: "done".into(),
                    note: String::new(),
                }),
                EventKind::Proof => serde_json::to_value(ProofBody {
                    text: "cargo test passed.".into(),
                    ..ProofBody::default()
                }),
                EventKind::Artifact => Ok(serde_json::json!({})),
                EventKind::Compact => serde_json::to_value(CompactBody {
                    summary: "files and decisions".into(),
                    through_event_id: "e1".into(),
                }),
                EventKind::Todos => serde_json::to_value(TodosBody {
                    items: vec![kyotoagent::events::TodoItem {
                        id: "read".into(),
                        title: "Read the crate".into(),
                        status: kyotoagent::events::TodoStatus::Pending,
                        description: None,
                        files: Vec::new(),
                        links: Vec::new(),
                    }],
                }),
                EventKind::CloseoutRun => {
                    serde_json::to_value(kyotoagent::events::CloseoutRunBody {
                        passed: None,
                        transcript: None,
                        argv: Vec::new(),
                        timed_out: false,
                        truncated: false,
                        id: "test".into(),
                        attempt: 1,
                        exit: 0,
                        tail: "test result: ok".into(),
                        workspace_fingerprint: String::new(),
                        policy_digest: String::new(),
                    })
                }
                EventKind::TaskStart => serde_json::to_value(TaskStartBody {
                    id: "ab12cd34".into(),
                    argv: vec!["sleep".into(), "30".into()],
                }),
                EventKind::TaskDone => serde_json::to_value(TaskDoneBody {
                    id: "ab12cd34".into(),
                    argv: vec!["sleep".into(), "30".into()],
                    exit: 0,
                    tail: String::new(),
                    state: kyotoagent::events::TaskStatus::Exited,
                }),
                EventKind::Schedule => serde_json::to_value(kyotoagent::events::ScheduleBody {
                    id: "cd34ef56".into(),
                    note: "Check gh comments".into(),
                    due_at: "2026-09-29T00:10:00.000Z".into(),
                }),
                EventKind::ScheduleCancel => {
                    serde_json::to_value(kyotoagent::events::ScheduleCancelBody {
                        id: "cd34ef56".into(),
                    })
                }
                EventKind::EnhanceRequest => {
                    serde_json::to_value(kyotoagent::events::EnhanceRequestBody {
                        text: "ship it".into(),
                        model: "m".into(),
                    })
                }
                EventKind::Enhance => serde_json::to_value(kyotoagent::events::EnhanceBody {
                    text: "Draft.".into(),
                    source: "ship it".into(),
                    model: "m".into(),
                    error: None,
                }),
                EventKind::EnhanceAnswer => {
                    serde_json::to_value(kyotoagent::events::EnhanceAnswerBody {
                        enhance_id: "e19".into(),
                        choice: kyotoagent::events::EnhanceChoice::Discard,
                        text: None,
                    })
                }
                EventKind::CloseoutChanged
                | EventKind::CloseoutStarted
                | EventKind::CloseoutOutput => Ok(serde_json::json!({})),
            }
            .expect("a body serializes");
            event(&id, *kind, body)
        })
        .collect();

    for event in &written {
        session.append(event).expect("the event is appended");
    }
    let read_back = session.events().expect("the log reads");

    assert_eq!(read_back, written, "every event came back as it went in");
    assert_eq!(
        read_back.iter().map(|e| e.kind).collect::<Vec<_>>(),
        kinds.to_vec()
    );
    let cards = view::read(&dir).expect("the session reads").cards;
    assert_eq!(
        cards.iter().map(|card| card.kind).collect::<Vec<_>>(),
        vec![
            CardKind::Ask,
            CardKind::Question,
            CardKind::Answer,
            CardKind::Result,
            CardKind::Proof
        ]
    );
    let question = cards
        .iter()
        .find(|card| card.kind == CardKind::Question)
        .expect("the question stays");
    assert_eq!(
        question.body["answer"],
        serde_json::Value::from("Kyoto Agent")
    );
    assert!(question.body["choices"]
        .as_array()
        .expect("choices")
        .is_empty());
    let answer = cards
        .iter()
        .find(|card| card.kind == CardKind::Answer)
        .expect("the answer card");
    assert_eq!(answer.body["text"], serde_json::Value::from("Kyoto Agent"));

    fs::remove_dir_all(&dir).expect("clean up");
}

/// A deny drops the permission the same way an allow does.
#[test]
fn a_denied_permission_leaves_the_card_list() {
    let dir = temp_dir("deny");
    let session = Session::at(&dir);
    session
        .create(&SessionMeta::new(
            "91bc7a1d",
            Path::new(WORKSPACE),
            "gpt",
            AT,
        ))
        .expect("the session is created");
    session
        .append(&event(
            "e1",
            EventKind::Permission,
            serde_json::to_value(PermissionBody::write(
                "Replace README.md",
                "/w/README.md",
                &["@@ -1 +1,2 @@", "+kyotoagent is the binary."],
            ))
            .expect("a permission body"),
        ))
        .expect("the permission is appended");
    session.append(&deny("e1")).expect("the deny is appended");

    let view = view::read(&dir).expect("the session reads");
    assert!(view
        .cards
        .iter()
        .all(|card| card.kind != CardKind::Permission));
    assert!(view.cards.is_empty());
    let log = fs::read_to_string(session.events_path()).expect("the log reads");
    assert!(log.contains("\"kind\":\"permission\""), "{log}");
    assert!(log.contains("\"decision\":\"deny\""), "{log}");

    fs::remove_dir_all(&dir).expect("clean up");
}

/// A log with nothing in it is a session with nothing on it, not an error.
#[test]
fn an_empty_log_projects_to_no_cards() {
    let dir = temp_dir("empty");
    Session::at(&dir)
        .create(&SessionMeta::new("91bc", Path::new(WORKSPACE), "gpt", AT))
        .expect("the session is created");

    let view = view::read(&dir).expect("the session reads");
    assert!(view.cards.is_empty());
    assert_eq!(view.revision, 0);
    assert_eq!(view.status, Status::Idle);

    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn cached_events_follow_external_append_replacement_and_truncation() {
    let dir = temp_dir("external-log-edits");
    let session = Session::at(&dir);
    session
        .create(&SessionMeta::new("s", Path::new(WORKSPACE), "m", AT))
        .unwrap();
    let first = event("e1", EventKind::Result, serde_json::json!({"text":"first"}));
    session.append(&first).unwrap();
    assert_eq!(
        view::project(&session).unwrap().cards[0].body["text"],
        "first"
    );
    let other = Session::at(&dir);
    other
        .append(&event(
            "e2",
            EventKind::Result,
            serde_json::json!({"text":"second"}),
        ))
        .unwrap();
    assert_eq!(view::project(&session).unwrap().cards.len(), 2);
    let replacement = event("e1", EventKind::Result, serde_json::json!({"text":"third"}));
    let line = format!("{}\n", serde_json::to_string(&replacement).unwrap());
    fs::write(session.events_path(), &line).unwrap();
    assert_eq!(
        view::project(&session).unwrap().cards[0].body["text"],
        "third"
    );
    let replacement = event("e1", EventKind::Result, serde_json::json!({"text":"forth"}));
    let next = format!("{}\n", serde_json::to_string(&replacement).unwrap());
    assert_eq!(line.len(), next.len());
    let temp = dir.join("replacement.jsonl");
    fs::write(&temp, next).unwrap();
    fs::rename(temp, session.events_path()).unwrap();
    assert_eq!(
        view::project(&session).unwrap().cards[0].body["text"],
        "forth"
    );
    fs::write(session.events_path(), "").unwrap();
    assert!(view::project(&session).unwrap().cards.is_empty());
    assert_eq!(session.next_event_id().unwrap(), "e1");
    fs::remove_dir_all(dir).unwrap();
}
