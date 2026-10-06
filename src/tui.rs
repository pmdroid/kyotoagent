use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Stdout, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::Terminal;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::chat::ModelRow;
use crate::config::EFFORTS;
use crate::events::TodoItem;
use crate::prompt::SkillEntry;
use crate::screen::{
    self, Bottom, Card, Choice, Drag, ItemKind, ItemRun, Outcome, Overlay, RightPane, ScreenModel,
    SelectPoint, SessionRow, SkillPicker, SkillPickerRow, Status, TextSelect, Wait, LIST_WIDTH,
    TODOS_WIDTH,
};
use crate::server;
use crate::skills;
use crate::view::{self, CardKind};

mod apply;
mod catalog;
mod connections;
mod deletion;
mod images;
mod key;
mod layout;
mod model;
mod mouse;
mod poll;
mod projects;
mod proof;
mod proof_files;
mod providers;
mod queue;
mod servers;

pub use apply::*;
pub async fn advance_requests(app: &mut App, client: &Client) {
    advance_notice(app, Instant::now());
    proof_files::advance(app).await;
    layout::advance(app).await;
    deletion::advance(app, client).await;
    projects::advance(app, client).await;
    catalog::advance(app).await;
}
pub use key::*;
pub use model::*;
pub use mouse::*;
pub use poll::*;

#[cfg(test)]
mod tests;

pub const WORKSPACE_QUESTION: &str = "Where should this session work?";
pub const WORKSPACE_HERE: &str = "This directory";
pub const WORKSPACE_WORKTREE: &str = "A new git worktree";
pub const PROFILE_QUESTION: &str = "Which profile?";
pub const PROFILE_EVERYTHING: &str = "Everything";

pub fn worktree_choice(line: &str) -> bool {
    let line = line.trim();
    line == "2" || line == WORKSPACE_WORKTREE
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
struct ListedProject {
    #[serde(default)]
    id: String,
    name: String,
    path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkspaceStep {
    Off,
    Projects(Vec<ListedProject>),
    AllProjects(Vec<servers::Project>),
    Worktree(PathBuf),
}

fn workspace_open(step: &WorkspaceStep) -> bool {
    !matches!(step, WorkspaceStep::Off)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingSession {
    workspace: PathBuf,
    worktree: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProfileStep {
    names: Vec<String>,
    pending: Option<PendingSession>,
}

fn profile_open(step: &Option<ProfileStep>) -> bool {
    step.is_some()
}

pub(super) fn choosing(app: &App) -> bool {
    workspace_open(&app.workspace_step)
        || profile_open(&app.profile_step)
        || app.server_step.is_some()
        || app.proof_file_popup.is_some()
        || app.proof_popup.is_some()
        || app.project_popup.is_some()
        || app.provider_popup.is_some()
        || app.server_popup.is_some()
}

fn choice_row(label: &str) -> Choice {
    Choice {
        label: label.to_string(),
        marked: false,
    }
}

fn question_overlay(choices: Vec<Choice>) -> Overlay {
    Overlay::Question {
        text: WORKSPACE_QUESTION.to_string(),
        choices,
        prompt: String::new(),
    }
}

fn workspace_overlay(
    step: &WorkspaceStep,
    remote: bool,
    server_names: &BTreeMap<String, String>,
) -> Option<Overlay> {
    match step {
        WorkspaceStep::Projects(projects) => {
            let mut choices = if remote {
                Vec::new()
            } else {
                vec![choice_row(WORKSPACE_HERE)]
            };
            for project in projects {
                choices.push(choice_row(&project.name));
            }
            choices.push(choice_row("Add project"));
            Some(question_overlay(choices))
        }
        WorkspaceStep::AllProjects(projects) => Some(question_overlay(
            (!remote)
                .then(|| choice_row(WORKSPACE_HERE))
                .into_iter()
                .chain(projects.iter().map(|project| {
                    choice_row(&format!(
                        "{} [{}]",
                        project.row.name,
                        project.server.as_deref().map_or("Local", |id| server_names
                            .get(id)
                            .map_or(id, String::as_str))
                    ))
                }))
                .chain(std::iter::once(choice_row("Add project")))
                .collect(),
        )),
        WorkspaceStep::Worktree(_) => Some(question_overlay(vec![
            choice_row(if remote {
                "Use project directory"
            } else {
                WORKSPACE_HERE
            }),
            choice_row(WORKSPACE_WORKTREE),
        ])),
        WorkspaceStep::Off => None,
    }
}

fn profile_overlay(step: &Option<ProfileStep>) -> Option<Overlay> {
    let step = step.as_ref()?;
    let mut choices = vec![choice_row(PROFILE_EVERYTHING)];
    for name in &step.names {
        choices.push(choice_row(name));
    }
    Some(Overlay::Question {
        text: PROFILE_QUESTION.to_string(),
        choices,
        prompt: String::new(),
    })
}

#[derive(Clone, Debug)]
pub struct PastedInput {
    start: usize,
    text: String,
    question: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Working,
    Permission,
    Question { choices: usize },
    QuestionText,
    Enhance { retry: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Exit,
    SelectNext,
    SelectPrev,
    SelectSession(String),
    ScrollUp,
    ScrollDown,
    ScrollList { up: bool },
    PageUp,
    PageDown,
    Type(char),
    Paste(String),
    Backspace,
    DeleteWord,
    DeleteLine,
    Submit,
    AllowOnce,
    AllowSession,
    Deny,
    Choose(usize),
    NewSession,
    Cancel,
    ToggleYolo,
    OpenOverlay,
    OpenText(String),
    OpenPull,
    CloseOverlay,
    OpenModel,
    OpenProjects,
    OpenProviders,
    OpenPalette,
    OpenHelp,
    OpenQueue,
    RemoveQueued,
    SelectQueued(usize),
    OpenTodo(String),
    TogglePane(RightPane),
    OpenProof,
    OpenCheckTranscripts,
    OpenArtifact(crate::proof::ProofFile),
    FocusArtifact(Option<String>),
    ProofAction(screen::ProofAction),
    ProofMove(bool),
    ProofChoose(usize),
    ProofFileMove,
    ProofFileChoose(usize),
    OpenCloseout,
    OpenCheck(String),
    OpenFile(String),
    OpenLink(String),
    OpenTasks,
    OpenTask(String),
    OpenImage(crate::attachment::ImageAttachment),
    RemoveImage(usize),
    OpenSchedules,
    OpenThinking,
    OpenContext,
    ToggleLeft,
    ToggleRight,
    SaveLayout,
    DismissNotice,
    DragLeft(u16),
    DragRight(u16),
    SetLeftWidth(u16),
    SetRightWidth(u16),
    EndDrag,
    ToggleHeader(String),
    CollapseHeader,
    ExpandHeader,
    SetListFilter(screen::ListFilter),
    CycleListFilter,
    ArmSelect(TextSelect),
    MoveSelect(SelectPoint),
    CopyText(String, SelectPoint),
    OpenMenu { id: String, column: u16, row: u16 },
    MenuItem(usize),
    ToggleDeleteWorkspace,
    ConfirmDelete,
    ChooseDelete(usize),
    ArchiveSession,
    UnarchiveSession,
    EnhanceUse,
    EnhanceEdit,
    EnhanceDiscard,
    EnhanceRetry,
}

pub struct App {
    pub sessions: Vec<SessionRow>,
    pub selected: String,
    pub cards: Vec<Card>,
    card_event_ids: Vec<String>,
    artifact_focus: Option<String>,
    pub ask: String,
    pastes: Vec<PastedInput>,
    images: BTreeMap<String, Vec<crate::attachment::ImageAttachment>>,
    open_image: Option<crate::attachment::ImageAttachment>,
    pub question_text: String,
    pub scroll: usize,
    pub follow: bool,
    pub tick: usize,
    pub poll_revision: u64,
    pub area: Rect,
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub notice: Option<String>,
    notice_seen: Option<(String, Instant)>,
    pub yolo: bool,
    pub show_closeout: bool,
    pub enhance: bool,
    enhance_source: Option<String>,
    pub overlay: bool,
    overlay_pull: bool,
    workspace_step: WorkspaceStep,
    profile_step: Option<ProfileStep>,
    server_step: Option<Vec<String>>,
    server_highlight: usize,
    server_popup: Option<connections::Popup>,
    server_job: Option<connections::Job>,
    server_requested: Option<Option<String>>,
    server_removed: Option<String>,
    server: Option<String>,
    servers: Option<BTreeMap<Option<String>, servers::Snapshot>>,
    project_rows: Vec<ListedProject>,
    workspace_target: Option<(Option<String>, PathBuf)>,
    server_names: BTreeMap<String, String>,
    project_popup: Option<projects::Popup>,
    project_job: Option<projects::Job>,
    catalog_popup: Option<catalog::Popup>,
    catalog_job: Option<catalog::Job>,
    layout_job: Option<layout::Job>,
    saved_layout: Option<Option<crate::config::Layout>>,
    provider_popup: Option<providers::Popup>,
    provider_job: Option<providers::Job>,
    provider_next_poll: Instant,
    remote_defaults: Option<crate::pairing::ServerInfo>,
    open_todo: Option<String>,
    open_file: Option<FileView>,
    open_text: Option<String>,
    file_scroll: usize,
    question_id: Option<String>,
    dismissed_question: Option<String>,
    pub start_yolo: bool,
    pub skills: Vec<SkillEntry>,
    skill_highlight: usize,
    skill_picker_closed: bool,
    pub model: String,
    pub effort: Option<String>,
    picker: Option<Picker>,
    pub todos: Vec<TodoItem>,
    pub closeout: Vec<screen::CloseoutCheck>,
    pub open_check: Option<String>,
    pub closeout_scroll: usize,
    pub todo_scroll: usize,
    pub task_scroll: usize,
    pub schedule_scroll: usize,
    pub proof_versions: Vec<crate::proof::ProofVersion>,
    pub proof_selected: Option<u64>,
    pub proof_status: Option<String>,
    pub proof_scroll: usize,
    proof_popup: Option<proof::Popup>,
    proof_file_popup: Option<proof_files::Popup>,
    proof_file_job: Option<proof_files::Job>,
    pub pointer: Option<(u16, u16)>,
    pub tasks: Vec<screen::TaskLine>,
    task_detail: Option<TaskDetail>,
    pub schedules: Vec<screen::ScheduleLine>,
    pub right_panes: BTreeSet<RightPane>,
    pub phase: Option<screen::Phase>,
    pub action: Option<String>,
    pub thinking: String,
    pub retry_status: Option<String>,
    thinking_open: bool,
    pub context: Option<crate::compact::ContextUsage>,
    context_open: bool,
    pub queue: Vec<String>,
    pub queue_items: Vec<view::QueuedMessage>,
    queue_open: bool,
    queue_highlight: Option<String>,
    queue_removal: Option<queue::Removal>,
    command_ui: Option<CommandUi>,
    pub left_open: bool,
    pub left_width: u16,
    pub right_open: bool,
    pub right_width: u16,
    pub drag: Option<Drag>,
    pub drag_origin: u16,
    pub select: Option<TextSelect>,
    pub collapsed: BTreeSet<String>,
    pub list_header: Option<String>,
    pub list_scroll: usize,
    pub list_filter: screen::ListFilter,
    known_status: Option<BTreeMap<String, (Status, Option<Wait>)>>,
    menu: Option<SessionMenu>,
    delete_confirm: Option<DeleteConfirm>,
    delete_job: Option<deletion::Job>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionMenu {
    id: String,
    column: u16,
    row: u16,
    items: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DeleteConfirm {
    id: String,
    highlight: usize,
    status: Option<server::WorkspaceStatus>,
    remove_workspace: bool,
    confirm_dirty: bool,
    error: Option<String>,
    busy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CommandUi {
    Palette {
        query: String,
        highlight: usize,
        scroll: usize,
    },
    Help {
        scroll: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CatalogRow {
    line: screen::CommandLine,
    action: CommandAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CommandAction {
    NewSession,
    OpenModel,
    OpenEffort,
    OpenProfile,
    OpenServer,
    OpenQueue,
    OpenProjects,
    OpenProviders,
    Compact,
    Goal,
    ToggleYolo,
    ToggleEnhance,
    ToggleLeft,
    CycleListFilter,
    ToggleRight,
    SaveLayout,
    TogglePane(RightPane),
    OpenProof,
    OpenCheckTranscripts,
    Cancel,
    CloseSession,
    ArchiveSession,
    UnarchiveSession,
    CloseOverlay,
    Help,
}

fn catalog_row(name: &str, keys: &str, hint: &str, action: CommandAction) -> CatalogRow {
    CatalogRow {
        line: screen::CommandLine {
            name: name.to_string(),
            keys: keys.to_string(),
            hint: hint.to_string(),
        },
        action,
    }
}

fn command_catalog(_skills: &[SkillEntry]) -> Vec<CatalogRow> {
    let rows = vec![
        catalog_row(
            "New session",
            "Ctrl-T",
            "ask where this session should work",
            CommandAction::NewSession,
        ),
        catalog_row(
            "Open model",
            "Ctrl-M",
            "open the model list",
            CommandAction::OpenModel,
        ),
        catalog_row(
            "Effort",
            "",
            "open the effort list",
            CommandAction::OpenEffort,
        ),
        catalog_row(
            "Profile",
            "",
            "open the profile list",
            CommandAction::OpenProfile,
        ),
        catalog_row(
            "Server",
            "",
            "manage saved servers",
            CommandAction::OpenServer,
        ),
        catalog_row(
            "Projects",
            "",
            "configure projects on this server",
            CommandAction::OpenProjects,
        ),
        catalog_row(
            "Providers",
            "",
            "sign in on the selected server",
            CommandAction::OpenProviders,
        ),
        catalog_row(
            "Compact",
            "",
            "summarize the older transcript",
            CommandAction::Compact,
        ),
        catalog_row(
            "Yolo",
            "Ctrl-Y",
            "allow writes and commands",
            CommandAction::ToggleYolo,
        ),
        catalog_row(
            "Enhance",
            "",
            "rewrite a short prompt before the turn",
            CommandAction::ToggleEnhance,
        ),
        catalog_row(
            "Queued messages",
            "",
            "remove a pending message",
            CommandAction::OpenQueue,
        ),
        catalog_row("Cancel", "Ctrl-X", "stop the turn", CommandAction::Cancel),
        catalog_row(
            "Close popup",
            "Esc",
            "close the open popup",
            CommandAction::CloseOverlay,
        ),
        catalog_row(
            "Archive session",
            "",
            "hide the selected session until it is restored",
            CommandAction::ArchiveSession,
        ),
        catalog_row(
            "Unarchive session",
            "",
            "restore an archived session",
            CommandAction::UnarchiveSession,
        ),
        catalog_row(
            "Delete session",
            "Ctrl-W",
            "delete the selected session",
            CommandAction::CloseSession,
        ),
        catalog_row(
            "Sessions",
            "Ctrl-B",
            "show or hide the session list",
            CommandAction::ToggleLeft,
        ),
        catalog_row(
            "Filter sessions",
            "Ctrl-F",
            "cycle all, run, ask, done, and old",
            CommandAction::CycleListFilter,
        ),
        catalog_row(
            "Panes",
            "Ctrl-G",
            "show or hide the right column",
            CommandAction::ToggleRight,
        ),
        catalog_row(
            "Save layout",
            "",
            "use this layout for all sessions on this server",
            CommandAction::SaveLayout,
        ),
        catalog_row(
            "Todos",
            "",
            "show or hide the todos pane",
            CommandAction::TogglePane(RightPane::Todos),
        ),
        catalog_row(
            "Tasks",
            "",
            "show or hide the background-command pane",
            CommandAction::TogglePane(RightPane::Tasks),
        ),
        catalog_row(
            "Schedules",
            "",
            "show or hide the schedules pane",
            CommandAction::TogglePane(RightPane::Schedules),
        ),
        catalog_row(
            "Closeout",
            "",
            "show or hide the closeout pane",
            CommandAction::TogglePane(RightPane::Closeout),
        ),
        catalog_row(
            "Closeout transcripts",
            "",
            "open retained verification output",
            CommandAction::OpenCheckTranscripts,
        ),
        catalog_row(
            "Artifacts history",
            "",
            "browse versions and evidence",
            CommandAction::OpenProof,
        ),
        catalog_row(
            "Artifacts pane",
            "",
            "show or hide artifacts",
            CommandAction::TogglePane(RightPane::Proof),
        ),
        catalog_row("Help", "?", "list every command", CommandAction::Help),
        catalog_row(
            "/artifacts",
            "/artifacts",
            "show or hide artifacts",
            CommandAction::TogglePane(RightPane::Proof),
        ),
        catalog_row(
            "/model",
            "/model",
            "open the model list",
            CommandAction::OpenModel,
        ),
        catalog_row(
            "/effort",
            "/effort",
            "set the reasoning effort",
            CommandAction::OpenEffort,
        ),
        catalog_row(
            "/goal",
            "/goal",
            "set a goal, or status, pause, resume, clear",
            CommandAction::Goal,
        ),
        catalog_row(
            "/compact",
            "/compact",
            "summarize the older transcript",
            CommandAction::Compact,
        ),
        catalog_row(
            "/yolo",
            "/yolo",
            "turn yolo on or off",
            CommandAction::ToggleYolo,
        ),
        catalog_row(
            "/enhance",
            "/enhance",
            "turn enhance on or off",
            CommandAction::ToggleEnhance,
        ),
        catalog_row(
            "/todos",
            "/todos",
            "show or hide the todos pane",
            CommandAction::TogglePane(RightPane::Todos),
        ),
        catalog_row(
            "/tasks",
            "/tasks",
            "show or hide the background-command pane",
            CommandAction::TogglePane(RightPane::Tasks),
        ),
        catalog_row(
            "/schedules",
            "/schedules",
            "show or hide the schedules pane",
            CommandAction::TogglePane(RightPane::Schedules),
        ),
        catalog_row(
            "/closeout",
            "/closeout",
            "show or hide the closeout pane",
            CommandAction::TogglePane(RightPane::Closeout),
        ),
    ];
    rows
}

pub fn command_lines(skills: &[SkillEntry]) -> Vec<screen::CommandLine> {
    command_catalog(skills)
        .into_iter()
        .map(|row| row.line)
        .collect()
}

fn command_matches(row: &CatalogRow, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let needle = query.to_ascii_lowercase();
    let name = row.line.name.to_ascii_lowercase();
    let keys = row.line.keys.to_ascii_lowercase();
    let hint = row.line.hint.to_ascii_lowercase();
    name.contains(&needle) || keys.contains(&needle) || hint.contains(&needle)
}

fn filtered_commands(skills: &[SkillEntry], query: &str) -> Vec<CatalogRow> {
    command_catalog(skills)
        .into_iter()
        .filter(|row| command_matches(row, query))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Picker {
    Model {
        rows: Vec<ModelRow>,
        highlight: usize,
        query: String,
    },
    Effort {
        rows: Vec<String>,
        highlight: usize,
    },
}

impl App {
    pub fn new(workspace: PathBuf, home: PathBuf, selected: String) -> App {
        App {
            sessions: Vec::new(),
            selected,
            cards: Vec::new(),
            card_event_ids: Vec::new(),
            artifact_focus: None,
            ask: String::new(),
            pastes: Vec::new(),
            images: BTreeMap::new(),
            open_image: None,
            question_text: String::new(),
            scroll: 0,
            follow: true,
            tick: 0,
            poll_revision: 0,
            area: Rect::new(0, 0, 76, 24),
            home,
            workspace,
            notice: None,
            notice_seen: None,
            yolo: false,
            show_closeout: true,
            enhance: false,
            enhance_source: None,
            overlay: false,
            overlay_pull: false,
            workspace_step: WorkspaceStep::Off,
            profile_step: None,
            server_step: None,
            server_highlight: 0,
            server_popup: None,
            server_job: None,
            server_requested: None,
            server_removed: None,
            server: None,
            servers: None,
            project_rows: Vec::new(),
            workspace_target: None,
            server_names: BTreeMap::new(),
            project_popup: None,
            project_job: None,
            catalog_popup: None,
            catalog_job: None,
            layout_job: None,
            saved_layout: None,
            provider_popup: None,
            provider_job: None,
            provider_next_poll: Instant::now(),
            remote_defaults: None,
            open_todo: None,
            open_file: None,
            open_text: None,
            file_scroll: 0,
            question_id: None,
            dismissed_question: None,
            start_yolo: false,
            skills: Vec::new(),
            skill_highlight: 0,
            skill_picker_closed: false,
            model: String::new(),
            effort: None,
            picker: None,
            todos: Vec::new(),
            closeout: Vec::new(),
            open_check: None,
            closeout_scroll: 0,
            todo_scroll: 0,
            task_scroll: 0,
            schedule_scroll: 0,
            proof_versions: Vec::new(),
            proof_selected: None,
            proof_status: None,
            proof_scroll: 0,
            proof_popup: None,
            proof_file_popup: None,
            proof_file_job: None,
            pointer: None,
            tasks: Vec::new(),
            task_detail: None,
            schedules: Vec::new(),
            right_panes: BTreeSet::new(),
            phase: None,
            action: None,
            thinking: String::new(),
            retry_status: None,
            thinking_open: false,
            context: None,
            context_open: false,
            queue: Vec::new(),
            queue_items: Vec::new(),
            queue_open: false,
            queue_highlight: None,
            queue_removal: None,
            command_ui: None,
            left_open: true,
            left_width: LIST_WIDTH,
            right_open: false,
            right_width: TODOS_WIDTH,
            drag: None,
            drag_origin: 0,
            select: None,
            collapsed: BTreeSet::new(),
            list_header: None,
            list_scroll: 0,
            list_filter: screen::ListFilter::All,
            known_status: None,
            menu: None,
            delete_confirm: None,
            delete_job: None,
        }
    }

    pub fn selected_session(&self) -> Option<&SessionRow> {
        self.sessions.iter().find(|row| row.id == self.selected)
    }

    fn server_label<'a>(&'a self, id: &'a str) -> &'a str {
        self.server_names.get(id).map_or(id, String::as_str)
    }

    fn server_name(&self) -> &str {
        self.server
            .as_deref()
            .map_or("Local", |id| self.server_label(id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileView {
    path: String,
    text: String,
    truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskDetail {
    id: String,
    argv: String,
    state: String,
    tail: String,
}

#[derive(Deserialize)]
struct TaskReply {
    id: String,
    argv: Vec<String>,
    state: String,
    #[serde(default)]
    exit: Option<i32>,
    #[serde(default)]
    tail: String,
}

#[derive(Deserialize)]
struct FileReply {
    path: String,
    text: String,
    truncated: bool,
}

#[derive(Clone)]
enum Transport {
    Socket(PathBuf),
    Url { base: String, http: reqwest::Client },
}

#[derive(Clone)]
pub struct Client {
    transport: Transport,
    projects: Arc<Mutex<Option<CachedProjects>>>,
}

struct CachedProjects {
    refreshed: Instant,
    rows: Vec<ListedProject>,
}

impl Client {
    pub async fn connect() -> Result<Client, String> {
        Client::connect_with(None).await
    }

    pub async fn connect_with(url: Option<String>) -> Result<Client, String> {
        let mut url = url.filter(|url| !url.trim().is_empty());
        if let Some(uri) = url.as_deref() {
            if crate::pairing::connection(uri.trim())?
                .1
                .is_some_and(|token| token.starts_with("pair_"))
            {
                let (credential, _) = crate::pairing::verify(uri.trim(), "Kyoto Agent CLI").await?;
                let path = crate::config::Config::default_path()
                    .ok_or("no home directory for the config")?;
                crate::pairing::Connections::remember(&path, &credential, true)?;
                url = Some(credential);
            }
        }
        let client = Self::configured(url.as_deref())?;
        let (status, body) = tokio::time::timeout(
            Duration::from_secs(8),
            client.request("GET", "/v1/sessions", None),
        )
        .await
        .map_err(|_| "server connection timed out")??;
        if status != 200 {
            return Err(error_text(&body, status));
        }
        serde_json::from_str::<Vec<ListRow>>(&body).map_err(|_| "invalid server session list")?;
        if client.server_id().is_some() {
            client.server_info().await?;
        }
        if let Some(url) = url.as_deref().filter(|value| !value.trim().is_empty()) {
            let path =
                crate::config::Config::default_path().ok_or("no home directory for the config")?;
            crate::pairing::Connections::remember(&path, url.trim(), true)?;
        }
        Ok(client)
    }

    pub fn configured(url: Option<&str>) -> Result<Client, String> {
        if let Some(url) = url.map(str::trim).filter(|value| !value.is_empty()) {
            return Self::at_url(url);
        }
        let path =
            crate::config::Config::default_path().ok_or("no home directory for the config")?;
        let saved = crate::pairing::Connections::load(&path)?;
        let client = match saved.selected() {
            Some(url) => Client::at_url(url)?,
            None => {
                let socket = server::default_socket().ok_or("no home directory for the socket")?;
                Client::at(socket)
            }
        };
        Ok(client)
    }

    pub fn at(socket: PathBuf) -> Client {
        Client {
            transport: Transport::Socket(socket),
            projects: Arc::new(Mutex::new(None)),
        }
    }

    fn server_id(&self) -> Option<String> {
        match &self.transport {
            Transport::Socket(_) => None,
            Transport::Url { base, .. } => Some(base.trim_start_matches("https://").to_string()),
        }
    }

    async fn server_info(&self) -> Result<crate::pairing::ServerInfo, String> {
        let (status, body) = self.request("GET", "/v1/server", None).await?;
        if status != 200 {
            return Err(error_text(&body, status));
        }
        let info: crate::pairing::ServerInfo =
            serde_json::from_str(&body).map_err(|_| "invalid server information")?;
        if !Path::new(&info.workspace).is_absolute() {
            return Err("invalid server workspace".into());
        }
        Ok(info)
    }

    pub fn at_url(url: &str) -> Result<Client, String> {
        let (base, token) = crate::pairing::connection(url.trim())?;
        let parsed = reqwest::Url::parse(&base).map_err(|_| "invalid connection URL")?;
        if parsed.scheme() != "https" {
            return Err("the url must be https".to_string());
        }
        let host = parsed.host_str().unwrap_or("");
        let loopback = host == "127.0.0.1" || host == "localhost" || host == "::1";
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(token) = token {
            let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| "invalid connection token")?;
            value.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(loopback)
            .build()
            .map_err(|source| source.to_string())?;
        Ok(Client {
            transport: Transport::Url { base, http },
            projects: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, String), String> {
        match &self.transport {
            Transport::Socket(socket) => request_socket(socket, method, path, body).await,
            Transport::Url { base, http } => request_url(http, base, method, path, body).await,
        }
    }
}

async fn request_url(
    http: &reqwest::Client,
    base: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, String), String> {
    let url = format!("{base}{path}");
    let builder = match method {
        "GET" => http.get(&url),
        "POST" => http.post(&url),
        "PUT" => http.put(&url),
        "DELETE" => http.delete(&url),
        other => return Err(format!("unsupported method {other}")),
    };
    let builder = match body {
        Some(body) => builder
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string()),
        None => builder,
    };
    let response = builder
        .send()
        .await
        .map_err(|source| format!("cannot connect to {base}: {source}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|source| source.to_string())?;
    Ok((status, text))
}

async fn request_socket(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, String), String> {
    let mut stream = UnixStream::connect(socket).await.map_err(|source| {
        format!(
            "cannot connect to {}: {source} (is `kyotoagent serve` running?)",
            socket.display()
        )
    })?;
    let body = body.unwrap_or("");
    let head = format!(
            "{method} {path} HTTP/1.1\r\nhost: kyotoagent\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|source| format!("could not send the request: {source}"))?;
    stream
        .write_all(body.as_bytes())
        .await
        .map_err(|source| format!("could not send the request: {source}"))?;
    stream
        .flush()
        .await
        .map_err(|source| format!("could not send the request: {source}"))?;

    let mut raw = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(at) = head_end(&raw) {
            let head = String::from_utf8_lossy(&raw[..at]);
            let content_length = content_length(&head);
            if raw.len() >= at + 4 + content_length {
                break;
            }
        }
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|source| format!("the server did not answer: {source}"))?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&raw);
    Ok(split_response(&text))
}

thread_local! {
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            crossterm::event::DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        if std::thread::panicking() {
            let line = LAST_PANIC
                .with(|slot| slot.borrow_mut().take())
                .unwrap_or_else(|| "panic".to_string());
            eprintln!("kyotoagent: {line}");
        }
    }
}

pub async fn attach(id: Option<String>, yolo: bool, url: Option<String>) -> Result<(), String> {
    let mut client = if url.as_deref().is_some_and(|url| !url.trim().is_empty())
        || !io::stdin().is_terminal()
        || !io::stdout().is_terminal()
    {
        Client::connect_with(url).await?
    } else {
        Client::configured(None)?
    };
    let workspace = if client.server_id().is_some() {
        PathBuf::new()
    } else {
        current_workspace()?
    };
    let home = home_dir();
    let mut app = App::new(workspace, home, id.unwrap_or_default());
    app.server = client.server_id();
    app.server_names =
        crate::pairing::Connections::load(&app.home.join(".kyotoagent/config.toml"))?.server_names;
    app.start_yolo = yolo;
    screen::theme::detect();
    enable_raw_mode().map_err(|source| source.to_string())?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        ),
        SetTitle(format!("{} | {}", screen::PRODUCT, app.server_name()))
    )
    .map_err(|source| source.to_string())?;
    let restore = Restore;
    install_panic_hook();
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|source| source.to_string())?;
    let result = run_loop(&mut terminal, &mut app, &mut client).await;
    drop(restore);
    if result.is_ok() {
        eprintln!("kyotoagent: detached");
    }
    result
}

pub fn loop_error_is_fatal(err: &str) -> bool {
    err.to_ascii_lowercase().contains("broken pipe")
}

pub fn panic_line(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "panic".to_string()
    }
}

fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            LAST_PANIC.with(|slot| {
                *slot.borrow_mut() = Some(panic_line(info.payload()));
            });
            previous(info);
        }));
    });
}

fn notice_or_fatal(app: &mut App, err: String) -> Result<(), String> {
    if loop_error_is_fatal(&err) {
        Err(err)
    } else {
        app.notice = Some(err);
        Ok(())
    }
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    client: &mut Client,
) -> Result<(), String> {
    let animation = Instant::now();
    let mut refresh = poll::BackgroundPoll::new();
    let mut preview = crate::image_preview::Preview::new();
    let mut servers = BTreeMap::new();
    let mut connections = servers::Connections::new();
    let mut switching = connections::BackgroundSwitch::default();
    let mut title = String::new();
    switching.request(app, &servers, app.server.clone());
    loop {
        queue::advance(app).await;
        proof_files::advance(app).await;
        layout::advance(app).await;
        deletion::advance(app, client).await;
        proof_files::advance(app).await;
        projects::advance(app, client).await;
        providers::advance(app, client).await;
        catalog::advance(app).await;
        if switching.advance(app, client, &mut servers).await {
            refresh.invalidate();
        }
        let next_title = format!("{} | {}", screen::PRODUCT, app.server_name());
        if next_title != title {
            if let Err(error) = execute!(io::stdout(), SetTitle(&next_title)) {
                app.notice = Some(error.to_string());
            }
            title = next_title;
        }
        connections::advance(app).await;
        refresh.advance(app, client).await;
        connections.advance(app, client, &mut servers).await;
        if let Ok(size) = terminal.size() {
            app.area = Rect::new(0, 0, size.width, size.height);
        }
        app.tick =
            (animation.elapsed().as_millis() / screen::SPINNER_INTERVAL.as_millis()) as usize;
        advance_notice(app, Instant::now());
        let model = screen_model(app);
        let drawn = panic::catch_unwind(AssertUnwindSafe(|| {
            terminal
                .draw(|frame| {
                    screen::render(&model, frame.area(), frame);
                })
                .map(|completed| completed.area)
                .map_err(|source| source.to_string())
        }));
        let area = match drawn {
            Ok(Ok(area)) => {
                app.area = area;
                let image = match &model.overlay {
                    Some(Overlay::Image { image }) => Some(image),
                    _ => None,
                };
                if let Err(source) = preview.sync(
                    &mut io::stdout(),
                    image,
                    screen::split_of(&model, area).session,
                ) {
                    notice_or_fatal(app, source.to_string())?;
                }
                Some(area)
            }
            Ok(Err(source)) => {
                notice_or_fatal(app, source)?;
                None
            }
            Err(payload) => {
                app.notice = Some(panic_line(payload.as_ref()));
                None
            }
        };
        match event::poll(screen::SPINNER_INTERVAL) {
            Ok(true) => match event::read() {
                Ok(Event::Key(event)) => {
                    if let Some(effect) = keystroke(app, event) {
                        let effect = match effect {
                            Effect::CloseOverlay if esc_stops(app) => {
                                app.thinking_open = false;
                                app.overlay = false;
                                Effect::Cancel
                            }
                            other => other,
                        };
                        if !servers::Connections::apply(app, client, &mut servers, effect).await? {
                            return Ok(());
                        }
                    }
                }
                Ok(Event::Mouse(event)) => {
                    app.pointer = Some((event.column, event.row));
                    if let Some(area) = area {
                        if let Some(effect) = track_mouse(app, area, event) {
                            if !servers::Connections::apply(app, client, &mut servers, effect)
                                .await?
                            {
                                return Ok(());
                            }
                        }
                    }
                }
                Ok(Event::Paste(text)) => {
                    apply(app, client, Effect::Paste(text)).await?;
                }
                Ok(Event::Resize(_, _)) => {}
                Ok(_) => {}
                Err(source) => notice_or_fatal(app, source.to_string())?,
            },
            Ok(false) => {}
            Err(source) => notice_or_fatal(app, source.to_string())?,
        }
        if let Some(id) = app.server_removed.take() {
            switching.forget(app, client, &mut servers, &id);
            refresh.invalidate();
        }
        if let Some((target, workspace)) = app.workspace_target.take() {
            match servers::Connections::activate(app, client, &mut servers, &target) {
                Ok(()) => {
                    clear_for_workspace(app);
                    app.workspace_step = WorkspaceStep::Worktree(workspace);
                    refresh.invalidate();
                }
                Err(error) => app.notice = Some(error),
            }
        }
        if let Some(target) = app.server_requested.take() {
            switching.request(app, &servers, target);
        }
    }
}
