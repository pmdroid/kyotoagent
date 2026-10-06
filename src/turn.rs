//! The turn loop: one ask runs a tool loop against the chat client until the
//! model finishes, the user cancels, or a model request fails.
//!
//! An ask appends `user_ask`, sets the session `working`, and posts the model
//! transcript. The model sees every tool call and tool result. The session view
//! stays the projection from the log: a read inside the workspace leaves no
//! card, and a write or a command waits on the permission gate.
//!
//! One task per running turn. A second message while a turn or a compact is
//! running waits in a queue of eight; a waiting permission or question is
//! still refused. A message to another session starts immediately. Tasks share
//! no lock. A turn is cancelled at the next tool boundary: a running command
//! gets SIGTERM and then SIGKILL two seconds later, and the turn ends with a
//! result that says it was stopped. The next queued ask starts once the
//! session is idle.
//!
//! A finished turn appends a `proof` event: what it wrote, the git state of the
//! workspace, and every command that did not succeed. The log keeps every
//! `write_file` and `run` tool result, including exit code and output, and the
//! proof event is built from that record.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;

use crate::chat::{self, ChatClient, ChatError, Message, Reply, Tool, ToolCall};
use crate::closeout::CloseoutState;
use crate::config::Config;
use crate::events::{
    now, parse_todo_items, AskBody, CompactBody, Event, EventKind, ModelMessageBody,
    PermissionAnswerBody, ProofBody, ProofFailure, QuestionAnswerBody, ResultBody, TodosBody,
    ToolCallBody, ToolResultBody,
};
use crate::hooks;
use crate::permit::{Answer, AnswerError, GateError};
use crate::prompt;
use crate::schedule::{Clock, ScheduleWake};
use crate::screen::{Phase, Status};
use crate::session::{Session, SessionError, SessionMeta};
use crate::skills;
use crate::task;
use crate::tools::{RunOutput, ToolError, Tools};
use crate::view;

mod batch;
mod closeout;
mod compact;
mod dispatch;
mod enhance;
mod goal;

pub use enhance::EnhanceError;
mod run;
mod spawn;

use closeout::*;
use compact::*;
use dispatch::*;
use run::*;

pub fn known_tool_names() -> &'static [&'static str] {
    &[
        "read_file",
        "web_fetch",
        "web_search",
        "grep",
        "list_dir",
        "search_replace",
        "write_file",
        "run",
        "start_task",
        "check_task",
        "kill_task",
        "spawn_subagent",
        "schedule",
        "cancel_schedule",
        "run_closeout",
        "ask",
        "finish",
        "attach_artifact",
        "use_skill",
        "todo",
    ]
}

/// The tools the model sees, in the function shape the server expects.
pub fn tool_definitions(config: &Config, child: bool) -> Vec<Tool> {
    tool_definitions_for(config, child, None)
}

pub fn tool_definitions_for(config: &Config, child: bool, profile: Option<&str>) -> Vec<Tool> {
    let mut tools = vec![
        Tool::new(
            "read_file",
            "Read text, images or PDFs, detected from their bytes. For text, pass line for a 1-based line or offset for a byte offset. For PDFs, format is image by default or text, and pages is a 1-based range such as 1-5. Specify pages for PDFs over 10 pages; read at most 20 pages per call.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer" },
                    "line": { "type": "integer" },
                    "limit": { "type": "integer" },
                    "pages": { "type": "string" },
                    "format": { "type": "string", "enum": ["image", "text"] },
                },
                "required": ["path"],
            }),
        ),
        Tool::new(
            "web_fetch",
            "Fetch a public URL. HTML comes back as markdown.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "max_bytes": { "type": "integer" },
                },
                "required": ["url"],
            }),
        ),
        Tool::new(
            "grep",
            "Search the workspace. Returns path:line:text.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "glob": { "type": "string" },
                    "head_limit": { "type": "integer" },
                },
                "required": ["pattern"],
            }),
        ),
        Tool::new(
            "list_dir",
            "List one level of names in the workspace.",
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }),
        ),
        Tool::new(
            "search_replace",
            "Replace an exact string in a file. old_string must match once unless replace_all is true.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" },
                },
                "required": ["path", "old_string", "new_string"],
            }),
        ),
        Tool::new(
            "write_file",
            "Replace a file in the workspace with new contents.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "contents": { "type": "string" },
                },
                "required": ["path", "contents"],
            }),
        ),
        Tool::new(
            "run",
            "Run a command in the workspace.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "argv": { "type": "array", "items": { "type": "string" } },
                    "timeout_sec": { "type": "integer" },
                },
                "required": ["argv"],
            }),
        ),
        Tool::new(
            "start_task",
            "Start a command in the workspace and return while it still runs.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "argv": { "type": "array", "items": { "type": "string" } },
                    "timeout_sec": { "type": "integer" },
                },
                "required": ["argv"],
            }),
        ),
        Tool::new(
            "check_task",
            "Check background commands or subagent sessions. id is one. ids is several. timeout_sec waits until every listed id is idle; zero or omitted returns snapshots now.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "ids": { "type": "array", "items": { "type": "string" } },
                    "timeout_sec": { "type": "integer" },
                },
            }),
        ),
        Tool::new(
            "kill_task",
            "Stop a background command. Stop a subagent and remove its session and worktree.",
            serde_json::json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
            }),
        ),
        Tool::new(
            "spawn_subagent",
            "Start a child session on this serve. A child is hidden from the session list unless visible is true. isolation is none or worktree. cwd and worktree together are refused. Call kill_task with the id when the child is done.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "description": { "type": "string" },
                    "run_in_background": { "type": "boolean" },
                    "isolation": { "type": "string" },
                    "resume_from": { "type": "string" },
                    "cwd": { "type": "string" },
                    "model": { "type": "string" },
                    "visible": { "type": "boolean" },
                },
                "required": ["prompt", "description"],
            }),
        ),
        Tool::new(
            "schedule",
            "Wake after a number of minutes with a note.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "minutes": { "type": "integer" },
                    "note": { "type": "string" },
                },
                "required": ["minutes", "note"],
            }),
        ),
        Tool::new(
            "cancel_schedule",
            "Cancel a pending schedule by id.",
            serde_json::json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
            }),
        ),
        Tool::new(
            "run_closeout",
            "Run a closeout check by its id and retain its transcript with the check. Review checks start a fresh reviewer session following the configured skill. Set model to a different available model when required by independence. This does not publish an artifact. To share useful evidence, explicitly call attach_artifact with the returned file_id.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "model": { "type": "string" },
                },
                "required": ["id"],
            }),
        ),
        Tool::new(
            "ask",
            "Ask the user a question and wait for the answer.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "choices": { "type": "array", "items": { "type": "string" } },
                },
                "required": ["text"],
            }),
        ),
        Tool::new(
            "attach_artifact",
            "Choose a useful deliverable or evidence file to publish immediately as a clickable chat artifact with a retained session copy. This is the only artifact publication tool. Provide exactly one of path or an archived file_id returned by run_closeout. Use Markdown reports, screenshots, videos, PDFs, HTML, archives or relevant terminal transcripts. Include git_sha when documenting or verifying a specific commit. Never invent evidence or attach routine tool output, transient lookup errors or a duplicate response merely to finish a turn. Attachments do not satisfy verification requirements. Answers and progress need no attachment.",
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" }, "file_id": { "type": "string" }, "git_sha": { "type": "string" }, "caption": { "type": "string" }, "check": { "type": "object", "properties": { "id": { "type": "string" }, "attempt": { "type": "integer", "minimum": 1 } }, "required": ["id", "attempt"] } },
            }),
        ),
        Tool::new(
            "finish",
            "End the turn. text is the result card. proof is optional. Leave proof empty when it would only repeat text. A task exit is not a result, and finish waits until the tasks started on this turn have exited.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "note": { "type": "string" },
                    "proof": { "type": "string" },
                },
                "required": ["text"],
            }),
        ),
        Tool::new(
            "use_skill",
            "Load a skill's instructions by name. Before task work, compare the request with the available skill descriptions and load clearly matching skills without waiting for a slash command. Follow the loaded instructions. Reconsider matching skills when moving to verification or shipping.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "args": { "type": "string" },
                },
                "required": ["name"],
            }),
        ),
        Tool::new(
            "todo",
            "Replace the session todo list.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "title": { "type": "string" },
                                "status": { "type": "string" },
                                "description": { "type": "string" },
                                "files": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                },
                                "links": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                },
                            },
                            "required": ["id", "title", "status"],
                        },
                    },
                },
                "required": ["items"],
            }),
        ),
    ];
    let search_allowed = config.tool_allowed(profile, "web_search");
    if config.search_vendor().is_some() && search_allowed {
        let search = Tool::new(
            "web_search",
            "Search the web. Returns a title, a url, and a short snippet for each hit.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "num_results": { "type": "integer" },
                },
                "required": ["query"],
            }),
        );
        let at = tools
            .iter()
            .position(|tool| tool.name == "web_fetch")
            .map(|index| index + 1)
            .unwrap_or(tools.len());
        tools.insert(at, search);
    }
    if let Some(name) = profile {
        match config.profile(name) {
            Some(profile) => {
                if profile.tools.is_some() {
                    tools.retain(|tool| config.tool_allowed(Some(&profile.id), &tool.name));
                }
            }
            None => tools.clear(),
        }
    }
    if child {
        tools.retain(|tool| tool.name != "ask" && tool.name != "spawn_subagent");
    }
    tools
}

pub(super) fn reviewer_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "grep"
            | "list_dir"
            | "web_fetch"
            | "web_search"
            | "use_skill"
            | "todo"
            | "finish"
    )
}

/// Something the runner could not do.
#[derive(Debug)]
pub enum TurnError {
    /// The session is not one this runner knows.
    NoSession,
    /// The session is waiting on a permission or a question.
    Busy,
    /// The session is archived, so a new ask is refused until it is restored.
    Archived,
    /// The ask queue already holds eight texts.
    QueueFull,
    /// The chat client could not be built.
    Chat(ChatError),
    /// The session directory could not be read or written.
    Session(SessionError),
    /// A tool could not do its job.
    Tools(ToolError),
    /// The gate could not ask or answer.
    Gate(GateError),
    /// A blocking task did not finish.
    Join(tokio::task::JoinError),
    /// The tool arguments were not the JSON the tool expected.
    Args(serde_json::Error),
    Goal(String),
    ContextLimit {
        estimated: u64,
        limit: u64,
    },
    Config(crate::config::ConfigError),
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TurnError::NoSession => write!(f, "no such session"),
            TurnError::Busy => write!(f, "the session is already working"),
            TurnError::Archived => write!(f, "the session is archived"),
            TurnError::QueueFull => write!(f, "the queue is full"),
            TurnError::Chat(source) => write!(f, "{source}"),
            TurnError::Session(source) => write!(f, "{source}"),
            TurnError::Tools(source) => write!(f, "{source}"),
            TurnError::Gate(source) => write!(f, "{source}"),
            TurnError::Join(source) => write!(f, "a task did not finish: {source}"),
            TurnError::Config(source) => write!(f, "{source}"),
            TurnError::Goal(message) => write!(f, "{message}"),
            TurnError::ContextLimit { estimated, limit } => write!(f, "The estimated request uses {estimated} tokens and exceeds the model context limit {limit}. Reduce the prompt or tool output and retry."),
            TurnError::Args(source) => write!(f, "the tool arguments were not JSON: {source}"),
        }
    }
}

impl std::error::Error for TurnError {}

impl From<SessionError> for TurnError {
    fn from(source: SessionError) -> TurnError {
        TurnError::Session(source)
    }
}

impl From<ToolError> for TurnError {
    fn from(source: ToolError) -> TurnError {
        TurnError::Tools(source)
    }
}

impl From<GateError> for TurnError {
    fn from(source: GateError) -> TurnError {
        TurnError::Gate(source)
    }
}

impl From<ChatError> for TurnError {
    fn from(source: ChatError) -> TurnError {
        TurnError::Chat(source)
    }
}

impl From<tokio::task::JoinError> for TurnError {
    fn from(source: tokio::task::JoinError) -> TurnError {
        TurnError::Join(source)
    }
}

impl From<serde_json::Error> for TurnError {
    fn from(source: serde_json::Error) -> TurnError {
        TurnError::Args(source)
    }
}

const ASK_QUEUE_LIMIT: usize = 8;

static QUEUED_COUNTER: AtomicU64 = AtomicU64::new(0);

struct QueuedAsk {
    id: String,
    text: String,
    enhance: bool,
    images: Vec<crate::attachment::ImageAttachment>,
}

/// What `Runner::ask` did with the text.
#[derive(Debug, PartialEq, Eq)]
pub enum AskOutcome {
    Started(String),
    Queued(String),
    Ignored,
    Enhancing(String),
}

/// One running turn's cancel handle.
struct RunningTurn {
    cancel: tokio::sync::watch::Sender<bool>,
    turn_id: String,
}

struct CompactSlot {
    job: Mutex<Option<Arc<CompactJob>>>,
}

struct CompactJob {
    cancel: tokio::sync::watch::Sender<bool>,
    done: tokio::sync::Notify,
    finished: AtomicBool,
}

impl CompactSlot {
    fn new() -> Arc<CompactSlot> {
        Arc::new(CompactSlot {
            job: Mutex::new(None),
        })
    }

    fn is_running(&self) -> bool {
        self.job
            .lock()
            .expect("the compact slot is not poisoned")
            .as_ref()
            .is_some_and(|job| !job.finished.load(Ordering::SeqCst))
    }

    fn current(&self) -> Option<Arc<CompactJob>> {
        self.job
            .lock()
            .expect("the compact slot is not poisoned")
            .clone()
    }

    fn begin(&self) -> (Arc<CompactJob>, bool) {
        let mut slot = self.job.lock().expect("the compact slot is not poisoned");
        if let Some(job) = slot.as_ref() {
            if !job.finished.load(Ordering::SeqCst) {
                return (Arc::clone(job), false);
            }
        }
        let (cancel, _) = tokio::sync::watch::channel(false);
        let job = Arc::new(CompactJob {
            cancel,
            done: tokio::sync::Notify::new(),
            finished: AtomicBool::new(false),
        });
        *slot = Some(Arc::clone(&job));
        (job, true)
    }

    fn finish(&self, job: &Arc<CompactJob>) {
        job.finished.store(true, Ordering::SeqCst);
        job.done.notify_waiters();
        let mut slot = self.job.lock().expect("the compact slot is not poisoned");
        if slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, job))
        {
            *slot = None;
        }
    }

    fn cancel(&self) {
        if let Some(job) = self.current() {
            let _ = job.cancel.send(true);
        }
    }
}

async fn wait_compact(slot: &CompactSlot) {
    let Some(job) = slot.current() else {
        return;
    };
    let notified = job.done.notified();
    if job.finished.load(Ordering::SeqCst) {
        return;
    }
    notified.await;
}

/// What one tool call produced, both for the model and for the proof.
///
/// The summary is the tool result the model reads. The `wrote` path and the
/// `failure` are what the proof event is built from: a write that landed names
/// the file it wrote, and a command that did not succeed names its argv, its
/// exit, and the end of its output.
struct ToolOutcome {
    is_error: bool,
    images: Vec<crate::attachment::ImageAttachment>,
    summary: String,
    wrote: Option<String>,
    failure: Option<ProofFailure>,
}

/// One session's state in the runner.
struct PendingWake {
    ask: String,
    context: String,
    schedule_id: Option<String>,
    silent: bool,
}

enum TurnStart {
    Id(String),
    Busy,
    Spent,
}

fn idle_after_turn_error(session: &Session, turn_id: &str, flight: &Flight, error: &TurnError) {
    let already = session.events().ok().is_some_and(|events| {
        events
            .iter()
            .any(|event| event.turn_id == turn_id && event.kind == EventKind::Result)
    });
    if !already {
        let _ = append_result(session, turn_id, &error.to_string(), "");
    }
    let _ = append_recovered_proof(session, turn_id, &error.to_string());
    flight.clear();
    let _ = set_status(session, Status::Idle);
}

struct Flight {
    phase: Mutex<Option<Phase>>,
    action: Mutex<Option<String>>,
    thoughts: Arc<Mutex<String>>,
    retry_status: crate::chat::RetryStatus,
}

impl Flight {
    fn new() -> Arc<Flight> {
        Arc::new(Flight {
            phase: Mutex::new(None),
            action: Mutex::new(None),
            thoughts: Arc::new(Mutex::new(String::new())),
            retry_status: Arc::new(Mutex::new(None)),
        })
    }

    fn begin_thinking(&self) {
        *self.action.lock().unwrap() = None;
        self.thoughts
            .lock()
            .expect("the thoughts buffer is not poisoned")
            .clear();
        *self.phase.lock().expect("the phase is not poisoned") = Some(Phase::Thinking);
    }

    fn begin_tools(&self) {
        *self.phase.lock().expect("the phase is not poisoned") = Some(Phase::Tool);
        self.thoughts
            .lock()
            .expect("the thoughts buffer is not poisoned")
            .clear();
    }

    fn tool_action(&self, tool: &str) {
        let action = match tool {
            "grep" | "list_dir" | "web_search" => "Searching",
            "read_file" | "web_fetch" | "use_skill" => "Reading",
            "write_file" | "search_replace" => "Editing",
            "run" | "start_task" | "check_task" | "kill_task" => "Running",
            "run_closeout" => "Verifying",
            "spawn_subagent" => "Delegating",
            "schedule" | "cancel_schedule" => "Scheduling",
            "todo" => "Organizing",
            "ask" => "Waiting",
            "finish" => "Finishing",
            "attach_artifact" => "Publishing artifact",
            _ => "Using tool",
        };
        *self.action.lock().unwrap() = Some(action.to_string());
    }

    fn clear(&self) {
        *self.action.lock().unwrap() = None;
        *self.retry_status.lock().unwrap() = None;
        *self.phase.lock().expect("the phase is not poisoned") = None;
        self.thoughts
            .lock()
            .expect("the thoughts buffer is not poisoned")
            .clear();
    }

    fn snapshot(&self) -> (Option<Phase>, Option<String>) {
        let phase = *self.phase.lock().expect("the phase is not poisoned");
        match phase {
            Some(Phase::Thinking) => {
                let text = self
                    .thoughts
                    .lock()
                    .expect("the thoughts buffer is not poisoned")
                    .clone();
                (Some(Phase::Thinking), Some(text))
            }
            Some(Phase::Tool) => (Some(Phase::Tool), None),
            None => (None, None),
        }
    }
}

#[derive(Default)]
struct ChildRun {
    wake: bool,
    settled: bool,
    seq: u64,
    parent_id: String,
}

struct SessionState {
    session: Session,
    tools: Tools,
    turn: Mutex<Option<RunningTurn>>,
    compact: Arc<CompactSlot>,
    enhance: Arc<enhance::EnhanceSlot>,
    pending_wakes: Mutex<VecDeque<PendingWake>>,
    ask_queue: Mutex<VecDeque<QueuedAsk>>,
    flight: Arc<Flight>,
    retiring: AtomicBool,
    turn_gen: AtomicU64,
    turn_idle: tokio::sync::watch::Sender<bool>,
    view_cache: Mutex<ViewCache>,
}

#[derive(Default)]
struct ViewCache {
    projection: Option<CachedView>,
    context: Option<CachedContext>,
}

struct CachedView {
    revision: u64,
    meta: SessionMeta,
    view: view::View,
}

#[derive(PartialEq, Eq)]
struct ContextKey {
    revision: u64,
    system: String,
    without_skills: String,
    skills: String,
    tools: String,
    prompt_tokens: Option<u64>,
    context_length: Option<u64>,
}

struct CachedContext {
    key: ContextKey,
    usage: crate::compact::ContextUsage,
}

/// The turn runner: owns the config, the chat client, and the sessions.
///
/// One task per running turn. A second message while a turn or a compact is
/// running waits in a queue of eight. A waiting permission or question is
/// still refused, and a message to another session starts immediately. Tasks
/// share no lock, so a session waiting on a permission does not block its
/// siblings.
pub struct Runner {
    me: Weak<Runner>,
    config: Config,
    config_path: Option<PathBuf>,
    sessions: Mutex<HashMap<String, Arc<SessionState>>>,
    gh_program: Mutex<PathBuf>,
    pull_looked: Mutex<HashSet<String>>,
    schedule_tx: tokio::sync::mpsc::UnboundedSender<ScheduleWake>,
    clock: Clock,
    child_runs: Mutex<HashMap<String, ChildRun>>,
}

impl Runner {
    /// A runner for the server and model in `config`.
    pub fn new(config: &Config) -> Result<Arc<Runner>, TurnError> {
        Self::assemble(config, None, Clock::system())
    }

    pub fn with_config_file(config: &Config, path: &Path) -> Result<Arc<Runner>, TurnError> {
        Self::assemble(config, Some(path.to_path_buf()), Clock::system())
    }

    pub fn with_clock(config: &Config, clock: Clock) -> Result<Arc<Runner>, TurnError> {
        Self::assemble(config, None, clock)
    }

    fn assemble(
        config: &Config,
        config_path: Option<PathBuf>,
        clock: Clock,
    ) -> Result<Arc<Runner>, TurnError> {
        let (schedule_tx, mut schedule_rx) = tokio::sync::mpsc::unbounded_channel();
        let runner = Arc::new_cyclic(move |me| Runner {
            me: me.clone(),
            config: config.clone(),
            config_path: config_path.clone(),
            sessions: Mutex::new(HashMap::new()),
            gh_program: Mutex::new(PathBuf::from("gh")),
            pull_looked: Mutex::new(HashSet::new()),
            schedule_tx,
            clock,
            child_runs: Mutex::new(HashMap::new()),
        });
        let weak = Arc::downgrade(&runner);
        tokio::spawn(async move {
            while let Some(msg) = schedule_rx.recv().await {
                if let Some(runner) = weak.upgrade() {
                    runner.on_schedule_due(msg);
                }
            }
        });
        Ok(runner)
    }

    fn config_for_turn(&self) -> Config {
        let Some(path) = &self.config_path else {
            return self.config.clone();
        };
        Config::load(path).unwrap_or_else(|_| self.config.clone())
    }

    pub fn current_config(&self) -> Config {
        self.config_for_turn()
    }

    fn config_for_session(&self, session: &Session) -> Result<Config, TurnError> {
        let mut config = self.config_for_turn();
        if let Ok(meta) = session.meta() {
            if let Some(selection) = meta.model_override {
                if let Some(provider) = selection.provider {
                    config = config.for_provider(&provider).map_err(TurnError::Config)?;
                }
                config.model = selection.model;
                config.effort = selection.effort;
            } else if meta.parent_id.is_some() {
                config.model = meta.model;
                config.effort = meta.effort;
            }
        }
        Ok(config)
    }

    pub fn config_path(&self) -> Option<PathBuf> {
        self.config_path.clone()
    }

    pub fn set_gh_program(&self, program: PathBuf) {
        *self.gh_program.lock().expect("gh program") = program;
    }

    pub async fn list_models(&self) -> Vec<chat::ModelRow> {
        self.model_catalog().await.models
    }

    pub async fn model_catalog(&self) -> chat::ModelCatalog {
        let config = self.config_for_turn();
        let root = self.config_path.as_ref().and_then(|path| path.parent());
        chat::model_catalog(&config, root).await
    }

    /// Add a session to the runner. The session's tools are built against the
    /// workspace its meta names.
    pub fn add_session(&self, session: &Session) -> Result<(), TurnError> {
        let tools = Tools::with_clock(session, self.clock.clone())?;
        let meta = session.meta()?;
        tools.schedules().listen(&meta.id, self.schedule_tx.clone());
        let state = Arc::new(SessionState {
            session: session.clone(),
            tools,
            turn: Mutex::new(None),
            compact: CompactSlot::new(),
            enhance: enhance::EnhanceSlot::new(),
            pending_wakes: Mutex::new(VecDeque::new()),
            ask_queue: Mutex::new(VecDeque::new()),
            flight: Flight::new(),
            retiring: AtomicBool::new(false),
            turn_gen: AtomicU64::new(0),
            turn_idle: tokio::sync::watch::channel(true).0,
            view_cache: Mutex::new(ViewCache::default()),
        });
        let old = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .insert(meta.id.clone(), Arc::clone(&state));
        if let Some(old) = old {
            old.tools.schedules().abort_live();
        }
        state.tools.schedules().arm_pending();
        Ok(())
    }

    /// Reload sessions from disk.
    ///
    /// Every directory in `dirs` that holds a session is read: its meta, with
    /// the allows on it, and its log. A session whose status is `working` but
    /// has no running turn died with the server, so it ends with a result that
    /// says the server stopped. A session with an unanswered permission stays
    /// `waiting`: the card is still on screen and the answer still lands.
    pub fn reload(&self, dirs: &[PathBuf]) -> Result<(), TurnError> {
        for dir in dirs {
            let session = Session::at(dir);
            let Ok(meta) = session.meta() else {
                continue;
            };
            if let Err(error) = session.recover_event_log() {
                eprintln!("Cannot reload session {}: {error}", meta.id);
                self.forget(&meta.id);
                continue;
            }
            if meta
                .goal
                .as_ref()
                .is_some_and(|goal| goal.status == crate::goal::GoalStatus::Active)
            {
                session.update(|meta| {
                    if let Some(goal) = &mut meta.goal {
                        goal.status = crate::goal::GoalStatus::Paused;
                        goal.verification =
                            "The server stopped. Use /goal resume to continue.".to_string();
                    }
                    true
                })?;
            }
            if meta.status == Status::Working {
                let events = session.events()?;
                if enhance::rewrite_in_flight(&events) {
                    session.update(|meta| {
                        meta.status = Status::Idle;
                        true
                    })?;
                } else {
                    // The turn died with the server. End it with a result.
                    let turn_id = events
                        .iter()
                        .rev()
                        .find(|event| event.kind == EventKind::UserAsk)
                        .map(|event| event.turn_id.clone())
                        .unwrap_or_else(|| next_turn_id(&session));
                    let result = Event::new(
                        &session.next_event_id()?,
                        &now(),
                        &turn_id,
                        EventKind::Result,
                    )
                    .with_body(&ResultBody {
                        text: "The server stopped.".into(),
                        note: String::new(),
                    })?;
                    session.append(&result)?;
                    append_recovered_proof(&session, &turn_id, "The server stopped.")?;
                    session.update(|meta| {
                        meta.status = Status::Idle;
                        true
                    })?;
                }
            }
            let tools = Tools::with_clock(&session, self.clock.clone())?;
            tools.schedules().listen(&meta.id, self.schedule_tx.clone());
            let state = Arc::new(SessionState {
                session: session.clone(),
                tools,
                turn: Mutex::new(None),
                compact: CompactSlot::new(),
                enhance: enhance::EnhanceSlot::new(),
                pending_wakes: Mutex::new(VecDeque::new()),
                ask_queue: Mutex::new(VecDeque::new()),
                flight: Flight::new(),
                retiring: AtomicBool::new(false),
                turn_gen: AtomicU64::new(0),
                turn_idle: tokio::sync::watch::channel(true).0,
                view_cache: Mutex::new(ViewCache::default()),
            });
            // An unanswered permission on disk is still open: the card is on
            // screen and the answer still lands, so the gate has to know.
            if let Some(id) = open_permission_id(&session) {
                state.tools.gate().hold_open(&id);
            } else if let Some(id) = open_question_id(&session) {
                state.tools.gate().hold_open_question(&id);
            }
            let yolo_open = meta.yolo && state.tools.gate().open_permission().is_some();
            let session_id = meta.id.clone();
            let old = self
                .sessions
                .lock()
                .expect("the session map is not poisoned")
                .insert(session_id.clone(), Arc::clone(&state));
            if let Some(old) = old {
                old.tools.schedules().abort_live();
            }
            if yolo_open {
                let _ = self.answer(&session_id, Answer::allow_once());
            }
            let _ = state.tools.tasks().settle_orphans();
            if !meta.archived {
                state.tools.schedules().arm_pending();
            }
        }
        Ok(())
    }

    /// Start a turn when the session is free. A live turn or a compact queues a
    /// non-empty ask, up to eight. An empty ask is ignored while that slot is
    /// taken. A waiting permission or question is refused.
    pub fn ask(&self, session_id: &str, text: &str) -> Result<AskOutcome, TurnError> {
        self.ask_with(session_id, text, None)
    }

    pub fn ask_with(
        &self,
        session_id: &str,
        text: &str,
        enhance: Option<bool>,
    ) -> Result<AskOutcome, TurnError> {
        self.ask_with_images(session_id, text, enhance, Vec::new())
    }

    pub fn ask_with_images(
        &self,
        session_id: &str,
        text: &str,
        enhance: Option<bool>,
        images: Vec<crate::attachment::ImageAttachment>,
    ) -> Result<AskOutcome, TurnError> {
        let state = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
            .ok_or(TurnError::NoSession)?;
        if state.session.meta().is_ok_and(|meta| meta.archived) {
            return Err(TurnError::Archived);
        }
        if let Some(command) = text
            .trim()
            .strip_prefix("/goal")
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            return self.goal_command(&state, command.trim(), images);
        }
        if self.waiting(&state) || self.enhance_open(&state) || self.enhance_running(&state) {
            return Err(TurnError::Busy);
        }
        let on = images.is_empty() && self.should_enhance(&state, text, enhance);
        if self.occupied(&state) {
            if text.trim().is_empty() && images.is_empty() {
                return Ok(AskOutcome::Ignored);
            }
            let id = self.enqueue(&state, text, on, images)?;
            let runner = self.me.upgrade().expect("the runner is still held");
            runner.drain_asks(&state);
            return Ok(AskOutcome::Queued(id));
        }
        if on {
            let id = self.launch_enhance(&state, text, None)?;
            return Ok(AskOutcome::Enhancing(id));
        }
        match self.start_turn(&state, text, "", None, false, images)? {
            TurnStart::Id(turn_id) => Ok(AskOutcome::Started(turn_id)),
            TurnStart::Busy | TurnStart::Spent => Err(TurnError::Busy),
        }
    }

    fn should_enhance(&self, state: &SessionState, text: &str, requested: Option<bool>) -> bool {
        if skills::slash_ask(text, state.tools.workspace())
            .skill_body
            .is_some()
        {
            return false;
        }
        match requested {
            Some(value) => value,
            None => state
                .session
                .meta()
                .map(|meta| meta.enhance)
                .unwrap_or(false),
        }
    }

    fn waiting(&self, state: &SessionState) -> bool {
        state.tools.gate().open_permission().is_some()
            || state.tools.gate().open_question().is_some()
    }

    fn occupied(&self, state: &SessionState) -> bool {
        state
            .turn
            .lock()
            .expect("the turn slot is not poisoned")
            .is_some()
            || state.compact.is_running()
    }

    fn enqueue(
        &self,
        state: &SessionState,
        text: &str,
        enhance: bool,
        images: Vec<crate::attachment::ImageAttachment>,
    ) -> Result<String, TurnError> {
        let mut queue = state
            .ask_queue
            .lock()
            .expect("the ask queue is not poisoned");
        if queue.len() >= ASK_QUEUE_LIMIT {
            return Err(TurnError::QueueFull);
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let count = QUEUED_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("{nanos:x}-{count:x}");
        queue.push_back(QueuedAsk {
            id: id.clone(),
            text: text.to_string(),
            enhance,
            images,
        });
        Ok(id)
    }

    pub fn remove_queued(&self, session_id: &str, id: &str) -> Result<bool, TurnError> {
        let state = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
            .ok_or(TurnError::NoSession)?;
        let mut queue = state
            .ask_queue
            .lock()
            .expect("the ask queue is not poisoned");
        let Some(index) = queue.iter().position(|queued| queued.id == id) else {
            return Ok(false);
        };
        queue.remove(index);
        Ok(true)
    }

    fn drain_asks(self: &Arc<Self>, state: &Arc<SessionState>) {
        if self.waiting(state)
            || self.occupied(state)
            || self.enhance_open(state)
            || self.enhance_running(state)
        {
            return;
        }
        let queued = {
            let mut queue = state
                .ask_queue
                .lock()
                .expect("the ask queue is not poisoned");
            queue.pop_front()
        };
        let Some(queued) = queued else {
            return;
        };
        let failed = if queued.enhance {
            self.launch_enhance(state, &queued.text, None).is_err()
        } else {
            !matches!(
                self.start_turn(state, &queued.text, "", None, false, queued.images.clone()),
                Ok(TurnStart::Id(_))
            )
        };
        if failed {
            state
                .ask_queue
                .lock()
                .expect("the ask queue is not poisoned")
                .push_front(queued);
        }
    }

    fn after_idle(self: &Arc<Self>, state: &Arc<SessionState>) {
        self.drain_asks(state);
        self.flush_wake(state);
    }

    fn start_turn(
        &self,
        state: &Arc<SessionState>,
        text: &str,
        context: &str,
        schedule_id: Option<&str>,
        silent: bool,
        images: Vec<crate::attachment::ImageAttachment>,
    ) -> Result<TurnStart, TurnError> {
        let runner = self.me.upgrade().expect("the runner is still held");
        let mut turn = state.turn.lock().expect("the turn slot is not poisoned");
        if state.retiring.load(Ordering::Acquire)
            || turn.is_some()
            || state.tools.gate().open_permission().is_some()
            || state.tools.gate().open_question().is_some()
            || self.enhance_open(state)
            || self.enhance_running(state)
        {
            return Ok(TurnStart::Busy);
        }

        // The turn id is counted under the turn slot, so two asks cannot share
        // one even if the first task has not written its ask to the log yet.
        let turn_id = next_turn_id(&state.session);
        state.tools.gate().reset_cancel();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let session = state.session.clone();
        let tools = state.tools.clone();
        let config = self.config_for_session(&state.session)?;
        let root = self
            .config_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf);
        let client = ChatClient::in_root(&config, root.as_deref())?;
        let compact_percent = config.compact_percent;
        let prefire_percent = config.prefire_percent;
        let workspace = state.tools.workspace().to_path_buf();
        let meta = state.session.meta().ok();
        let profile = meta.as_ref().and_then(|meta| meta.profile.clone());
        let allowance = config.skill_allowance(profile.as_deref());
        let skills = skills::keep_allowed(skills::index(&workspace), allowance.as_deref());
        let slash = skills::slash_ask_allowed(text, &workspace, allowance.as_deref());
        let closeout_file = config
            .closeout_for(
                &workspace,
                meta.as_ref()
                    .and_then(|meta| meta.requested_workspace.as_deref()),
            )
            .ok()
            .flatten();
        let context_length = meta.as_ref().and_then(|meta| meta.context_length);
        let child = meta.as_ref().is_some_and(|meta| meta.parent_id.is_some());
        let prompt_closeout = closeout_file;
        let skill = match slash.refused {
            Some(error) => error,
            None => slash.skill_body.unwrap_or_default(),
        };
        let text = slash.user_text;
        let context = context.to_string();
        let compact = Arc::clone(&state.compact);

        let done = Arc::clone(state);
        let turn_ctx = Turn {
            session,
            tools,
            client,
            config,
            compact: Arc::clone(&compact),
            prompt_skills: skills,
            prompt_closeout,
            context_length,
            text,
            images,
            skill,
            context,
            silent,
            turn_id: turn_id.clone(),
            compact_percent,
            prefire_percent,
            flight: Arc::clone(&state.flight),
            runner: Arc::clone(&runner),
            state: Arc::clone(state),
            root,
            child,
            goal_run: meta
                .as_ref()
                .and_then(|meta| meta.goal.as_ref())
                .is_some_and(|goal| goal.status == crate::goal::GoalStatus::Active),
        };
        if let Some(id) = schedule_id {
            if !state.tools.schedules().fire(id) {
                return Ok(TurnStart::Spent);
            }
        }
        let generation = state.turn_gen.fetch_add(1, Ordering::AcqRel) + 1;
        state.turn_idle.send_replace(false);
        tokio::spawn(async move {
            wait_compact(&compact).await;
            if let Err(error) = run_turn(&turn_ctx, cancel_rx).await {
                idle_after_turn_error(
                    &turn_ctx.session,
                    &turn_ctx.turn_id,
                    &turn_ctx.flight,
                    &error,
                );
            }
            *done.turn.lock().expect("the turn slot is not poisoned") = None;
            runner.settle_child(&done);
            if !done.retiring.load(Ordering::Acquire) {
                runner.after_idle(&done);
            }
            let turn = done.turn.lock().expect("the turn slot is not poisoned");
            if turn.is_none() && done.turn_gen.load(Ordering::Acquire) == generation {
                done.turn_idle.send_replace(true);
            }
        });

        *turn = Some(RunningTurn {
            cancel: cancel_tx,
            turn_id: turn_id.clone(),
        });
        Ok(TurnStart::Id(turn_id))
    }

    pub fn is_compacting(&self, session_id: &str) -> bool {
        self.sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .is_some_and(|state| state.compact.is_running())
    }

    pub fn compact(&self, session_id: &str) -> Result<(), TurnError> {
        let state = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
            .ok_or(TurnError::NoSession)?;
        let config = self.config_for_session(&state.session)?;
        let root = self.config_path.as_ref().and_then(|path| path.parent());
        let client = ChatClient::in_root(&config, root)?;
        let runner = self.me.upgrade().expect("the runner is still held");
        let done = Arc::clone(&state);
        let preserve_turn = state
            .turn
            .lock()
            .expect("the turn slot is not poisoned")
            .as_ref()
            .map(|turn| turn.turn_id.clone());
        spawn_compact(
            state.session.clone(),
            Arc::clone(&state.compact),
            client,
            config,
            preserve_turn,
            move || runner.after_idle(&done),
        );
        Ok(())
    }

    /// Cancel the running turn on a session. The turn stops at the next tool
    /// boundary; a running command is killed. Other sessions keep running.
    pub fn cancel(&self, session_id: &str) {
        let Some(state) = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
        else {
            return;
        };
        let turn = state.turn.lock().expect("the turn slot is not poisoned");
        if let Some(running) = turn.as_ref() {
            running.cancel.send_replace(true);
            state.tools.gate().cancel();
        }
        drop(turn);
        state.compact.cancel();
        state.enhance.cancel();
        state.tools.tasks().cancel_all();
    }

    pub async fn finish_for_delete(&self, session_id: &str) {
        let state = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned();
        let Some(state) = state else {
            return;
        };
        state.retiring.store(true, Ordering::Release);
        self.cancel(session_id);
        let mut idle = state.turn_idle.subscribe();
        loop {
            if *idle.borrow() {
                break;
            }
            if idle.changed().await.is_err() {
                break;
            }
        }
        state.tools.tasks().wait_idle().await;
    }

    /// Stop a live turn and its schedules without removing the session. Archive
    /// uses this so a restored session can take a new ask.
    pub async fn retire(&self, session_id: &str) {
        self.finish_for_delete(session_id).await;
        if let Some(state) = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
        {
            state.tools.schedules().abort_live();
            state.tools.tasks().cancel_all();
            state.retiring.store(false, Ordering::Release);
        }
    }

    /// Arm schedules again after a session is restored.
    pub fn resume(&self, session_id: &str) {
        let Some(state) = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
        else {
            return;
        };
        state.retiring.store(false, Ordering::Release);
        state.tools.schedules().arm_pending();
    }

    pub fn forget(&self, session_id: &str) {
        self.cancel(session_id);
        let removed = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .remove(session_id);
        if let Some(state) = removed {
            state.tools.schedules().abort_live();
            state.tools.tasks().cancel_all();
        }
        self.child_runs
            .lock()
            .expect("child runs")
            .remove(session_id);
    }

    pub fn reload_tools(&self, session_id: &str) -> Result<(), TurnError> {
        let state = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
            .ok_or(TurnError::NoSession)?;
        let busy = state
            .turn
            .lock()
            .expect("the turn slot is not poisoned")
            .is_some()
            || state.compact.is_running();
        if busy {
            return Ok(());
        }
        self.add_session(&state.session)
    }

    /// The model every session of this runner talks to.
    pub fn model(&self) -> String {
        self.config_for_turn().model
    }

    pub fn effort(&self) -> Option<String> {
        self.config_for_turn().effort
    }

    /// Answer every open card, so a turn the server is shutting down does not
    /// stay blocked on a permission nobody is going to answer. The turn then
    /// finishes on its own: the tool runs, the model is called once more, and
    /// the session goes back to idle.
    pub fn release_all(&self) {
        let sessions = self
            .sessions
            .lock()
            .expect("the session map is not poisoned");
        let open: Vec<(String, &'static str)> = sessions
            .iter()
            .map(|(id, state)| {
                let gate = state.tools.gate();
                if gate.open_permission().is_some() {
                    (id.clone(), "permission")
                } else if gate.open_question().is_some() {
                    (id.clone(), "question")
                } else {
                    (String::new(), "")
                }
            })
            .filter(|(_, kind)| !kind.is_empty())
            .collect();
        drop(sessions);
        for (id, kind) in open {
            match kind {
                "permission" => {
                    let _ = self.answer(&id, Answer::allow_once());
                }
                _ => {
                    let _ = self.answer_question(&id, "");
                }
            }
        }
    }

    /// Answer the open permission on a session.
    ///
    /// A permission the reload held open has no turn waiting on it, because the
    /// turn that asked it died with the old server. The answer still settles
    /// the card: the event goes to the log and the session goes back to idle.
    pub fn answer(&self, session_id: &str, answer: Answer) -> Result<(), AnswerError> {
        let state = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
            .ok_or(AnswerError::NothingOpen)?;
        let held = state.tools.gate().is_held_open();
        state.tools.gate().answer(answer.clone())?;
        if held {
            settle_held_answer(&state.session, &answer)?;
        }
        Ok(())
    }

    /// Answer the open question on a session.
    pub fn answer_question(&self, session_id: &str, text: &str) -> Result<(), AnswerError> {
        let state = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
            .ok_or(AnswerError::NothingOpen)?;
        let held = state.tools.gate().is_held_open_question();
        state.tools.gate().answer_question(text.to_string())?;
        if held {
            settle_held_question(&state.session, text)?;
        }
        Ok(())
    }

    /// The view of a session: its status, its cards, and its revision.
    pub fn view(&self, session_id: &str) -> Result<view::View, SessionError> {
        let state = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
            .ok_or(SessionError::MissingMeta {
                path: PathBuf::from(session_id),
            })?;
        let now = state.tools.schedules().clock().now_millis();
        let meta = state.session.meta()?;
        let (revision, events) = state.session.event_snapshot()?;
        let mut cache = state
            .view_cache
            .lock()
            .expect("the view cache is not poisoned");
        let mut view = match cache.projection.as_ref() {
            Some(cached) if cached.revision == revision && cached.meta == meta => {
                cached.view.clone()
            }
            _ => {
                let view = view::project_events(meta.clone(), &events, now);
                cache.projection = Some(CachedView {
                    revision,
                    meta: meta.clone(),
                    view: view.clone(),
                });
                view
            }
        };
        view.schedules = view::pending_schedules(&events, now);
        let config = self.config_for_turn();
        view.closeout = view::closeout_rows_for(
            config
                .closeout_for(state.tools.workspace(), meta.requested_workspace.as_deref())
                .ok()
                .flatten(),
            &events,
            meta.show_closeout,
        );
        let allowance = config.skill_allowance(meta.profile.as_deref());
        view.skills =
            skills::keep_allowed(skills::index(state.tools.workspace()), allowance.as_deref());
        view.context = session_context(
            &state,
            &view.skills,
            &config,
            &meta,
            &events,
            revision,
            &mut cache.context,
        );
        let (phase, thinking) = state.flight.snapshot();
        view.phase = phase;
        view.action = state.flight.action.lock().unwrap().clone().or(view.action);
        view.thinking = thinking;
        view.retry_status = state.flight.retry_status.lock().unwrap().clone();
        let queue = state
            .ask_queue
            .lock()
            .expect("the ask queue is not poisoned");
        view.queue = queue.iter().map(|queued| queued.text.clone()).collect();
        view.queue_items = queue
            .iter()
            .map(|queued| view::QueuedMessage {
                id: queued.id.clone(),
                text: queued.text.clone(),
                image_count: queued.images.len(),
                enhance: queued.enhance,
            })
            .collect();
        Ok(view)
    }

    pub fn task(&self, session_id: &str, task_id: &str) -> Result<task::TaskView, String> {
        let state = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
            .ok_or_else(|| "no such session".to_string())?;
        state.tools.tasks().check(task_id)
    }

    fn on_schedule_due(&self, msg: ScheduleWake) {
        let Some(state) = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(&msg.session_id)
            .cloned()
        else {
            return;
        };
        self.queue_or_start(&state, msg.note, String::new(), Some(msg.id), false);
    }

    fn queue_or_start(
        &self,
        state: &Arc<SessionState>,
        ask: String,
        context: String,
        schedule_id: Option<String>,
        silent: bool,
    ) {
        match self.start_turn(
            state,
            &ask,
            &context,
            schedule_id.as_deref(),
            silent,
            Vec::new(),
        ) {
            Ok(TurnStart::Id(_)) | Ok(TurnStart::Spent) => {}
            Ok(TurnStart::Busy) => {
                state
                    .pending_wakes
                    .lock()
                    .expect("the wake queue is not poisoned")
                    .push_back(PendingWake {
                        ask,
                        context,
                        schedule_id,
                        silent,
                    });
            }
            Err(_) => {
                let still = schedule_id
                    .as_ref()
                    .is_some_and(|id| state.tools.schedules().is_pending(id));
                if still {
                    state
                        .pending_wakes
                        .lock()
                        .expect("the wake queue is not poisoned")
                        .push_back(PendingWake {
                            ask,
                            context,
                            schedule_id,
                            silent,
                        });
                }
            }
        }
    }

    fn flush_wake(self: &Arc<Self>, state: &Arc<SessionState>) {
        let pending = state
            .pending_wakes
            .lock()
            .expect("the wake queue is not poisoned")
            .pop_front();
        let Some(wake) = pending else {
            return;
        };
        self.queue_or_start(state, wake.ask, wake.context, wake.schedule_id, wake.silent);
    }

    pub fn maybe_fill_pull(&self, session_id: &str) {
        let Some(state) = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
        else {
            return;
        };
        let Ok(meta) = state.session.meta() else {
            return;
        };
        if meta.status != Status::Idle || meta.pull_url.is_some() {
            return;
        }
        let workspace = PathBuf::from(&meta.workspace);
        if !workspace.join(".git").exists() {
            return;
        }
        {
            let mut looked = self.pull_looked.lock().expect("pull looked");
            if !looked.insert(meta.id.clone()) {
                return;
            }
        }
        let session = state.session.clone();
        let gh = self.gh_program.lock().expect("gh program").clone();
        tokio::spawn(async move {
            fill_pull_url(&session, &workspace, &gh).await;
        });
    }
}

async fn fill_pull_url(session: &Session, workspace: &Path, gh: &Path) {
    let Ok(Ok(output)) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::process::Command::new(gh)
            .args(["pr", "view", "--json", "url"])
            .current_dir(workspace)
            .output(),
    )
    .await
    else {
        return;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let Some(url) = crate::session::url_from_pr_view(&stdout)
        .or_else(|| crate::session::github_pull_url(&stdout))
        .or_else(|| crate::session::github_pull_url(&stderr))
    else {
        return;
    };
    if let Ok(meta) = session.meta() {
        if meta.status == Status::Idle && meta.pull_url.is_none() {
            let _ = session.set_pull_url(&url);
        }
    }
}

fn session_context(
    state: &SessionState,
    skills: &[prompt::SkillEntry],
    config: &Config,
    meta: &SessionMeta,
    events: &[Event],
    revision: u64,
    cache: &mut Option<CachedContext>,
) -> Option<crate::compact::ContextUsage> {
    let workspace = state.tools.workspace();
    let closeout = config
        .closeout_for(workspace, meta.requested_workspace.as_deref())
        .ok()
        .flatten();
    let prompt_closeout = closeout.as_ref();
    let agents = crate::agents_doc::load(workspace);
    let mut parts = prompt::prompt_parts(
        &workspace.to_string_lossy(),
        skills,
        prompt_closeout,
        &agents,
        meta.context_length,
    );
    if !config.tool_allowed(meta.profile.as_deref(), "attach_artifact") {
        parts.full = parts.full.replace(prompt::ARTIFACT_LINE, "");
        parts.without_skills = parts.without_skills.replace(prompt::ARTIFACT_LINE, "");
    }
    let tools_json = serde_json::to_string(&tool_definitions_for(
        config,
        meta.parent_id.is_some(),
        meta.profile.as_deref(),
    ))
    .unwrap_or_default();
    let key = ContextKey {
        revision,
        system: parts.full,
        without_skills: parts.without_skills,
        skills: parts.skills,
        tools: tools_json,
        prompt_tokens: meta.prompt_tokens,
        context_length: meta.context_length,
    };
    if let Some(cached) = cache.as_ref().filter(|cached| cached.key == key) {
        return Some(cached.usage.clone());
    }
    let messages =
        crate::compact::meter_messages(&key.system, events, &workspace.to_string_lossy());
    let usage = crate::compact::context_usage(
        &key.without_skills,
        &key.skills,
        &key.tools,
        &messages,
        meta.prompt_tokens,
        meta.context_length,
    );
    *cache = Some(CachedContext {
        key,
        usage: usage.clone(),
    });
    Some(usage)
}

/// One turn's everything: the session it writes to, the tools it runs, the
/// client it asks, and what it runs for. Bundled so the turn loop takes one
/// argument, not eight.
struct Turn {
    session: Session,
    tools: Tools,
    client: ChatClient,
    config: Config,
    compact: Arc<CompactSlot>,
    prompt_skills: Vec<prompt::SkillEntry>,
    prompt_closeout: Option<crate::closeout::CloseoutFile>,
    context_length: Option<u64>,
    text: String,
    images: Vec<crate::attachment::ImageAttachment>,
    skill: String,
    context: String,
    silent: bool,
    turn_id: String,
    compact_percent: u32,
    prefire_percent: u32,
    flight: Arc<Flight>,
    runner: Arc<Runner>,
    state: Arc<SessionState>,
    root: Option<PathBuf>,
    child: bool,
    goal_run: bool,
}

impl Turn {
    fn system_prompt(&self) -> String {
        let workspace = self.tools.workspace();
        let agents = crate::agents_doc::load(workspace);
        let build = if self.child {
            prompt::subagent_prompt
        } else {
            prompt::system_prompt
        };
        let text = build(
            &workspace.to_string_lossy(),
            &self.prompt_skills,
            self.prompt_closeout.as_ref(),
            &agents,
            self.context_length,
        );
        let profile = self.session.meta().ok().and_then(|meta| meta.profile);
        if self
            .config
            .tool_allowed(profile.as_deref(), "attach_artifact")
        {
            text
        } else {
            text.replace(prompt::ARTIFACT_LINE, "")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tool_definitions;
    use crate::config::Config;

    fn names(child: bool) -> Vec<String> {
        tool_definitions(&Config::default(), child)
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    #[test]
    fn a_child_tool_list_drops_ask_and_spawn_subagent() {
        let child = names(true);
        assert!(child.iter().any(|name| name == "finish"));
        assert!(child.iter().any(|name| name == "read_file"));
        assert!(!child.iter().any(|name| name == "ask"));
        assert!(!child.iter().any(|name| name == "spawn_subagent"));
        let parent = names(false);
        assert!(parent.iter().any(|name| name == "ask"));
        assert!(parent.iter().any(|name| name == "spawn_subagent"));
        assert!(parent.iter().any(|name| name == "finish"));
        assert!(parent.iter().any(|name| name == "read_file"));
    }

    #[test]
    fn every_known_tool_has_an_action_and_thinking_clears_it() {
        let expected = [
            ("read_file", "Reading"),
            ("web_fetch", "Reading"),
            ("web_search", "Searching"),
            ("grep", "Searching"),
            ("list_dir", "Searching"),
            ("search_replace", "Editing"),
            ("write_file", "Editing"),
            ("run", "Running"),
            ("start_task", "Running"),
            ("check_task", "Running"),
            ("kill_task", "Running"),
            ("spawn_subagent", "Delegating"),
            ("schedule", "Scheduling"),
            ("cancel_schedule", "Scheduling"),
            ("run_closeout", "Verifying"),
            ("ask", "Waiting"),
            ("finish", "Finishing"),
            ("attach_artifact", "Publishing artifact"),
            ("use_skill", "Reading"),
            ("todo", "Organizing"),
        ];
        assert_eq!(expected.len(), super::known_tool_names().len());
        let flight = super::Flight::new();
        for (tool, label) in expected {
            assert!(super::known_tool_names().contains(&tool));
            flight.begin_tools();
            flight.tool_action(tool);
            assert_eq!(flight.action.lock().unwrap().as_deref(), Some(label));
            flight.begin_thinking();
            assert!(flight.action.lock().unwrap().is_none());
        }
        flight.tool_action("run");
        flight.clear();
        assert!(flight.action.lock().unwrap().is_none());
    }

    fn profiled(body: &str) -> Config {
        Config::from_toml(&format!("base_url = \"x\"\nmodel = \"m\"\n{body}"))
            .expect("the profile config parses")
    }

    fn offered(config: &Config, child: bool, profile: Option<&str>) -> Vec<String> {
        super::tool_definitions_for(config, child, profile)
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    #[test]
    fn a_review_profile_omits_write_and_run_and_a_missing_tools_key_keeps_run() {
        let every = names(false);
        assert!(every.iter().any(|name| name == "run"));
        assert!(every.iter().any(|name| name == "write_file"));
        assert!(every.iter().any(|name| name == "todo"));
        assert!(every.iter().any(|name| name == "use_skill"));
        let config = profiled(
            r#"
[profiles.review]
tools = ["read_file", "grep", "list_dir", "web_fetch", "ask", "finish"]

[profiles.planning]
tools = ["read_file", "grep", "list_dir", "web_fetch", "ask", "finish", "todo", "use_skill", "spawn_subagent"]

[profiles.open]
skills = ["skill-a"]
"#,
        );
        let review = offered(&config, false, Some("review"));
        assert!(!review.iter().any(|name| name == "write_file"));
        assert!(!review.iter().any(|name| name == "run"));
        assert!(review.iter().any(|name| name == "ask"));
        assert!(review.iter().any(|name| name == "finish"));
        let planning = offered(&config, false, Some("planning"));
        assert!(planning.iter().any(|name| name == "todo"));
        assert!(planning.iter().any(|name| name == "use_skill"));
        assert!(!planning.iter().any(|name| name == "write_file"));
        assert!(!planning.iter().any(|name| name == "run"));
        let open = offered(&config, false, Some("open"));
        assert!(open.iter().any(|name| name == "run"));
        assert!(open.iter().any(|name| name == "write_file"));
        let child = offered(&config, true, Some("planning"));
        assert!(!child.iter().any(|name| name == "ask"));
        assert!(!child.iter().any(|name| name == "spawn_subagent"));
        assert!(child.iter().any(|name| name == "todo"));
        assert!(child.iter().any(|name| name == "finish"));
        assert_eq!(offered(&config, false, None), every);
    }

    #[test]
    fn a_turn_error_is_a_result_and_the_session_goes_idle() {
        let root = std::env::temp_dir().join(format!("kyotoagent-turn-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let workspace = root.join("w");
        std::fs::create_dir_all(&workspace).expect("the workspace exists");
        let session = crate::session::Session::at(&root.join("s"));
        session
            .create(&crate::session::SessionMeta::new(
                "s",
                &workspace,
                "test/model",
                "2026-09-29T00:00:00.000Z",
            ))
            .expect("the session is created");
        super::set_status(&session, crate::screen::Status::Working).expect("status is working");
        let source = serde_json::from_str::<serde_json::Value>("").expect_err("empty is not json");
        let error = super::TurnError::Args(source);
        let flight = super::Flight::new();
        flight.begin_thinking();
        super::idle_after_turn_error(&session, "t1", &flight, &error);
        assert_eq!(
            session.meta().expect("meta reads").status,
            crate::screen::Status::Idle
        );
        let events = session.events().expect("the log reads");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, crate::events::EventKind::Result);
        assert_eq!(events[0].body["text"], error.to_string());
        assert!(events[0].body["text"]
            .as_str()
            .unwrap()
            .contains("the tool arguments were not JSON"));
        assert!(flight.snapshot().0.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_turn_error_leaves_an_existing_result_in_place() {
        let root =
            std::env::temp_dir().join(format!("kyotoagent-turn-err-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let workspace = root.join("w");
        std::fs::create_dir_all(&workspace).expect("the workspace exists");
        let session = crate::session::Session::at(&root.join("s"));
        session
            .create(&crate::session::SessionMeta::new(
                "s",
                &workspace,
                "test/model",
                "2026-09-29T00:00:00.000Z",
            ))
            .expect("the session is created");
        super::set_status(&session, crate::screen::Status::Working).expect("status is working");
        super::append_result(&session, "t1", "kept", "").expect("the result lands");
        let source = serde_json::from_str::<serde_json::Value>("{").expect_err("truncated");
        let error = super::TurnError::Args(source);
        let flight = super::Flight::new();
        super::idle_after_turn_error(&session, "t1", &flight, &error);
        let events = session.events().expect("the log reads");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].body["text"], "kept");
        assert!(crate::proof::versions(&events).unwrap().is_empty());
        super::idle_after_turn_error(&session, "t1", &flight, &error);
        assert_eq!(session.events().unwrap().len(), 1);
        assert_eq!(
            session.meta().expect("meta reads").status,
            crate::screen::Status::Idle
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bad_arguments_echo_the_raw_string_and_cap_it() {
        let raw = "{\"cmd\":\"echo hello";
        let source = serde_json::from_str::<serde_json::Value>(raw).expect_err("truncated json");
        let error = super::TurnError::Args(source);
        let text = super::bad_arguments_result(&error, raw);
        assert!(text.starts_with("the tool arguments were not JSON:"));
        assert!(text.contains("Your original arguments:"));
        assert!(text.contains(raw));
        assert!(text.contains("Please fix the syntax and retry."));
        let empty = super::bad_arguments_result(&error, "");
        assert!(empty.starts_with("the tool arguments were not JSON:"));
        assert!(!empty.contains("Your original arguments"));
        let long = "a".repeat(2_001);
        let capped = super::bad_arguments_result(&error, &long);
        assert!(capped.contains(&"a".repeat(2_000)));
        assert!(!capped.contains(&"a".repeat(2_001)));
        let mut boundary = "a".repeat(1_999);
        boundary.push('é');
        boundary.push('z');
        let cut = super::bad_arguments_result(&error, &boundary);
        assert!(cut.contains(&"a".repeat(1_999)));
        assert!(!cut.contains('é'));
        assert!(!cut.contains('z'));
    }
}
