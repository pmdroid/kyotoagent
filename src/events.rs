//! The events a session log is made of.
//!
//! One turn appends one JSON object per line to `events.jsonl`, and nothing is
//! ever rewritten. A line looks like this:
//!
//! ```json
//! { "id": "e1", "at": "2026-09-29T00:00:00.000Z", "turnId": "t1", "kind": "tool_call", "body": {} }
//! ```
//!
//! The body stays a [`serde_json::Value`] on the event itself, because the log
//! is the model's transcript and a kind it does not know yet still has to
//! survive a round trip. The structs below are the bodies this crate reads and
//! writes, and each one is what its card shows.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// What kind of thing happened. The names are the ones in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// The user asked the agent something. This is what opens a turn.
    UserAsk,
    /// The model said something. It is a tool result, not a card.
    ModelMessage,
    /// The model called a tool. Never a card: a read leaves no mark.
    ToolCall,
    /// A tool came back. Never a card either.
    ToolResult,
    /// The agent asked the user to choose. Holds the turn open.
    Question,
    /// The user chose. Fills the question's answer.
    QuestionAnswer,
    /// The agent wants to write a file or run a command. Holds the turn open.
    Permission,
    /// The user allowed or denied it.
    PermissionAnswer,
    /// What the agent finished with.
    Result,
    /// What the turn changed, and the checks it ran.
    Proof,
    Artifact,
    Compact,
    Todos,
    CloseoutRun,
    CloseoutChanged,
    CloseoutStarted,
    CloseoutOutput,
    CloseoutBypassed,
    TaskStart,
    TaskDone,
    Schedule,
    ScheduleCancel,
    AskQueued,
    AskDequeued,
    EnhanceRequest,
    Enhance,
    EnhanceAnswer,
    BtwRequest,
    BtwResult,
}

impl EventKind {
    /// The name as the log spells it.
    pub fn label(self) -> &'static str {
        match self {
            EventKind::UserAsk => "user_ask",
            EventKind::ModelMessage => "model_message",
            EventKind::ToolCall => "tool_call",
            EventKind::ToolResult => "tool_result",
            EventKind::Question => "question",
            EventKind::QuestionAnswer => "question_answer",
            EventKind::Permission => "permission",
            EventKind::PermissionAnswer => "permission_answer",
            EventKind::Result => "result",
            EventKind::Proof => "proof",
            EventKind::Artifact => "artifact",
            EventKind::Compact => "compact",
            EventKind::Todos => "todos",
            EventKind::CloseoutRun => "closeout_run",
            EventKind::CloseoutChanged => "closeout_changed",
            EventKind::CloseoutStarted => "closeout_started",
            EventKind::CloseoutOutput => "closeout_output",
            EventKind::CloseoutBypassed => "closeout_bypassed",
            EventKind::TaskStart => "task_start",
            EventKind::TaskDone => "task_done",
            EventKind::Schedule => "schedule",
            EventKind::ScheduleCancel => "schedule_cancel",
            EventKind::AskQueued => "ask_queued",
            EventKind::AskDequeued => "ask_dequeued",
            EventKind::EnhanceRequest => "enhance_request",
            EventKind::Enhance => "enhance",
            EventKind::EnhanceAnswer => "enhance_answer",
            EventKind::BtwRequest => "btw_request",
            EventKind::BtwResult => "btw_result",
        }
    }

    /// Whether an event of this kind draws a card. A model message, a tool
    /// call, and a tool result are the quiet ones: they are in the log for the
    /// model and for debugging, and on no screen.
    pub fn is_card(self) -> bool {
        matches!(
            self,
            EventKind::UserAsk
                | EventKind::Question
                | EventKind::QuestionAnswer
                | EventKind::Permission
                | EventKind::Result
                | EventKind::Proof
                | EventKind::Artifact
                | EventKind::Enhance
                | EventKind::BtwRequest
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnhanceChoice {
    Use,
    Revise,
    Discard,
    Retry,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnhanceRequestBody {
    pub text: String,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnhanceBody {
    pub text: String,
    pub source: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnhanceAnswerBody {
    #[serde(rename = "enhanceId")]
    pub enhance_id: String,
    pub choice: EnhanceChoice,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

pub fn open_enhance(events: &[Event]) -> Option<&Event> {
    let mut open = None;
    for event in events {
        match event.kind {
            EventKind::Enhance => open = Some(event),
            EventKind::EnhanceAnswer => {
                let Ok(body) = event.body_as::<EnhanceAnswerBody>() else {
                    continue;
                };
                if body.choice == EnhanceChoice::Retry {
                    continue;
                }
                if open.is_some_and(|card| card.id == body.enhance_id) {
                    open = None;
                }
            }
            _ => {}
        }
    }
    open
}

/// One line of `events.jsonl`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Unique within the session. `e1`, `e2`, and so on.
    pub id: String,
    /// When it happened, as UTC with milliseconds.
    pub at: String,
    /// The turn it belongs to. `t1`, `t2`, and so on.
    #[serde(rename = "turnId")]
    pub turn_id: String,
    pub kind: EventKind,
    pub body: Value,
}

impl Event {
    /// An event with no body of its own, for a kind whose body the caller
    /// fills in later.
    pub fn new(id: &str, at: &str, turn_id: &str, kind: EventKind) -> Event {
        Event {
            id: id.to_string(),
            at: at.to_string(),
            turn_id: turn_id.to_string(),
            kind,
            body: Value::Object(serde_json::Map::new()),
        }
    }

    /// The event with `body` as its body.
    pub fn with_body<B: Serialize>(mut self, body: &B) -> Result<Event, serde_json::Error> {
        self.body = serde_json::to_value(body)?;
        Ok(self)
    }

    /// The body as the type its kind uses, or an error naming the event that
    /// did not fit.
    pub fn body_as<B: for<'de> Deserialize<'de>>(&self) -> Result<B, serde_json::Error> {
        serde_json::from_value(self.body.clone())
    }
}

/// What the user asked for. The body of a [`EventKind::UserAsk`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskBody {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::attachment::ImageAttachment>,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub skill: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub silent: bool,
}

/// What the model said. In the log for the transcript, on no screen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelMessageBody {
    pub text: String,
}

/// A tool the model called. The `tool` name is deliberately never projected
/// into a card: a quiet read must leave no trace on screen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallBody {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

/// What a tool returned. Also never a card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResultBody {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::attachment::ImageAttachment>,
    pub tool: String,
    #[serde(default)]
    pub output: String,
}

/// A question the agent asked. The body of a [`EventKind::Question`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionBody {
    pub text: String,
    /// The choices, in the order they were offered.
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visuals: Vec<crate::question::QuestionVisual>,
}

/// The chosen answer. `question_id` names the question it answers; without it
/// the answer belongs to the newest question in the same turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionAnswerBody {
    #[serde(
        rename = "questionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub question_id: Option<String>,
    pub answer: String,
}

/// A write or a command waiting on the user. The body of a
/// [`EventKind::Permission`].
///
/// `diff` and `argv` are the reason the card exists, so they are here in full
/// while the question is open. The log keeps them whatever the user answers;
/// it is the view that leaves them out once the answer is in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PermissionBody {
    /// The action as proposed, such as `Replace README.md`.
    pub action: String,
    /// The absolute path, for a write or for a read outside the workspace.
    /// Absent for a command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The exact argv, for a command. Absent for a write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    /// The proposed change, one line per entry. Absent for a command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<Vec<String>>,
    /// A timeout that is not the default one. Absent when the caller left it
    /// alone, because a card should not name a number nobody changed.
    #[serde(
        rename = "timeoutSec",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub timeout_sec: Option<u64>,
    /// How many bytes the write proposes. A card that had to cut its diff
    /// down still says how big the change is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// A digest of the bytes that were on disk when the card went up, so the
    /// answer can be checked against the file as it is when it comes back. A
    /// file that is not there yet has the digest of nothing.
    #[serde(rename = "oldHash", default, skip_serializing_if = "Option::is_none")]
    pub old_hash: Option<String>,
    /// The whole proposed contents.
    ///
    /// The card draws the diff and not this, so the view never projects it: the
    /// log keeps every byte of what was asked for, and the screen keeps the
    /// part a person reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
}

impl PermissionBody {
    /// A write to `path`, with the change it proposes.
    pub fn write(action: &str, path: &str, diff: &[&str]) -> PermissionBody {
        PermissionBody {
            action: action.to_string(),
            path: Some(path.to_string()),
            diff: Some(diff.iter().map(|line| (*line).to_string()).collect()),
            ..PermissionBody::default()
        }
    }

    /// A command, with the argv it would run.
    pub fn run(action: &str, argv: &[&str], timeout_sec: Option<u64>) -> PermissionBody {
        PermissionBody {
            action: action.to_string(),
            argv: Some(argv.iter().map(|arg| (*arg).to_string()).collect()),
            timeout_sec,
            ..PermissionBody::default()
        }
    }

    /// Whether this is a command rather than a write. A permission with an argv
    /// is a command; one with a path is a write.
    pub fn is_command(&self) -> bool {
        self.argv.is_some()
    }
}

/// What the user answered. The body of a [`EventKind::PermissionAnswer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Do this once. Nothing is remembered.
    AllowOnce,
    /// Do this, and anything exactly like it, for the rest of the session.
    AllowSession,
    /// Do not. The tool result is the denial and the workspace is untouched.
    Deny,
}

impl Decision {
    /// The verb a decided card leads with.
    pub fn verb(self) -> &'static str {
        match self {
            Decision::AllowOnce | Decision::AllowSession => "Allowed",
            Decision::Deny => "Denied",
        }
    }
}

/// The answer to a permission. `permission_id` names the permission; without
/// it the answer belongs to the newest open permission in the same turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionAnswerBody {
    #[serde(
        rename = "permissionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_id: Option<String>,
    pub decision: Decision,
}

/// What the agent finished with. The body of a [`EventKind::Result`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResultBody {
    pub text: String,
    /// A short note from `finish`, kept on the turn so the proof issue can copy
    /// it. Empty when the turn ended any other way.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// One check the turn ran and did not pass. `tail` is the last few lines of its
/// output, because the whole output is the model's business, not the screen's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofFailure {
    /// The command, as it was run.
    pub argv: Vec<String>,
    /// Its exit code.
    pub exit: i32,
    /// The end of its output.
    #[serde(default)]
    pub tail: String,
}

/// The finished turn's record. The body of a [`EventKind::Proof`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofBody {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::proof::ProofFile>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// What the runner wrote, in the order it wrote it.
    #[serde(default)]
    pub wrote: Vec<String>,
    /// How the turn ended, such as `passed` or `failed`.
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub head: String,
    #[serde(
        default,
        rename = "workspaceFingerprint",
        skip_serializing_if = "String::is_empty"
    )]
    pub workspace_fingerprint: String,
    /// The diffstat, as one line.
    #[serde(rename = "diffStat", default)]
    pub diff_stat: String,
    /// A sentence for the user, when there is one worth reading.
    #[serde(default)]
    pub note: String,
    /// Every check that did not pass. An empty list is a turn that passed them
    /// all, not a turn that ran none.
    #[serde(default)]
    pub failures: Vec<ProofFailure>,
    #[serde(default)]
    pub items: Vec<ProofItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactBody {
    #[serde(default)]
    pub source: ArtifactSource,
    pub file: crate::proof::ProofFile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<ArtifactCheck>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactSource {
    #[default]
    Agent,
    Closeout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCheck {
    pub id: String,
    pub attempt: u32,
    #[serde(default, rename = "eventId", skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofItem {
    pub id: String,
    pub kind: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tail: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CloseoutChangedBody {
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CloseoutStartedBody {
    pub id: String,
    pub attempt: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CloseoutOutputBody {
    pub id: String,
    pub attempt: u32,
    pub stderr: bool,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseoutRunBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<crate::proof::ProofFile>,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub truncated: bool,
    pub id: String,
    pub attempt: u32,
    pub exit: i32,
    pub tail: String,
    #[serde(
        default,
        rename = "workspaceFingerprint",
        skip_serializing_if = "String::is_empty"
    )]
    pub workspace_fingerprint: String,
    #[serde(
        default,
        rename = "policyDigest",
        skip_serializing_if = "String::is_empty"
    )]
    pub policy_digest: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompactBody {
    pub summary: String,
    #[serde(rename = "throughEventId")]
    pub through_event_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    #[serde(alias = "content")]
    pub title: String,
    pub status: TodoStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodosBody {
    pub items: Vec<TodoItem>,
}

pub fn parse_todo_items(args: &Value) -> Result<Vec<TodoItem>, String> {
    let items = args
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "todo needs items".to_string())?;
    if items.len() > 20 {
        return Err("todo items must be at most 20".to_string());
    }
    let mut out = Vec::with_capacity(items.len());
    let mut in_progress = 0usize;
    for item in items {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "todo needs id".to_string())?
            .to_string();
        if !valid_todo_id(&id) {
            return Err("todo id must match [a-z0-9-]+".to_string());
        }
        if out.iter().any(|seen: &TodoItem| seen.id == id) {
            return Err("todo ids must be unique".to_string());
        }
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| "todo needs title".to_string())?
            .to_string();
        if title.chars().count() > 120 {
            return Err("todo title must be at most 120 characters".to_string());
        }
        let description = parse_todo_description(item)?;
        let files = parse_todo_files(item)?;
        let links = parse_todo_links(item)?;
        let status = match item.get("status").and_then(Value::as_str) {
            Some("pending") => TodoStatus::Pending,
            Some("in_progress") => TodoStatus::InProgress,
            Some("done") => TodoStatus::Done,
            _ => return Err("todo status must be pending, in_progress, or done".to_string()),
        };
        if status == TodoStatus::InProgress {
            in_progress += 1;
            if in_progress > 1 {
                return Err("todo allows at most one in_progress item".to_string());
            }
        }
        out.push(TodoItem {
            id,
            title,
            status,
            description,
            files,
            links,
        });
    }
    Ok(out)
}

fn parse_todo_description(item: &Value) -> Result<Option<String>, String> {
    let Some(value) = item.get("description") else {
        return Ok(None);
    };
    let description = value
        .as_str()
        .ok_or_else(|| "todo description must be a string".to_string())?;
    if description.chars().count() > 4000 {
        return Err("todo description must be at most 4000 characters".to_string());
    }
    if description.is_empty() {
        Ok(None)
    } else {
        Ok(Some(description.to_string()))
    }
}

fn parse_todo_files(item: &Value) -> Result<Vec<String>, String> {
    parse_todo_strings(item, "files", 8, valid_todo_file)
}

fn parse_todo_links(item: &Value) -> Result<Vec<String>, String> {
    parse_todo_strings(item, "links", 8, valid_todo_link)
}

fn parse_todo_strings(
    item: &Value,
    field: &str,
    cap: usize,
    valid: fn(&str) -> Result<(), String>,
) -> Result<Vec<String>, String> {
    let Some(value) = item.get(field) else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_array()
        .ok_or_else(|| format!("todo {field} must be an array"))?;
    if entries.len() > cap {
        return Err(format!("todo {field} must be at most {cap}"));
    }
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let text = entry
            .as_str()
            .ok_or_else(|| format!("todo {field} must be strings"))?;
        if text.chars().count() > 500 {
            return Err(format!("todo {field} must be at most 500 characters"));
        }
        valid(text)?;
        out.push(text.to_string());
    }
    Ok(out)
}

fn valid_todo_file(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("todo file must be a workspace-relative path".to_string());
    }
    if path.starts_with('/') || path.contains('!') || path.contains('\\') {
        return Err("todo file must be a workspace-relative path".to_string());
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err("todo file must be a workspace-relative path".to_string());
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("todo file must be a workspace-relative path".to_string());
        }
    }
    Ok(())
}

fn valid_todo_link(link: &str) -> Result<(), String> {
    let Some((scheme, rest)) = link.split_once(':') else {
        return Err("todo link must be an http or https URL".to_string());
    };
    if scheme != "http" && scheme != "https" {
        return Err("todo link must be an http or https URL".to_string());
    }
    let bytes = rest.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'/' && bytes[1] == b'/' {
        return Ok(());
    }
    Err("todo link must be an http or https URL".to_string())
}

fn valid_todo_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The current time as the log spells it: UTC, milliseconds, `Z`.
pub fn now() -> String {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_millis(since_epoch.as_millis() as i64)
}

/// A count of milliseconds since the epoch as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
///
/// The log does not need a date library: the only thing it does with time is
/// order the events already have, and print when one happened.
pub fn format_millis(millis: i64) -> String {
    let (days, rest) = (millis.div_euclid(86_400_000), millis.rem_euclid(86_400_000));
    let (year, month, day) = civil_from_days(days);
    let seconds = rest / 1000;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60,
        rest % 1000
    )
}

pub fn parse_millis(stamp: &str) -> Option<i64> {
    if stamp.len() != 24 || !stamp.ends_with('Z') {
        return None;
    }
    let year: i64 = stamp.get(0..4)?.parse().ok()?;
    if stamp.get(4..5)? != "-" {
        return None;
    }
    let month: u32 = stamp.get(5..7)?.parse().ok()?;
    if stamp.get(7..8)? != "-" {
        return None;
    }
    let day: u32 = stamp.get(8..10)?.parse().ok()?;
    if stamp.get(10..11)? != "T" {
        return None;
    }
    let hour: i64 = stamp.get(11..13)?.parse().ok()?;
    if stamp.get(13..14)? != ":" {
        return None;
    }
    let minute: i64 = stamp.get(14..16)?.parse().ok()?;
    if stamp.get(16..17)? != ":" {
        return None;
    }
    let second: i64 = stamp.get(17..19)?.parse().ok()?;
    if stamp.get(19..20)? != "." {
        return None;
    }
    let millis: i64 = stamp.get(20..23)?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some(days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1000 + millis)
}

/// The calendar date of a count of days since 1970-01-01.
///
/// Days since the epoch become a date with the shift that puts March at the
/// start of the year, so the leap day lands at the end of it.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = if month > 2 {
        month as i64 - 3
    } else {
        month as i64 + 9
    };
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Exited,
    TimedOut,
    Stopped,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Running => "running",
            TaskStatus::Exited => "exited",
            TaskStatus::TimedOut => "timed_out",
            TaskStatus::Stopped => "stopped",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskStartBody {
    pub id: String,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskDoneBody {
    pub id: String,
    pub argv: Vec<String>,
    pub exit: i32,
    #[serde(default)]
    pub tail: String,
    pub state: TaskStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduleBody {
    pub id: String,
    pub note: String,
    #[serde(rename = "dueAt")]
    pub due_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduleCancelBody {
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskQueuedBody {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub enhance: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::attachment::ImageAttachment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_itself_is_a_known_moment() {
        assert_eq!(format_millis(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_leap_day_lands_where_it_belongs() {
        // 2000 was a leap year and a century that stayed divisible by 400, and
        // 1900 was a century that did not.
        assert_eq!(format_millis(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(
            format_millis(-2_203_891_200_000),
            "1900-03-01T00:00:00.000Z"
        );
    }

    #[test]
    fn a_stamp_parses_back_to_its_millis() {
        for millis in [0_i64, 1_790_640_000_000, 951_782_400_000] {
            assert_eq!(parse_millis(&format_millis(millis)), Some(millis));
        }
    }

    #[test]
    fn a_leap_second_of_milliseconds_stays_in_its_second() {
        // A millisecond count with every field filled in, so a day boundary
        // cannot drift into the next day unnoticed.
        assert_eq!(format_millis(1_790_683_196_123), "2026-09-29T11:59:56.123Z");
    }

    #[test]
    fn the_quiet_kinds_draw_no_card() {
        assert!(!EventKind::ModelMessage.is_card());
        assert!(!EventKind::ToolCall.is_card());
        assert!(!EventKind::ToolResult.is_card());
        assert!(!EventKind::Compact.is_card());
        assert!(!EventKind::Todos.is_card());
        assert!(!EventKind::CloseoutRun.is_card());
        assert!(!EventKind::TaskStart.is_card());
        assert!(!EventKind::TaskDone.is_card());
        assert!(!EventKind::Schedule.is_card());
        assert!(!EventKind::ScheduleCancel.is_card());
        assert!(!EventKind::AskQueued.is_card());
        assert!(!EventKind::AskDequeued.is_card());
        assert!(!EventKind::EnhanceRequest.is_card());
        assert!(!EventKind::EnhanceAnswer.is_card());
        for kind in [
            EventKind::UserAsk,
            EventKind::Question,
            EventKind::QuestionAnswer,
            EventKind::Permission,
            EventKind::Result,
            EventKind::Proof,
            EventKind::Enhance,
        ] {
            assert!(kind.is_card(), "{} draws a card", kind.label());
        }
    }

    #[test]
    fn an_event_round_trips_through_its_line() {
        let event = Event::new(
            "e7",
            "2026-09-29T00:00:00.000Z",
            "t1",
            EventKind::Permission,
        )
        .with_body(&PermissionBody::write(
            "Replace README.md",
            "/w/README.md",
            &["@@ -1 +1,2 @@"],
        ))
        .expect("a body serializes");
        let line = serde_json::to_string(&event).expect("an event serializes");
        // The line names the event, its turn, its kind, and its body, and a
        // body keeps only the fields that kind has.
        let parsed: Value = serde_json::from_str(&line).expect("the line is JSON");
        assert_eq!(parsed["id"], Value::from("e7"));
        assert_eq!(parsed["turnId"], Value::from("t1"));
        assert_eq!(parsed["kind"], Value::from("permission"));
        assert_eq!(parsed["body"]["action"], Value::from("Replace README.md"));
        assert!(parsed["body"].get("argv").is_none(), "a write has no argv");
        assert!(parsed["body"].get("timeoutSec").is_none(), "and no timeout");
        let back: Event = serde_json::from_str(&line).expect("an event parses");
        assert_eq!(back, event);
    }

    #[test]
    fn a_closeout_run_event_round_trips_through_its_line() {
        let event = Event::new(
            "e9",
            "2026-09-29T00:00:00.000Z",
            "t1",
            EventKind::CloseoutRun,
        )
        .with_body(&CloseoutRunBody {
            passed: None,
            transcript: None,
            argv: Vec::new(),
            timed_out: false,
            truncated: false,
            id: "test".to_string(),
            attempt: 1,
            exit: 0,
            tail: "test result: ok".to_string(),
            workspace_fingerprint: String::new(),
            policy_digest: String::new(),
        })
        .expect("a body serializes");
        let line = serde_json::to_string(&event).expect("an event serializes");
        let parsed: Value = serde_json::from_str(&line).expect("the line is JSON");
        assert_eq!(parsed["kind"], Value::from("closeout_run"));
        assert_eq!(parsed["body"]["id"], Value::from("test"));
        assert_eq!(parsed["body"]["attempt"], Value::from(1));
        assert_eq!(parsed["body"]["exit"], Value::from(0));
        let back: Event = serde_json::from_str(&line).expect("the line parses");
        assert_eq!(back, event);
    }

    #[test]
    fn a_proof_stores_each_closeout_item() {
        let event = Event::new("e8", "2026-09-29T00:00:00.000Z", "t1", EventKind::Proof)
            .with_body(&ProofBody {
                files: Vec::new(),
                text: String::new(),
                wrote: vec!["README.md".into()],
                status: "M README.md".into(),
                head: String::new(),
                workspace_fingerprint: String::new(),
                diff_stat: "README.md | 1 +".into(),
                note: String::new(),
                failures: vec![ProofFailure {
                    argv: vec!["cargo".into(), "clippy".into()],
                    exit: 1,
                    tail: "error: unused".into(),
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
                        tail: "error: unused".into(),
                    },
                ],
            })
            .expect("a body serializes");
        let line = serde_json::to_string(&event).expect("an event serializes");
        let parsed: Value = serde_json::from_str(&line).expect("the line is JSON");
        assert_eq!(parsed["body"]["items"][0]["id"], Value::from("test"));
        assert_eq!(parsed["body"]["items"][0]["outcome"], Value::from("passed"));
        assert!(parsed["body"]["items"][0].get("argv").is_none());
        assert_eq!(parsed["body"]["items"][1]["id"], Value::from("lint"));
        assert_eq!(parsed["body"]["items"][1]["outcome"], Value::from("failed"));
        assert_eq!(parsed["body"]["items"][1]["exit"], Value::from(1));
        assert_eq!(
            parsed["body"]["items"][1]["tail"],
            Value::from("error: unused")
        );
        let back: Event = serde_json::from_str(&line).expect("the line parses");
        assert_eq!(back, event);
    }

    #[test]
    fn a_command_permission_knows_it_is_one() {
        let body = PermissionBody::run("Run cargo test", &["cargo", "test"], None);
        assert!(body.is_command());
        assert!(!PermissionBody::write("Replace README.md", "/w/README.md", &[]).is_command());
    }

    fn todo(id: &str, title: &str, status: &str) -> Value {
        serde_json::json!({ "id": id, "title": title, "status": status })
    }

    fn https_url(host_path: &str) -> String {
        let mut url = String::from("https:");
        url.push('/');
        url.push('/');
        url.push_str(host_path);
        url
    }

    #[test]
    fn a_valid_todo_list_parses() {
        let items = parse_todo_items(&serde_json::json!({
            "items": [
                todo("read", "Read the crate", "done"),
                todo("write", "Write the tool", "in_progress"),
                todo("draw", "Draw the pane", "pending"),
            ]
        }))
        .expect("the list parses");
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].status, TodoStatus::InProgress);
    }

    #[test]
    fn twenty_one_todo_items_are_refused() {
        let items: Vec<Value> = (0..21)
            .map(|i| todo(&format!("n{i}"), "step", "pending"))
            .collect();
        let error = parse_todo_items(&serde_json::json!({ "items": items })).expect_err("capped");
        assert!(error.contains("20"), "{error}");
    }

    #[test]
    fn a_bad_todo_status_is_refused() {
        let error = parse_todo_items(&serde_json::json!({
            "items": [todo("a", "step", "working")]
        }))
        .expect_err("bad status");
        assert!(error.contains("status"), "{error}");
    }

    #[test]
    fn two_in_progress_todos_are_refused() {
        let error = parse_todo_items(&serde_json::json!({
            "items": [
                todo("a", "one", "in_progress"),
                todo("b", "two", "in_progress"),
            ]
        }))
        .expect_err("one in progress");
        assert!(error.contains("in_progress"), "{error}");
    }

    #[test]
    fn a_todo_keeps_title_description_files_and_links() {
        let mut item = todo("write", "Write the todo tool", "in_progress");
        item["description"] = Value::from("Replace content with title.");
        item["files"] = serde_json::json!(["src/events.rs"]);
        item["links"] = serde_json::json!([https_url("docs.rs/serde")]);
        let items = parse_todo_items(&serde_json::json!({ "items": [item] })).expect("parses");
        assert_eq!(items[0].title, "Write the todo tool");
        assert_eq!(
            items[0].description.as_deref(),
            Some("Replace content with title.")
        );
        assert_eq!(items[0].files, vec!["src/events.rs"]);
        assert_eq!(items[0].links, vec![https_url("docs.rs/serde")]);
        let json = serde_json::to_value(&items[0]).expect("serializes");
        assert!(json.get("content").is_none(), "{json}");
        assert_eq!(json["title"], Value::from("Write the todo tool"));
    }

    #[test]
    fn empty_todo_files_and_links_omit_from_json() {
        let item = TodoItem {
            id: "write".into(),
            title: "Write the todo tool".into(),
            status: TodoStatus::Pending,
            description: None,
            files: Vec::new(),
            links: Vec::new(),
        };
        let json = serde_json::to_value(&item).expect("serializes");
        assert!(json.get("files").is_none(), "{json}");
        assert!(json.get("links").is_none(), "{json}");
        assert!(json.get("description").is_none(), "{json}");
    }

    #[test]
    fn a_stored_content_field_projects_as_title() {
        let item: TodoItem = serde_json::from_value(serde_json::json!({
            "id": "read",
            "content": "Read the crate",
            "status": "done"
        }))
        .expect("legacy content parses");
        assert_eq!(item.title, "Read the crate");
        assert!(item.description.is_none());
        assert!(item.files.is_empty());
        assert!(item.links.is_empty());
    }

    #[test]
    fn a_todo_call_with_content_and_no_title_is_refused() {
        let error = parse_todo_items(&serde_json::json!({
            "items": [{ "id": "a", "content": "old", "status": "pending" }]
        }))
        .expect_err("content is not title");
        assert!(error.contains("title"), "{error}");
    }

    #[test]
    fn a_long_todo_title_is_refused() {
        let title = "a".repeat(121);
        let error = parse_todo_items(&serde_json::json!({
            "items": [todo("a", &title, "pending")]
        }))
        .expect_err("title cap");
        assert!(error.contains("120"), "{error}");
    }

    #[test]
    fn a_long_todo_description_is_refused() {
        let mut item = todo("a", "step", "pending");
        item["description"] = Value::from("a".repeat(4001));
        let error =
            parse_todo_items(&serde_json::json!({ "items": [item] })).expect_err("desc cap");
        assert!(error.contains("4000"), "{error}");
    }

    #[test]
    fn a_ninth_todo_file_is_refused() {
        let mut item = todo("a", "step", "pending");
        item["files"] = serde_json::json!([
            "a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs", "g.rs", "h.rs", "i.rs"
        ]);
        let error =
            parse_todo_items(&serde_json::json!({ "items": [item] })).expect_err("file cap");
        assert!(error.contains("8"), "{error}");
    }

    #[test]
    fn a_todo_file_with_dotdot_is_refused() {
        let mut item = todo("a", "step", "pending");
        item["files"] = serde_json::json!(["src/../secret"]);
        let error = parse_todo_items(&serde_json::json!({ "items": [item] })).expect_err("path");
        assert!(error.contains("path"), "{error}");
    }

    #[test]
    fn a_todo_link_that_is_not_http_is_refused() {
        let mut item = todo("a", "step", "pending");
        item["links"] = serde_json::json!(["ftp:example.com"]);
        let error = parse_todo_items(&serde_json::json!({ "items": [item] })).expect_err("link");
        assert!(error.contains("http"), "{error}");
    }
}
