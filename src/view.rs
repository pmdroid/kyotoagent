//! What a session log looks like as cards.
//!
//! This is the projection the whole project is named for: the log holds every
//! event the model produced, and the view keeps only the ones a person is
//! waiting on. A model message, a tool call, and a tool result project to
//! nothing at all, so a file the agent read quietly leaves no mark, and neither
//! the tool's name nor its arguments reach the card list.
//!
//! A [`EventKind::QuestionAnswer`] drops that question from the card list. A
//! [`EventKind::PermissionAnswer`] drops that permission from the card list,
//! so a yolo turn does not stack Allowed lines in the pane. The log keeps the
//! permission and the answer. A permission whose `decision` is still null stays,
//! which is the waiting write or command.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::compact::ContextUsage;
use crate::events::{
    parse_millis, AskBody, CloseoutRunBody, EnhanceAnswerBody, EnhanceBody, EnhanceChoice, Event,
    EventKind, PermissionAnswerBody, PermissionBody, ProofBody, QuestionAnswerBody, QuestionBody,
    ResultBody, ScheduleBody, ScheduleCancelBody, TaskDoneBody, TaskStartBody, TaskStatus,
    TodoItem, TodosBody, ToolCallBody, ToolResultBody,
};
use crate::prompt::SkillEntry;
use crate::schedule::remaining_minutes;
use crate::screen::{Phase, Status};
use crate::session::{AllowList, Session, SessionError, SessionMeta};

/// The kind of card, named as the screen names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardKind {
    /// What the user asked.
    Ask,
    /// A question the agent asked that is still open.
    Question,
    Answer,
    /// A write or a command that is still waiting.
    Permission,
    /// What the agent finished with.
    Result,
    /// What the turn changed and which checks it ran.
    Proof,
    Artifact,
    Enhance,
}

impl CardKind {
    pub fn label(self) -> &'static str {
        match self {
            CardKind::Ask => "ask",
            CardKind::Question => "question",
            CardKind::Answer => "answer",
            CardKind::Permission => "permission",
            CardKind::Result => "result",
            CardKind::Proof => "proof",
            CardKind::Artifact => "artifact",
            CardKind::Enhance => "enhance",
        }
    }
}

/// One card in the right pane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Card {
    /// `c1`, `c2`, and so on, in the order the cards appear.
    pub id: String,
    pub kind: CardKind,
    /// When the event that opened the card was appended.
    pub at: String,
    /// The kind's own fields. See the module docs for each shape.
    pub body: Value,
}

/// A session as the screen reads it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    /// From `meta.json`, not from the log: the log says what happened and the
    /// meta says where the session got to.
    pub status: Status,
    pub cards: Vec<Card>,
    /// How many events the log holds. The screen redraws when this changes,
    /// so a tool call that drew nothing still moves it.
    pub revision: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<TodoItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<ScheduleItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<String>,
    #[serde(default, rename = "queueItems", skip_serializing_if = "Vec::is_empty")]
    pub queue_items: Vec<QueuedMessage>,
    #[serde(default)]
    pub allow: AllowList,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closeout: Vec<CloseoutRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<crate::goal::Goal>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessage {
    pub id: String,
    pub text: String,
    pub image_count: usize,
    pub enhance: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseoutStatus {
    Missing,
    Running,
    Passed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CloseoutRow {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<CloseoutRunBody>,
    pub id: String,
    pub kind: String,
    pub hint: String,
    pub status: CloseoutStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tail: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduleItem {
    pub id: String,
    pub note: String,
    #[serde(rename = "dueAt")]
    pub due_at: String,
    #[serde(rename = "remainingMin")]
    pub remaining_min: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskItem {
    pub id: String,
    pub argv: Vec<String>,
    pub state: TaskStatus,
}

/// Read a session directory and project it to cards.
pub fn read(dir: &Path) -> Result<View, SessionError> {
    project(&Session::at(dir))
}

/// Project a session to cards: its status, its cards, and how many events it
/// has.
pub fn project(session: &Session) -> Result<View, SessionError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0);
    project_at(session, now)
}

pub fn project_at(session: &Session, now_millis: i64) -> Result<View, SessionError> {
    let meta = session.meta()?;
    let (_, events) = session.event_snapshot()?;
    Ok(project_events(meta, &events, now_millis))
}

pub(crate) fn project_events(meta: SessionMeta, events: &[Event], now_millis: i64) -> View {
    View {
        status: meta.status,
        cards: cards(events),
        revision: events.len(),
        skills: Vec::new(),
        todos: latest_todos(events),
        tasks: running_tasks(events),
        schedules: pending_schedules(events, now_millis),
        phase: None,
        action: tool_error_action(events),
        thinking: None,
        retry_status: None,
        queue: Vec::new(),
        queue_items: Vec::new(),
        allow: meta.allow,
        closeout: closeout_rows(Path::new(&meta.workspace), events, meta.show_closeout),
        context: None,
        goal: meta.goal,
    }
}

fn tool_error_action(events: &[Event]) -> Option<String> {
    let mut failure: Option<(String, String, usize)> = None;
    let mut batch_failed = false;
    for event in events {
        match event.kind {
            EventKind::UserAsk | EventKind::Result => {
                failure = None;
                batch_failed = false;
            }
            EventKind::ModelMessage => batch_failed = false,
            EventKind::ToolResult => {
                let Ok(body) = event.body_as::<ToolResultBody>() else {
                    continue;
                };
                if !body.is_error {
                    if !batch_failed {
                        failure = None;
                    }
                    continue;
                }
                batch_failed = true;
                let count = failure
                    .as_ref()
                    .filter(|(tool, output, _)| tool == &body.tool && output == &body.output)
                    .map_or(1, |(_, _, count)| count + 1);
                failure = Some((body.tool, body.output, count));
            }
            _ => {}
        }
    }
    failure.map(|(tool, output, count)| format!("{tool} failed ({count} attempts): {output}"))
}

pub fn closeout_rows(workspace: &Path, events: &[Event], show: bool) -> Vec<CloseoutRow> {
    if !show {
        return Vec::new();
    }
    let file = match crate::closeout::read(workspace) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let Some(file) = file else {
        return Vec::new();
    };
    if file.items.is_empty() {
        return Vec::new();
    }
    let mut latest: std::collections::HashMap<String, CloseoutRunBody> =
        std::collections::HashMap::new();
    let mut inflight: Option<String> = None;
    for event in events {
        match event.kind {
            EventKind::ToolCall => {
                let Ok(body) = event.body_as::<ToolCallBody>() else {
                    continue;
                };
                if body.tool != "run_closeout" {
                    continue;
                }
                let Some(id) = body.args.get("id").and_then(Value::as_str) else {
                    continue;
                };
                inflight = Some(id.to_string());
            }
            EventKind::CloseoutRun => {
                let Ok(body) = event.body_as::<CloseoutRunBody>() else {
                    continue;
                };
                if inflight.as_deref() == Some(body.id.as_str()) {
                    inflight = None;
                }
                latest.insert(body.id.clone(), body);
            }
            EventKind::ToolResult => {
                let Ok(body) = event.body_as::<ToolResultBody>() else {
                    continue;
                };
                if body.tool == "run_closeout" {
                    inflight = None;
                }
            }
            _ => {}
        }
    }
    file.items
        .into_iter()
        .map(|item| {
            let run = latest.get(&item.id);
            let running = inflight.as_deref() == Some(item.id.as_str());
            let status = if running {
                CloseoutStatus::Running
            } else if let Some(run) = run {
                if run.exit == 0 && !run.timed_out {
                    CloseoutStatus::Passed
                } else {
                    CloseoutStatus::Failed
                }
            } else {
                CloseoutStatus::Missing
            };
            CloseoutRow {
                runs: events
                    .iter()
                    .filter(|event| event.kind == EventKind::CloseoutRun)
                    .filter_map(|event| event.body_as::<CloseoutRunBody>().ok())
                    .filter(|run| run.id == item.id)
                    .collect(),
                id: item.id,
                kind: item.kind.label().to_string(),
                hint: item.hint,
                status,
                exit: run.map(|run| run.exit),
                attempt: run.map(|run| run.attempt),
                tail: run.map(|run| run.tail.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

pub(crate) fn pending_schedules(events: &[Event], now_millis: i64) -> Vec<ScheduleItem> {
    let mut open: Vec<ScheduleItem> = Vec::new();
    for event in events {
        match event.kind {
            EventKind::Schedule => {
                let Ok(body) = event.body_as::<ScheduleBody>() else {
                    continue;
                };
                let due = parse_millis(&body.due_at).unwrap_or(0);
                let item = ScheduleItem {
                    id: body.id.clone(),
                    note: body.note,
                    due_at: body.due_at,
                    remaining_min: remaining_minutes(due, now_millis),
                };
                if let Some(existing) = open.iter_mut().find(|item| item.id == body.id) {
                    *existing = item;
                } else {
                    open.push(item);
                }
            }
            EventKind::ScheduleCancel => {
                let Ok(body) = event.body_as::<ScheduleCancelBody>() else {
                    continue;
                };
                open.retain(|item| item.id != body.id);
            }
            _ => {}
        }
    }
    open.sort_by(|left, right| left.due_at.cmp(&right.due_at).then(left.id.cmp(&right.id)));
    open
}

fn running_tasks(events: &[Event]) -> Vec<TaskItem> {
    let mut open: Vec<TaskItem> = Vec::new();
    for event in events {
        match event.kind {
            EventKind::TaskStart => {
                let Ok(body) = event.body_as::<TaskStartBody>() else {
                    continue;
                };
                if let Some(existing) = open.iter_mut().find(|item| item.id == body.id) {
                    existing.argv = body.argv;
                    existing.state = TaskStatus::Running;
                } else {
                    open.push(TaskItem {
                        id: body.id,
                        argv: body.argv,
                        state: TaskStatus::Running,
                    });
                }
            }
            EventKind::TaskDone => {
                let Ok(body) = event.body_as::<TaskDoneBody>() else {
                    continue;
                };
                open.retain(|item| item.id != body.id);
            }
            _ => {}
        }
    }
    open
}

fn latest_todos(events: &[Event]) -> Vec<TodoItem> {
    events
        .iter()
        .rev()
        .find(|event| event.kind == EventKind::Todos)
        .and_then(|event| event.body_as::<TodosBody>().ok())
        .map(|body| body.items)
        .unwrap_or_default()
}

/// The cards a log projects to, in order.
pub fn cards(events: &[Event]) -> Vec<Card> {
    let mut open = opened(events);
    settle(events, &mut open);
    open.into_iter()
        .enumerate()
        .map(|(index, mut opened)| {
            opened.card.id = format!("c{}", index + 1);
            opened.card
        })
        .collect()
}

/// A card, with the id of the event that opened it. The card's own id is `c1`,
/// `c2`, and so on, so an answer in the log cannot name it; it names the event,
/// and this is the link.
struct Opened {
    event_id: String,
    card: Card,
}

/// One card per event that opens one, before any answer is applied.
fn opened(events: &[Event]) -> Vec<Opened> {
    let mut out: Vec<Opened> = Vec::new();
    for event in events {
        if !event.kind.is_card() {
            continue;
        }
        let body = match event.kind {
            EventKind::UserAsk => ask_body(event),
            EventKind::Question => question_body(event),
            EventKind::QuestionAnswer => answer_body(event),
            EventKind::Permission => permission_body(event),
            EventKind::Result => result_body(event),
            EventKind::Proof => proof_body(event),
            EventKind::Artifact => event
                .body_as::<crate::events::ArtifactBody>()
                .ok()
                .and_then(|body| serde_json::to_value(body).ok()),
            EventKind::Enhance => enhance_body(event),
            _ => None,
        };
        if event.kind == EventKind::Enhance {
            out.retain(|open| open.card.kind != CardKind::Enhance);
        }
        if let Some(mut body) = body {
            if matches!(event.kind, EventKind::Result | EventKind::Artifact) {
                body["eventId"] = Value::String(event.id.clone());
                body["turnId"] = Value::String(event.turn_id.clone());
            }
            out.push(Opened {
                event_id: event.id.clone(),
                card: Card {
                    id: format!("c{}", out.len() + 1),
                    kind: card_kind(event.kind),
                    at: event.at.clone(),
                    body,
                },
            });
        }
    }
    out
}

/// A question's `question_answer` drops that card from the list. A permission's
/// `permission_answer` drops that card from the list.
///
/// An answer names the event it belongs to. When it does not, it belongs to the
/// newest card of that kind that is still open, which is the only card a user
/// could have been looking at when they answered.
fn settle(events: &[Event], cards: &mut Vec<Opened>) {
    for event in events {
        match event.kind {
            EventKind::QuestionAnswer => {
                let Ok(answer) = event.body_as::<QuestionAnswerBody>() else {
                    continue;
                };
                let Some(index) = latest(cards, CardKind::Question, &answer.question_id) else {
                    continue;
                };
                if let Some(body) = cards[index].card.body.as_object_mut() {
                    body.insert("choices".to_string(), Value::Array(Vec::new()));
                    body.insert("answer".to_string(), Value::String(answer.answer));
                }
            }
            EventKind::PermissionAnswer => {
                let Ok(answer) = event.body_as::<PermissionAnswerBody>() else {
                    continue;
                };
                let Some(index) = latest(cards, CardKind::Permission, &answer.permission_id) else {
                    continue;
                };
                cards.remove(index);
            }
            EventKind::EnhanceAnswer => {
                let Ok(answer) = event.body_as::<EnhanceAnswerBody>() else {
                    continue;
                };
                if answer.choice == EnhanceChoice::Retry {
                    continue;
                }
                let Some(index) = cards.iter().position(|open| {
                    open.card.kind == CardKind::Enhance && open.event_id == answer.enhance_id
                }) else {
                    continue;
                };
                cards.remove(index);
            }
            _ => {}
        }
    }
}

/// The card an answer settles: the one it names, or else the newest card of
/// that kind that is still open.
fn latest(cards: &[Opened], kind: CardKind, named: &Option<String>) -> Option<usize> {
    if let Some(named) = named {
        return cards
            .iter()
            .position(|open| open.card.kind == kind && open.event_id == *named);
    }
    // A turn holds one open question or permission at a time, because it stops
    // and waits there, so the newest unsettled card of the kind is the one the
    // user was answering.
    cards
        .iter()
        .rposition(|open| open.card.kind == kind && !is_settled(&open.card, kind))
}

/// Whether a card already carries an answer.
fn is_settled(card: &Card, kind: CardKind) -> bool {
    let key = match kind {
        CardKind::Question => "answer",
        _ => "decision",
    };
    card.body
        .get(key)
        .map(|value| !value.is_null())
        .unwrap_or(false)
}

fn card_kind(kind: EventKind) -> CardKind {
    match kind {
        EventKind::UserAsk => CardKind::Ask,
        EventKind::Question => CardKind::Question,
        EventKind::QuestionAnswer => CardKind::Answer,
        EventKind::Permission => CardKind::Permission,
        EventKind::Result => CardKind::Result,
        EventKind::Enhance => CardKind::Enhance,
        EventKind::Artifact => CardKind::Artifact,
        _ => CardKind::Proof,
    }
}

fn answer_body(event: &Event) -> Option<Value> {
    let answer: QuestionAnswerBody = event.body_as().ok()?;
    Some(serde_json::json!({ "text": answer.answer }))
}

fn enhance_body(event: &Event) -> Option<Value> {
    let body: EnhanceBody = event.body_as().ok()?;
    let mut value = serde_json::json!({
        "text": body.text,
        "source": body.source,
        "model": body.model,
        "eventId": event.id,
    });
    if let Some(error) = body.error {
        value["error"] = Value::String(error);
    }
    Some(value)
}

fn ask_body(event: &Event) -> Option<Value> {
    let ask: AskBody = event.body_as().ok()?;
    if ask.silent {
        return None;
    }
    let mut body = serde_json::json!({ "text": ask.text });
    if !ask.images.is_empty() {
        body["images"] = serde_json::to_value(ask.images).ok()?;
    }
    Some(body)
}

fn result_body(event: &Event) -> Option<Value> {
    let result: ResultBody = event.body_as().ok()?;
    serde_json::to_value(result).ok()
}

fn proof_body(event: &Event) -> Option<Value> {
    let proof: ProofBody = event.body_as().ok()?;
    if proof.text.trim().is_empty() {
        return None;
    }
    let mut body = serde_json::json!({ "text": proof.text });
    if !proof.items.is_empty() {
        body["items"] = serde_json::to_value(proof.items).ok()?;
    }
    Some(body)
}

/// A question card. The `answer` is null until the user picks one, which is
/// what tells the screen to draw the choices unmarked.
fn question_body(event: &Event) -> Option<Value> {
    let question: QuestionBody = event.body_as().unwrap_or(QuestionBody {
        text: String::new(),
        choices: Vec::new(),
    });
    Some(serde_json::json!({
        "text": question.text,
        "choices": question.choices,
        "answer": Value::Null,
        "eventId": event.id,
    }))
}

/// A permission card. While the question is open it carries the diff or the
/// argv that would run, and its `decision` is null so the screen knows to draw
/// the action and the answer keys.
fn permission_body(event: &Event) -> Option<Value> {
    let permission: PermissionBody = event.body_as().unwrap_or_default();
    Some(serde_json::json!({
        "action": permission.action,
        "path": permission.path,
        "argv": permission.argv,
        "diff": permission.diff,
        "timeoutSec": permission.timeout_sec,
        "decision": Value::Null,
        "eventId": event.id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{now, AskBody, ModelMessageBody, ToolCallBody, ToolResultBody};

    #[test]
    fn a_successful_sibling_does_not_hide_a_failed_tool_batch() {
        let mut events = vec![
            event("model-1", EventKind::ModelMessage, serde_json::json!({})),
            event(
                "failed",
                EventKind::ToolResult,
                serde_json::json!({"tool":"read_file","output":"Missing file","is_error":true}),
            ),
            event(
                "succeeded",
                EventKind::ToolResult,
                serde_json::json!({"tool":"read_file","output":"Contents"}),
            ),
        ];
        assert!(tool_error_action(&events).unwrap().contains("Missing file"));
        events.push(event(
            "model-2",
            EventKind::ModelMessage,
            serde_json::json!({}),
        ));
        events.push(event(
            "recovered",
            EventKind::ToolResult,
            serde_json::json!({"tool":"read_file","output":"Contents"}),
        ));
        assert_eq!(tool_error_action(&events), None);
    }

    const AT: &str = "2026-09-29T00:00:00.000Z";

    fn event(id: &str, kind: EventKind, body: Value) -> Event {
        Event {
            id: id.to_string(),
            at: AT.to_string(),
            turn_id: "t1".to_string(),
            kind,
            body,
        }
    }

    fn ask(id: &str, text: &str) -> Event {
        event(
            id,
            EventKind::UserAsk,
            serde_json::to_value(AskBody {
                images: Vec::new(),
                text: text.into(),
                context: String::new(),
                skill: String::new(),
                silent: false,
            })
            .expect("an ask body"),
        )
    }

    #[test]
    fn a_model_message_and_a_tool_leave_no_card() {
        let events = vec![
            ask("e1", "Rename the binary."),
            event(
                "e2",
                EventKind::ModelMessage,
                serde_json::to_value(ModelMessageBody {
                    text: "Looking.".into(),
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
        ];
        let cards = cards(&events);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, CardKind::Ask);
        assert!(!serde_json::to_string(&cards)
            .expect("cards serialize")
            .contains("read_file"));
    }

    #[test]
    fn a_question_is_answered_in_place() {
        let events = vec![
            event(
                "e1",
                EventKind::Question,
                serde_json::to_value(QuestionBody {
                    text: "Which title?".into(),
                    choices: vec!["Kyoto Agent".into(), "Kyoto Agent CLI".into()],
                })
                .expect("a question body"),
            ),
            event(
                "e2",
                EventKind::QuestionAnswer,
                serde_json::to_value(QuestionAnswerBody {
                    question_id: Some("e1".into()),
                    answer: "Kyoto Agent".into(),
                })
                .expect("an answer body"),
            ),
        ];
        let cards = cards(&events);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].kind, CardKind::Question);
        assert_eq!(cards[0].body["text"], Value::from("Which title?"));
        assert_eq!(cards[0].body["answer"], Value::from("Kyoto Agent"));
        assert!(cards[0].body["choices"]
            .as_array()
            .expect("choices")
            .is_empty());
        assert_eq!(cards[1].kind, CardKind::Answer);
        assert_eq!(cards[1].body["text"], Value::from("Kyoto Agent"));
        assert_ne!(cards[1].body["text"], Value::from("1"));
    }

    #[test]
    fn a_waiting_question_stays_a_card() {
        let events = vec![event(
            "e1",
            EventKind::Question,
            serde_json::to_value(QuestionBody {
                text: "Which title?".into(),
                choices: vec!["Kyoto Agent".into(), "Kyoto Agent CLI".into()],
            })
            .expect("a question body"),
        )];
        let cards = cards(&events);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, CardKind::Question);
        assert!(cards[0].body["answer"].is_null());
        assert_eq!(cards[0].body["choices"][1], Value::from("Kyoto Agent CLI"));
        assert_eq!(cards[0].body["eventId"], Value::from("e1"));
    }

    #[test]
    fn a_waiting_permission_names_its_event_and_an_answer_drops_it() {
        let open = event(
            "e9",
            EventKind::Permission,
            serde_json::to_value(PermissionBody::write(
                "Replace src/view.rs",
                "/w/src/view.rs",
                &["+eventId"],
            ))
            .expect("a permission body"),
        );
        let waiting = cards(std::slice::from_ref(&open));
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].kind, CardKind::Permission);
        assert!(waiting[0].body["decision"].is_null());
        assert_eq!(waiting[0].body["eventId"], Value::from("e9"));
        assert_eq!(
            waiting[0].body["action"],
            Value::from("Replace src/view.rs")
        );
        assert_eq!(waiting[0].body["path"], Value::from("/w/src/view.rs"));
        let answered = vec![
            open,
            event(
                "e10",
                EventKind::PermissionAnswer,
                serde_json::to_value(PermissionAnswerBody {
                    permission_id: Some("e9".into()),
                    decision: crate::events::Decision::AllowOnce,
                })
                .expect("an answer body"),
            ),
        ];
        assert!(cards(&answered).is_empty());
    }

    #[test]
    fn an_unnamed_answer_lands_on_the_open_question() {
        let events = vec![
            event(
                "e1",
                EventKind::Question,
                serde_json::to_value(QuestionBody {
                    text: "First?".into(),
                    choices: vec!["a".into()],
                })
                .expect("a question body"),
            ),
            event(
                "e2",
                EventKind::Question,
                serde_json::to_value(QuestionBody {
                    text: "Second?".into(),
                    choices: vec!["b".into()],
                })
                .expect("a question body"),
            ),
            event(
                "e3",
                EventKind::QuestionAnswer,
                serde_json::to_value(QuestionAnswerBody {
                    question_id: None,
                    answer: "b".into(),
                })
                .expect("an answer body"),
            ),
        ];
        let cards = cards(&events);
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].kind, CardKind::Question);
        assert_eq!(cards[0].body["text"], Value::from("First?"));
        assert!(cards[0].body["answer"].is_null());
        assert_eq!(cards[1].kind, CardKind::Question);
        assert_eq!(cards[1].body["text"], Value::from("Second?"));
        assert_eq!(cards[1].body["answer"], Value::from("b"));
        assert!(cards[1].body["choices"]
            .as_array()
            .expect("choices")
            .is_empty());
        assert_eq!(cards[2].kind, CardKind::Answer);
        assert_eq!(cards[2].body["text"], Value::from("b"));
    }

    #[test]
    fn the_current_time_is_a_log_timestamp() {
        let stamp = now();
        assert_eq!(stamp.len(), 24, "YYYY-MM-DDTHH:MM:SS.mmmZ: {stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
    }

    #[test]
    fn the_latest_todos_event_is_the_view_list() {
        let first = event(
            "e2",
            EventKind::Todos,
            serde_json::json!({
                "items": [{ "id": "a", "content": "old", "status": "pending" }]
            }),
        );
        let second = event(
            "e3",
            EventKind::Todos,
            serde_json::json!({
                "items": [
                    { "id": "a", "content": "old", "status": "done" },
                    { "id": "b", "content": "now", "status": "in_progress" }
                ]
            }),
        );
        assert!(latest_todos(&[]).is_empty());
        let once = latest_todos(&[ask("e1", "Do it."), first.clone()]);
        assert_eq!(once.len(), 1);
        assert_eq!(once[0].title, "old");
        let latest = latest_todos(&[ask("e1", "Do it."), first, second]);
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[1].title, "now");
        let cards = cards(&[
            ask("e1", "Do it."),
            event(
                "e2",
                EventKind::Todos,
                serde_json::json!({
                    "items": [{ "id": "a", "content": "old", "status": "pending" }]
                }),
            ),
        ]);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].kind, CardKind::Ask);
    }

    #[test]
    fn pending_schedules_are_soonest_first_and_a_cancel_drops_one() {
        let soon = event(
            "e2",
            EventKind::Schedule,
            serde_json::json!({
                "id": "bbbbbbbb",
                "note": "later",
                "dueAt": "2026-09-29T00:20:00.000Z"
            }),
        );
        let first = event(
            "e3",
            EventKind::Schedule,
            serde_json::json!({
                "id": "aaaaaaaa",
                "note": "Check gh comments",
                "dueAt": "2026-09-29T00:10:00.000Z"
            }),
        );
        let now = 1_790_640_000_000_i64;
        let listed = pending_schedules(&[ask("e1", "Wake me."), soon.clone(), first.clone()], now);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "aaaaaaaa");
        assert_eq!(listed[0].note, "Check gh comments");
        assert_eq!(listed[0].remaining_min, 10);
        assert_eq!(listed[1].note, "later");
        assert_eq!(listed[1].remaining_min, 20);
        let cancelled = event(
            "e4",
            EventKind::ScheduleCancel,
            serde_json::json!({ "id": "aaaaaaaa" }),
        );
        let left = pending_schedules(&[ask("e1", "Wake me."), soon, first, cancelled], now);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "bbbbbbbb");
        let cards = cards(&[ask("e1", "Wake me.")]);
        assert_eq!(cards.len(), 1);
    }

    #[test]
    fn closeout_rows_follow_the_file_and_the_latest_run() {
        let dir =
            std::env::temp_dir().join(format!("kyotoagent-view-closeout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(closeout_rows(&dir, &[], true).is_empty());
        std::fs::create_dir_all(dir.join(".kyotoagent")).expect("dir");
        std::fs::write(
            dir.join(".kyotoagent").join("closeout.yaml"),
            "version: 1\nitems:\n  - id: test\n    kind: command\n    run: cargo test\n    hint: cargo test\n  - id: lint\n    kind: command\n    run: cargo clippy\n    hint: cargo clippy\n",
        )
        .expect("file");
        let missing = closeout_rows(&dir, &[], true);
        assert_eq!(missing.len(), 2);
        assert_eq!(missing[0].id, "test");
        assert_eq!(missing[1].id, "lint");
        assert!(missing
            .iter()
            .all(|row| row.status == CloseoutStatus::Missing));
        assert!(missing.iter().all(|row| row.exit.is_none()));

        let running = vec![event(
            "e1",
            EventKind::ToolCall,
            serde_json::json!({ "tool": "run_closeout", "args": { "id": "test" } }),
        )];
        let rows = closeout_rows(&dir, &running, true);
        assert_eq!(rows[0].status, CloseoutStatus::Running);
        assert_eq!(rows[1].status, CloseoutStatus::Missing);

        let settled = vec![
            event(
                "e1",
                EventKind::ToolCall,
                serde_json::json!({ "tool": "run_closeout", "args": { "id": "test" } }),
            ),
            event(
                "e2",
                EventKind::CloseoutRun,
                serde_json::json!({ "id": "test", "attempt": 1, "exit": 0, "tail": "ok" }),
            ),
            event(
                "e3",
                EventKind::ToolResult,
                serde_json::json!({ "tool": "run_closeout", "output": "passed" }),
            ),
            event(
                "e4",
                EventKind::ToolCall,
                serde_json::json!({ "tool": "run_closeout", "args": { "id": "lint" } }),
            ),
            event(
                "e5",
                EventKind::CloseoutRun,
                serde_json::json!({ "id": "lint", "attempt": 1, "exit": 1, "tail": "---\nfail" }),
            ),
            event(
                "e6",
                EventKind::ToolResult,
                serde_json::json!({ "tool": "run_closeout", "output": "failed" }),
            ),
            event(
                "e7",
                EventKind::ToolCall,
                serde_json::json!({ "tool": "run_closeout", "args": { "id": "test" } }),
            ),
            event(
                "e8",
                EventKind::CloseoutRun,
                serde_json::json!({ "id": "test", "attempt": 2, "exit": 0, "tail": "ok again" }),
            ),
            event(
                "e9",
                EventKind::ToolResult,
                serde_json::json!({ "tool": "run_closeout", "output": "passed" }),
            ),
            event(
                "e10",
                EventKind::ToolCall,
                serde_json::json!({ "tool": "run_closeout", "args": { "id": "test" } }),
            ),
        ];
        let rows = closeout_rows(&dir, &settled, true);
        assert_eq!(rows[0].status, CloseoutStatus::Running);
        assert_eq!(rows[0].attempt, Some(2));
        assert_eq!(rows[0].tail, "ok again");
        assert_eq!(rows[1].status, CloseoutStatus::Failed);
        assert_eq!(rows[1].exit, Some(1));
        assert_eq!(rows[1].kind, "command");
        assert_eq!(rows[1].hint, "cargo clippy");
        assert_eq!(rows[1].tail, "---\nfail");
        let json = serde_json::to_string(&rows[1]).expect("row");
        assert!(json.contains("\"status\":\"failed\""), "{json}");
        assert!(closeout_rows(&dir, &settled, false).is_empty());
        assert!(dir.join(".kyotoagent").join("closeout.yaml").is_file());
        std::fs::write(
            dir.join(".kyotoagent").join("closeout.yaml"),
            "version: 1\nitems: []\n",
        )
        .expect("empty file");
        assert!(closeout_rows(&dir, &[], true).is_empty());
        std::fs::write(
            dir.join(".kyotoagent").join("closeout.yaml"),
            "version: 1\nitems:\n  - id: ../nope\n",
        )
        .expect("bad file");
        assert!(closeout_rows(&dir, &[], true).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn enhance(id: &str, text: &str, error: Option<&str>) -> Event {
        let mut body = serde_json::json!({
            "text": text,
            "source": "short",
            "model": "m",
        });
        if let Some(error) = error {
            body["error"] = serde_json::Value::String(error.to_string());
        }
        event(id, EventKind::Enhance, body)
    }

    fn enhance_answer(id: &str, choice: &str, enhance_id: &str, text: Option<&str>) -> Event {
        let mut body = serde_json::json!({
            "enhanceId": enhance_id,
            "choice": choice,
        });
        if let Some(text) = text {
            body["text"] = serde_json::Value::String(text.to_string());
        }
        event(id, EventKind::EnhanceAnswer, body)
    }

    #[test]
    fn use_revise_and_discard_drop_the_enhance_card_and_retry_keeps_the_latest() {
        let open = vec![
            enhance("e1", "first", None),
            enhance_answer("e2", "retry", "e1", None),
            enhance("e3", "second", Some("boom")),
        ];
        let shown = cards(&open);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].kind, CardKind::Enhance);
        assert_eq!(shown[0].body["text"], "second");
        assert_eq!(shown[0].body["source"], "short");
        assert_eq!(shown[0].body["model"], "m");
        assert_eq!(shown[0].body["error"], "boom");
        assert_eq!(shown[0].body["eventId"], "e3");
        for choice in ["use", "revise", "discard"] {
            let text = (choice == "revise").then_some("edited");
            let events = vec![
                enhance("e1", "draft", None),
                enhance_answer("e2", choice, "e1", text),
            ];
            assert!(cards(&events).is_empty(), "{choice}");
        }
    }
}
