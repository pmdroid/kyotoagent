//! The quiet screen: a session list on the left, one session's cards on the
//! right, and a single input row at the bottom.
//!
//! The model is plain data. There is no socket, no event log, and no terminal
//! handling here, so a test can draw any state through [`render`] and read the
//! buffer back as text.
//!
//! The screen shows very little on purpose, so it spends its space on looking
//! deliberate: one accent colour per card kind, a rail down the left of every
//! card, badges instead of bare labels, and an input row that reads as a field.
//! All of that lives in [`theme`] and in the three render functions below, so
//! the live TUI later gets the same look for free.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::events::{TodoItem, TodoStatus};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod proof;
pub mod theme;
pub use proof::{proof_action_at, ProofAction};

mod card;
mod file;
mod hit;
mod list;
mod overlay;
mod select;
mod session;

use card::*;
pub use hit::*;
pub use list::*;
pub use overlay::file_scroll_max;
use overlay::*;
pub use select::*;
pub use session::*;

#[cfg(test)]
mod tests;

pub const LIST_WIDTH: u16 = 30;
pub const TODOS_WIDTH: u16 = 24;
const RAIL_WIDTH: u16 = 1;
pub const CARD_MIN: u16 = 20;

pub use crate::config::SidebarPane as RightPane;

impl RightPane {
    pub const ORDER: [RightPane; 5] = [
        RightPane::Todos,
        RightPane::Closeout,
        RightPane::Tasks,
        RightPane::Schedules,
        RightPane::Proof,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RightPane::Todos => "todos",
            RightPane::Closeout => "closeout",
            RightPane::Tasks => "tasks",
            RightPane::Schedules => "schedules",
            RightPane::Proof => "artifacts",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenTask {
    pub id: String,
    pub argv: String,
    pub state: String,
    pub tail: String,
}
/// Height of the header bar.
const HEADER_HEIGHT: u16 = 1;
pub const LIST_GLYPH: &str = "\u{2630}";
pub const PANES_GLYPH: &str = "\u{25a6}";
/// The rail and the gap after it, before a card's own text.
const RAIL: &str = "\u{258e} ";
const RAIL_RIGHT: &str = " \u{258e}";
/// The dim rail that runs down a card's remaining lines.
const RAIL_DIM: &str = "\u{2502} ";
const RIGHT_RAIL: &str = " \u{258e}";
const RIGHT_RAIL_DIM: &str = " \u{2502}";
/// Frames of the working spinner, in order.
pub(crate) const FLUX_SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const SPINNER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(80);

/// The rainbow the header's `yolo` word runs through, one colour per column.
/// The live TUI's tick shifts the pattern so the word animates while yolo is
/// on: yolo should feel like a magic power, and a still word does not.
const YOLO_RAINBOW: [Color; 7] = [
    Color::Rgb(0xff, 0x4d, 0x4d),
    Color::Rgb(0xff, 0xa6, 0x4d),
    Color::Rgb(0xff, 0xe0, 0x4d),
    Color::Rgb(0x5c, 0xe0, 0x5c),
    Color::Rgb(0x4d, 0xc4, 0xff),
    Color::Rgb(0x8a, 0x7d, 0xff),
    Color::Rgb(0xd8, 0x6d, 0xff),
];

pub const PRODUCT: &str = "Kyoto Agent";
pub const OTHER_PROJECT: &str = "other";

/// The first four characters of a session id, as the list shows it.
pub fn short_id(id: &str) -> String {
    id.chars().take(4).collect()
}

/// A workspace path with the home directory shown as `~`.
///
/// `path` outside `home` is returned unchanged. `path` equal to `home` is `~`.
pub fn relative_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Greedy word wrap, so a long card line breaks at spaces instead of relying on
/// the widget layer. Gaps inside a line keep their size, because a choice list
/// such as `1  kyotoagent` is aligned on purpose. A word longer than `width` is
/// broken by display columns.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for (gap, word) in words(text) {
        if line.is_empty() {
            push_word(&mut lines, &mut line, word, width);
        } else if cols(&line) + gap + cols(word) <= width {
            line.extend(std::iter::repeat_n(' ', gap));
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            push_word(&mut lines, &mut line, word, width);
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

fn cols(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn push_word(lines: &mut Vec<String>, line: &mut String, word: &str, width: usize) {
    if cols(word) <= width {
        line.push_str(word);
        return;
    }
    for ch in word.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if !line.is_empty() && cols(line) + w > width {
            lines.push(std::mem::take(line));
        }
        line.push(ch);
    }
}

/// Every word in `text` with the size of the whitespace run before it. The
/// first word has no gap, and the text is trimmed at both ends.
fn words(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            return out;
        }
        let gap = rest.len() - trimmed.len();
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        out.push((if out.is_empty() { 0 } else { gap }, &trimmed[..end]));
        rest = &trimmed[end..];
    }
}

/// What a session is doing.
///
/// The spelling is shared with `meta.json`: a session on disk carries the same
/// three words the list shows, so one type serves both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Idle,
    Working,
    Waiting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Thinking,
    Tool,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Working => "working",
            Status::Waiting => "waiting",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Status::Idle => theme::muted(),
            Status::Working => theme::accent(),
            Status::Waiting => theme::attention(),
        }
    }
}

/// The card a waiting session is blocked on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    Permission,
    Question,
    Enhance,
}

impl Wait {
    pub fn label(self) -> &'static str {
        match self {
            Wait::Permission => "permission",
            Wait::Question => "question",
            Wait::Enhance => "enhance",
        }
    }
}

/// One row of the session list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    /// The full session id. The list shows only [`short_id`] of it.
    pub id: String,
    /// The session's working directory.
    pub workspace: PathBuf,
    pub title: Option<String>,
    pub status: Status,
    /// Set while the status is [`Status::Waiting`]: the card that blocks it.
    pub waiting: Option<Wait>,
    pub pull_url: Option<String>,
    pub compacting: bool,
    pub yolo: bool,
    pub show_closeout: bool,
    pub profile: Option<String>,
    pub enhance: bool,
    pub model: String,
    pub effort: Option<String>,
    pub project: Option<String>,
    pub project_name: Option<String>,
    pub parent_id: Option<String>,
    pub isolation: Option<String>,
    pub worktree: bool,
}

impl SessionRow {
    /// The directory name, which is what the list shows next to the id.
    pub fn name(&self) -> String {
        if let Some(title) = self
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            return title.to_string();
        }
        self.workspace
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.workspace.display().to_string())
    }

    /// The status as the list shows it. A waiting row also names its card.
    pub fn status_text(&self) -> String {
        match (self.status, self.waiting) {
            (Status::Idle, _) => Status::Idle.label().to_string(),
            (Status::Working, _) => Status::Working.label().to_string(),
            (Status::Waiting, Some(wait)) => {
                format!("{} {}", Status::Waiting.label(), wait.label())
            }
            (Status::Waiting, None) => Status::Waiting.label().to_string(),
        }
    }

    pub fn pull_mark(&self) -> Option<String> {
        self.pull_url.as_deref().and_then(crate::session::pull_mark)
    }
}

/// One choice of a question card, and whether it is the chosen one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub marked: bool,
}

/// One card in the right pane. Tool calls and tool results are not cards, so
/// a file read that the agent did silently leaves nothing on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Card {
    /// What the user asked.
    Ask {
        text: String,
        images: Vec<crate::attachment::ImageAttachment>,
    },
    /// A question the agent asked the user.
    Question {
        text: String,
        choices: Vec<Choice>,
        answer: Option<String>,
    },
    Answer {
        text: String,
    },
    /// A write or a command waiting on an allow.
    Permission {
        /// The action as the agent proposed it, such as `Replace README.md`.
        action: String,
        /// The answer, once one came. It replaces the action on screen.
        decision: Option<String>,
        /// The proposed change, absent once the answer came.
        diff: Vec<String>,
        /// The command as the harness would run it, for a permission on a
        /// command rather than on a write. Absent once the answer came.
        argv: Vec<String>,
    },
    /// The answer the agent finished with.
    Result {
        text: String,
    },
    Proof {
        text: String,
        items: Vec<ItemRun>,
    },
    Artifact {
        file: crate::proof::ProofFile,
        caption: Option<String>,
        focused: bool,
    },
    Enhance {
        source: String,
        text: String,
        error: Option<String>,
        event_id: String,
    },
}

/// What a closeout item does, as `.kyotoagent/closeout.yaml` names its `kind`.
///
/// `command` is the only kind kyotoagent accepts so far: the others are parse
/// errors until their issues land. They are here because the file can name
/// them, and because the turn loop has to branch on the kind to know what a
/// permission for that item looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Command,
    Cucumber,
    Visual,
    Receipt,
    Review,
    Ci,
    ReviewThreads,
}

impl ItemKind {
    /// The spelling the file uses.
    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Command => "command",
            ItemKind::Cucumber => "cucumber",
            ItemKind::Visual => "visual",
            ItemKind::Receipt => "receipt",
            ItemKind::Review => "review",
            ItemKind::Ci => "ci",
            ItemKind::ReviewThreads => "reviewThreads",
        }
    }

    pub fn from_label(label: &str) -> Option<ItemKind> {
        match label {
            "command" => Some(ItemKind::Command),
            "cucumber" => Some(ItemKind::Cucumber),
            "visual" => Some(ItemKind::Visual),
            "receipt" => Some(ItemKind::Receipt),
            "review" => Some(ItemKind::Review),
            "ci" => Some(ItemKind::Ci),
            "reviewThreads" => Some(ItemKind::ReviewThreads),
            _ => None,
        }
    }
}

/// How a closeout item ended. The attempt count is the run log's business, so
/// only the outcome reaches the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
}

impl Outcome {
    pub fn marker(self) -> &'static str {
        match self {
            Outcome::Passed => "\u{2713}",
            Outcome::Failed => "\u{2717}",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Outcome::Passed => theme::good(),
            Outcome::Failed => theme::bad(),
        }
    }

    pub fn from_label(label: &str) -> Option<Outcome> {
        match label {
            "passed" => Some(Outcome::Passed),
            "failed" => Some(Outcome::Failed),
            _ => None,
        }
    }
}

/// One closeout item the harness ran for this turn, as the file named it: an
/// id, what kind of check it is, and how it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemRun {
    pub id: String,
    pub kind: ItemKind,
    pub outcome: Outcome,
    pub argv: Vec<String>,
    pub exit: Option<i32>,
    pub tail: String,
}

/// What the one input row is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bottom {
    /// The next ask, typed by the user. Idle sessions show this.
    Prompt,
    /// Keys that answer the open card. A permission shows one letter per
    /// answer, a question shows its numbers.
    Keys,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    Queue {
        rows: Vec<String>,
        highlight: usize,
        removing: bool,
    },
    Pairing {
        title: String,
        qr: String,
    },
    Image {
        image: crate::attachment::ImageAttachment,
    },
    Text {
        text: String,
    },
    Question {
        text: String,
        choices: Vec<Choice>,
        prompt: String,
    },
    Permission {
        action: String,
        diff: Vec<String>,
        argv: Vec<String>,
    },
    Proof {
        text: String,
        items: Vec<ItemRun>,
    },
    Pull {
        url: String,
    },
    Model {
        rows: Vec<String>,
        highlight: usize,
    },
    Effort {
        rows: Vec<String>,
        highlight: usize,
    },
    File {
        path: String,
        text: String,
        truncated: bool,
    },
    Thinking {
        text: String,
    },
    Palette {
        rows: Vec<CommandLine>,
        highlight: usize,
        scroll: usize,
        query: String,
    },
    Help {
        rows: Vec<CommandLine>,
        scroll: usize,
    },
    Context {
        percent: u32,
        used: u64,
        reported_prompt_tokens: Option<u64>,
        window: u64,
        buckets: Vec<ContextLine>,
    },
    Menu {
        id: String,
        items: Vec<String>,
        column: u16,
        row: u16,
    },
    Delete {
        path: Option<String>,
        highlight: usize,
        remove_workspace: bool,
        confirm_dirty: bool,
        warning: Option<String>,
        loading: bool,
    },
    Enhance {
        source: String,
        text: String,
        error: Option<String>,
    },
}

pub const MENU_CLOSE: &str = "Delete session";
pub const MENU_REMOVE_WORKTREE: &str = "Remove worktree";
pub const DELETE_PROMPT: &str = "Delete this session?";
pub const DELETE_DIRECTORY: &str = "That directory will be removed.";
pub const DELETE_YES: &str = "Delete";
pub const DELETE_NO: &str = "Cancel";

pub fn session_menu_items(_worktree: bool) -> Vec<String> {
    vec![MENU_CLOSE.to_string()]
}

pub fn delete_labels(
    path: Option<&str>,
    remove_workspace: bool,
    confirm_dirty: bool,
) -> Vec<String> {
    let mut labels = Vec::new();
    if path.is_some() {
        labels.push(format!(
            "[{}] Also delete workspace",
            if remove_workspace { "x" } else { " " }
        ));
    }
    labels.push(if confirm_dirty {
        "Delete session and uncommitted files".into()
    } else {
        DELETE_YES.into()
    });
    labels.push(DELETE_NO.into());
    labels
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextLine {
    pub id: String,
    pub tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskLine {
    pub id: String,
    pub argv: String,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleLine {
    pub id: String,
    pub note: String,
    pub remaining_min: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillPickerRow {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandLine {
    pub name: String,
    pub keys: String,
    pub hint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillPicker {
    pub rows: Vec<SkillPickerRow>,
    pub selected: usize,
}

/// Everything the screen draws. Plain data, so a test can hand it a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseoutMark {
    NotRequired,
    Missing,
    Running,
    Passed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseoutCheck {
    pub runs: Vec<crate::events::CloseoutRunBody>,
    pub id: String,
    pub kind: String,
    pub status: CloseoutMark,
    pub required: bool,
    pub exit: Option<i32>,
    pub attempt: Option<u32>,
    pub tail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub server: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenModel {
    /// Every known session, newest first.
    pub sessions: Vec<SessionRow>,
    pub projects: Vec<ProjectRow>,
    /// The full id of the selected session.
    pub selected: String,
    /// The selected session's cards, in order.
    pub cards: Vec<Card>,
    /// The one input row. It holds the next ask, the permission keys, or
    /// nothing at all.
    pub bottom: String,
    pub toast: Option<String>,
    /// Whether `bottom` is something the user types or a set of keys that
    /// answer the open card.
    pub bottom_kind: Bottom,
    pub pasted_text: Option<String>,
    pub pending_images: Vec<crate::attachment::ImageAttachment>,
    /// The home directory. A workspace under it is shown as `~/...`.
    pub home: PathBuf,
    /// Which frame of the working spinner to draw. The live TUI advances it.
    pub tick: usize,
    pub yolo: bool,
    pub enhance: bool,
    pub overlay: Option<Overlay>,
    pub file_scroll: usize,
    pub skill_picker: Option<SkillPicker>,
    pub model: String,
    pub effort: Option<String>,
    pub compacting: bool,
    pub todos: Vec<TodoItem>,
    pub tasks: Vec<TaskLine>,
    pub schedules: Vec<ScheduleLine>,
    pub phase: Option<Phase>,
    pub action: Option<String>,
    pub thinking: String,
    pub retry_status: Option<String>,
    pub queue: usize,
    pub left_open: bool,
    pub right_open: bool,
    pub left_width: u16,
    pub right_width: u16,
    pub right_panes: BTreeSet<RightPane>,
    pub open_todo: Option<String>,
    pub todo_scroll: usize,
    pub closeout: Vec<CloseoutCheck>,
    pub open_check: Option<String>,
    pub closeout_scroll: usize,
    pub open_task: Option<OpenTask>,
    pub task_scroll: usize,
    pub schedule_scroll: usize,
    pub proof_versions: Vec<crate::proof::ProofVersion>,
    pub proof_selected: Option<u64>,
    pub proof_status: Option<String>,
    pub proof_scroll: usize,
    pub drag: Option<Drag>,
    pub drag_origin: u16,
    pub scroll: usize,
    pub select: Option<TextSelect>,
    pub collapsed: BTreeSet<String>,
    pub list_header: Option<String>,
    pub context_percent: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drag {
    Left,
    Right,
}

impl Default for ScreenModel {
    fn default() -> ScreenModel {
        ScreenModel {
            sessions: Vec::new(),
            projects: Vec::new(),
            selected: String::new(),
            cards: Vec::new(),
            bottom: String::new(),
            toast: None,
            bottom_kind: Bottom::Prompt,
            pasted_text: None,
            pending_images: Vec::new(),
            home: PathBuf::new(),
            tick: 0,
            yolo: false,
            enhance: false,
            overlay: None,
            file_scroll: 0,
            skill_picker: None,
            model: String::new(),
            effort: None,
            compacting: false,
            todos: Vec::new(),
            tasks: Vec::new(),
            schedules: Vec::new(),
            phase: None,
            action: None,
            thinking: String::new(),
            retry_status: None,
            queue: 0,
            left_open: true,
            right_open: false,
            left_width: LIST_WIDTH,
            right_width: TODOS_WIDTH,
            right_panes: BTreeSet::new(),
            open_todo: None,
            todo_scroll: 0,
            closeout: Vec::new(),
            open_check: None,
            closeout_scroll: 0,
            open_task: None,
            task_scroll: 0,
            schedule_scroll: 0,
            proof_versions: Vec::new(),
            proof_selected: None,
            proof_status: None,
            proof_scroll: 0,
            drag: None,
            drag_origin: 0,
            scroll: 0,
            select: None,
            collapsed: BTreeSet::new(),
            list_header: None,
            context_percent: None,
        }
    }
}

impl ScreenModel {
    pub fn selected_session(&self) -> Option<&SessionRow> {
        self.sessions.iter().find(|row| row.id == self.selected)
    }
}

/// Draw `model` into `area`.
///
/// `area` is the whole screen: a header bar, the session column, the session
/// pane, and the input row. The 76 by 24 frames in `tests/screens` are this
/// layout at its intended size.
pub fn render(model: &ScreenModel, area: Rect, frame: &mut Frame) {
    let split = split_of(model, area);
    render_header(model, split.header, frame);
    if split.list.width <= RAIL_WIDTH {
        render_rail(split.list, frame);
    } else {
        render_list(model, split.list, frame);
    }
    render_session(model, split.session, frame);
    if split.todos.width > RAIL_WIDTH {
        render_right(model, split.todos, frame);
    }
    if let Some(skill_picker) = &model.skill_picker {
        render_picker(skill_picker, split.picker, frame);
    }
    render_input(model, split.input, frame);
    if model.overlay.is_some() {
        let pane = if matches!(
            model.overlay,
            Some(Overlay::Menu { .. } | Overlay::Pairing { .. })
        ) {
            area
        } else {
            split.session
        };
        render_overlay(model, area, pane, frame);
    }
    render_toast(model, area, frame);
}

pub fn toast_rect(model: &ScreenModel, area: Rect) -> Option<Rect> {
    let text = model.toast.as_ref()?;
    let bottom = split_of(model, area).input.y.saturating_sub(1);
    let available_height = bottom.saturating_sub(area.y + HEADER_HEIGHT);
    let width = u16::try_from(cols(text).min(60) + 4)
        .unwrap_or(64)
        .min(area.width.saturating_sub(2));
    if width < 5 || available_height < 3 {
        return None;
    }
    let height = (wrap(text, usize::from(width - 4)).len().min(4) as u16 + 2).min(available_height);
    Some(Rect::new(
        area.right().saturating_sub(width + 1),
        bottom.saturating_sub(height),
        width,
        height,
    ))
}

pub fn toast_close_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    toast_rect(model, area).is_some_and(|rect| row == rect.y && column == rect.right() - 1)
}

fn render_toast(model: &ScreenModel, area: Rect, frame: &mut Frame) {
    let Some(rect) = toast_rect(model, area) else {
        return;
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border_focused())
        .style(theme::body().bg(theme::selected_bg()));
    let inner = block.inner(rect);
    let text = model.toast.as_deref().unwrap_or_default();
    let lines: Vec<Line<'static>> = wrap(text, usize::from(rect.width - 4))
        .into_iter()
        .take(usize::from(inner.height))
        .map(|line| Line::from(format!(" {line}")))
        .collect();
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(lines), inner);
    paint_close(frame, rect);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Split {
    pub header: Rect,
    pub list: Rect,
    pub session: Rect,
    pub todos: Rect,
    pub picker: Rect,
    pub input: Rect,
}

pub fn split_of(model: &ScreenModel, area: Rect) -> Split {
    let picker_rows = model
        .skill_picker
        .as_ref()
        .map(|picker| picker.rows.len() as u16)
        .unwrap_or(0);
    let framed = composer_framed(model, area.height);
    let input_rows = u16::try_from(
        input_lines(model, area.width.saturating_sub(if framed { 2 } else { 0 })).len()
            + if framed { 2 } else { 0 },
    )
    .unwrap_or(u16::MAX)
    .max(1)
    .min(
        area.height
            .saturating_sub(HEADER_HEIGHT.saturating_add(picker_rows).saturating_add(3))
            .max(1),
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HEADER_HEIGHT),
            Constraint::Min(0),
            Constraint::Length(picker_rows),
            Constraint::Length(input_rows),
        ])
        .split(area);
    let (left_w, right_w) = side_widths(model, area.width);
    let constraints = if right_w == 0 {
        vec![Constraint::Length(left_w), Constraint::Min(CARD_MIN)]
    } else {
        vec![
            Constraint::Length(left_w),
            Constraint::Min(CARD_MIN),
            Constraint::Length(right_w),
        ]
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(rows[1]);
    let todos = if right_w == 0 {
        Rect::new(0, 0, 0, 0)
    } else {
        columns[2]
    };
    Split {
        header: rows[0],
        list: columns[0],
        session: columns[1],
        todos,
        picker: rows[2],
        input: rows[3],
    }
}

pub fn side_widths(model: &ScreenModel, area_width: u16) -> (u16, u16) {
    let mut left = if model.left_open {
        model.left_width.max(2)
    } else {
        RAIL_WIDTH
    };
    let mut right = if model.right_open && !model.right_panes.is_empty() {
        model.right_width.max(2)
    } else {
        0
    };
    let min_total = left.saturating_add(right).saturating_add(CARD_MIN);
    if area_width < min_total {
        let mut deficit = min_total - area_width;
        if right > 0 && deficit > 0 {
            let kept = right.saturating_sub(deficit);
            if kept < 2 {
                deficit = deficit.saturating_sub(right);
                right = 0;
            } else {
                right = kept;
                deficit = 0;
            }
        }
        if deficit > 0 && left > RAIL_WIDTH {
            let shrink = deficit.min(left - RAIL_WIDTH);
            left -= shrink;
        }
    }
    (left, right)
}

pub fn wide_right_width(model: &ScreenModel, area_width: u16) -> u16 {
    let left = if model.left_open {
        model.left_width.max(2)
    } else {
        RAIL_WIDTH
    };
    area_width
        .saturating_sub(left)
        .saturating_sub(CARD_MIN)
        .max(2)
}

pub fn session_inner(area: Rect) -> Rect {
    session_inner_of(&ScreenModel::default(), area)
}

pub fn session_inner_with_picker(area: Rect, picker_rows: u16) -> Rect {
    let mut model = ScreenModel::default();
    if picker_rows > 0 {
        model.skill_picker = Some(SkillPicker {
            rows: vec![
                SkillPickerRow {
                    name: String::new(),
                    description: String::new(),
                };
                picker_rows as usize
            ],
            selected: 0,
        });
    }
    session_inner_of(&model, area)
}

fn session_frame_inner(model: &ScreenModel, area: Rect) -> Rect {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .inner(split_of(model, area).session)
}

pub fn session_inner_of(model: &ScreenModel, area: Rect) -> Rect {
    session_frame_inner(model, area)
}

pub fn right_column_visible(model: &ScreenModel) -> bool {
    !model.right_panes.is_empty()
        || !model.todos.is_empty()
        || !model.closeout.is_empty()
        || !model.tasks.is_empty()
        || !model.schedules.is_empty()
}

pub fn stacked_panes(model: &ScreenModel) -> Vec<RightPane> {
    if !model.right_open {
        return Vec::new();
    }
    RightPane::ORDER
        .into_iter()
        .filter(|pane| model.right_panes.contains(pane))
        .collect()
}

pub fn right_pane_rects(model: &ScreenModel, area: Rect) -> Vec<(RightPane, Rect)> {
    layout_panes(model, split_of(model, area).todos)
}

fn layout_panes(model: &ScreenModel, column: Rect) -> Vec<(RightPane, Rect)> {
    let panes = stacked_panes(model);
    if panes.is_empty() || column.width == 0 || column.height == 0 {
        return Vec::new();
    }
    let constraints = vec![Constraint::Fill(1); panes.len()];
    let rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(column);
    panes.into_iter().zip(rects.iter().copied()).collect()
}

pub fn pane_rect(model: &ScreenModel, area: Rect, pane: RightPane) -> Option<Rect> {
    right_pane_rects(model, area)
        .into_iter()
        .find(|(candidate, _)| *candidate == pane)
        .map(|(_, rect)| rect)
}

fn pane_scroll(model: &ScreenModel, pane: RightPane) -> usize {
    match pane {
        RightPane::Todos => model.todo_scroll,
        RightPane::Closeout => model.closeout_scroll,
        RightPane::Tasks => model.task_scroll,
        RightPane::Schedules => model.schedule_scroll,
        RightPane::Proof => model.proof_scroll,
    }
}

fn list_glyph_style(model: &ScreenModel) -> Style {
    if model.left_open {
        theme::bar()
    } else {
        theme::faint()
    }
}

fn panes_glyph_style(model: &ScreenModel) -> Style {
    if model.right_open {
        theme::bar()
    } else {
        theme::faint()
    }
}

fn header_left(model: &ScreenModel) -> Line<'static> {
    let mut spans = vec![
        Span::styled(" ", theme::bar()),
        Span::styled(LIST_GLYPH, list_glyph_style(model)),
        Span::styled(format!(" {PRODUCT} "), theme::bar()),
    ];
    if model.yolo {
        spans.push(Span::styled(
            " yolo ",
            Style::default()
                .fg(YOLO_RAINBOW[1])
                .add_modifier(Modifier::BOLD),
        ));
    }
    if model.enhance {
        spans.push(Span::styled(" enhance ", theme::faint()));
    }
    if let Some(percent) = model.context_percent {
        spans.push(Span::styled(format!(" {percent}%"), theme::faint()));
    }
    Line::from(spans)
}

pub fn context_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    let Some((start, end)) = percent_span(model) else {
        return false;
    };
    let header = split_of(model, area).header;
    if row != header.y || column < header.x {
        return false;
    }
    let local = usize::from(column - header.x);
    local >= start && local < end
}

pub fn list_glyph_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    let header = split_of(model, area).header;
    header.width > 1 && row == header.y && column == header.x.saturating_add(1)
}

pub fn panes_glyph_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    let header = split_of(model, area).header;
    let Some(at) = panes_glyph_column(model, header.width) else {
        return false;
    };
    row == header.y && column == header.x.saturating_add(at)
}

pub fn panes_glyph_column(_model: &ScreenModel, width: u16) -> Option<u16> {
    width.checked_sub(1)
}

fn percent_span(model: &ScreenModel) -> Option<(usize, usize)> {
    let percent = model.context_percent?;
    let label = format!(" {percent}%");
    let mut start = 1 + LIST_GLYPH.chars().count() + format!(" {PRODUCT} ").chars().count();
    if model.yolo {
        start += " yolo ".chars().count();
    }
    if model.enhance {
        start += " enhance ".chars().count();
    }
    Some((start, start + label.chars().count()))
}

fn grouped(value: u64) -> String {
    let raw = value.to_string();
    let mut out = String::new();
    for (index, ch) in raw.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn header_profile(model: &ScreenModel) -> Option<&str> {
    model
        .selected_session()
        .and_then(|row| row.profile.as_deref())
        .filter(|name| !name.is_empty())
}

fn header_right(model: &ScreenModel) -> String {
    let counts = format!(
        "{} session{} \u{00b7} {} waiting",
        model.sessions.len(),
        if model.sessions.len() == 1 { "" } else { "s" },
        model
            .sessions
            .iter()
            .filter(|row| row.status == Status::Waiting)
            .count(),
    );
    let rest = match (
        model.compacting,
        model.model.is_empty(),
        model.effort.as_deref(),
    ) {
        (true, true, _) => format!("compact \u{00b7} {counts}"),
        (true, false, None | Some("")) => {
            format!("compact \u{00b7} {} \u{00b7} {counts}", model.model)
        }
        (true, false, Some(effort)) => {
            format!(
                "compact \u{00b7} {} {effort} \u{00b7} {counts}",
                model.model
            )
        }
        (false, true, _) => counts,
        (false, false, None | Some("")) => format!("{} \u{00b7} {counts}", model.model),
        (false, false, Some(effort)) => format!("{} {effort} \u{00b7} {counts}", model.model),
    };
    match header_profile(model) {
        Some(name) => format!("{name} \u{00b7} {rest}"),
        None => rest,
    }
}

fn render_header(model: &ScreenModel, area: Rect, frame: &mut Frame) {
    let left = header_left(model);
    let right = header_right(model);
    let content = Rect {
        width: area.width.saturating_sub(2),
        ..area
    };
    let gap = usize::from(content.width).saturating_sub(left.width() + cols(&right));
    let mut spans = left.spans;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::styled(right, theme::faint()));
    frame.render_widget(Paragraph::new(Line::from(spans)), content);
    if let Some(column) = panes_glyph_column(model, area.width) {
        frame.render_widget(
            Paragraph::new(Span::styled(PANES_GLYPH, panes_glyph_style(model))),
            Rect::new(area.x.saturating_add(column), area.y, 1, area.height),
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TodoTarget {
    Item(String),
    File(String),
    Link(String),
}

fn todo_status_word(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in_progress",
        TodoStatus::Done => "done",
    }
}

fn todo_entries(model: &ScreenModel, width: usize) -> Vec<(Line<'static>, TodoTarget)> {
    let mut out = Vec::new();
    for item in &model.todos {
        let (mark, style) = match item.status {
            TodoStatus::Pending => ("\u{00b7} ", theme::faint()),
            TodoStatus::InProgress => (
                "\u{25b8} ",
                Style::default()
                    .fg(theme::attention())
                    .add_modifier(Modifier::BOLD),
            ),
            TodoStatus::Done => ("\u{2713} ", Style::default().fg(theme::good())),
        };
        let mark_cols = UnicodeWidthStr::width(mark);
        let rest = width.saturating_sub(mark_cols);
        let title = truncate_cols(&item.title, rest);
        let text = format!("{mark}{title}");
        out.push((
            Line::from(Span::styled(pad(&text, width), style)),
            TodoTarget::Item(item.id.clone()),
        ));
        if model.open_todo.as_deref() != Some(item.id.as_str()) {
            continue;
        }
        out.push((
            Line::from(Span::styled(
                pad(todo_status_word(item.status), width),
                theme::faint(),
            )),
            TodoTarget::Item(item.id.clone()),
        ));
        if let Some(description) = &item.description {
            for line in wrap(description, width.max(1)) {
                out.push((
                    Line::from(Span::styled(pad(&line, width), theme::body())),
                    TodoTarget::Item(item.id.clone()),
                ));
            }
        }
        for path in &item.files {
            for line in wrap(path, width.max(1)) {
                out.push((
                    Line::from(Span::styled(pad(&line, width), theme::faint())),
                    TodoTarget::File(path.clone()),
                ));
            }
        }
        for link in &item.links {
            for line in wrap(link, width.max(1)) {
                out.push((
                    Line::from(Span::styled(pad(&line, width), theme::faint())),
                    TodoTarget::Link(link.clone()),
                ));
            }
        }
    }
    out
}

fn pane_body(rect: Rect) -> Option<Rect> {
    if rect.width < 2 || rect.height < 2 {
        return None;
    }
    let inner = Block::bordered().inner(rect);
    if inner.width == 0 || inner.height == 0 {
        None
    } else {
        Some(inner)
    }
}

fn pane_line_index(
    model: &ScreenModel,
    area: Rect,
    pane: RightPane,
    column: u16,
    row: u16,
) -> Option<(usize, usize)> {
    let rect = pane_rect(model, area, pane)?;
    let inner = pane_body(rect)?;
    if !inner.contains(Position { x: column, y: row }) {
        return None;
    }
    let index = usize::from(row.saturating_sub(inner.y)).saturating_add(pane_scroll(model, pane));
    Some((index, usize::from(inner.width)))
}

pub fn todo_target(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<TodoTarget> {
    let (index, width) = pane_line_index(model, area, RightPane::Todos, column, row)?;
    let offset = usize::from(!model.todos.is_empty());
    if index < offset {
        return None;
    }
    todo_entries(model, width)
        .get(index - offset)
        .map(|(_, target)| target.clone())
}

fn render_right(model: &ScreenModel, column: Rect, frame: &mut Frame) {
    for (pane, rect) in layout_panes(model, column) {
        render_pane(model, pane, rect, frame);
    }
}

fn render_pane(model: &ScreenModel, pane: RightPane, area: Rect, frame: &mut Frame) {
    let Some(inner) = pane_body(area) else {
        return;
    };
    let width = usize::from(inner.width);
    let lines = pane_lines(model, pane, width);
    let visible = usize::from(inner.height);
    let scroll = pane_scroll(model, pane).min(lines.len().saturating_sub(visible));
    let shown: Vec<Line<'static>> = if lines.is_empty() {
        vec![Line::from(Span::styled(
            format!("No {} yet", pane.name()),
            theme::faint(),
        ))]
    } else {
        lines.into_iter().skip(scroll).take(visible).collect()
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(Line::from(Span::styled(
            format!(" {} ", pane.name()),
            theme::title(),
        )));
    frame.render_widget(block, area);
    paint_close(frame, area);
    frame.render_widget(Paragraph::new(shown), inner);
}

pub fn pane_close_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<RightPane> {
    if model.overlay.is_some() {
        return None;
    }
    right_pane_rects(model, area)
        .into_iter()
        .find_map(|(pane, rect)| {
            (pane_body(rect).is_some()
                && row == rect.y
                && column == rect.x.saturating_add(rect.width.saturating_sub(1)))
            .then_some(pane)
        })
}

fn pane_lines(model: &ScreenModel, pane: RightPane, width: usize) -> Vec<Line<'static>> {
    match pane {
        RightPane::Todos => {
            let mut lines = Vec::new();
            if !model.todos.is_empty() {
                lines.push(progress_line(&model.todos, width));
            }
            lines.extend(todo_entries(model, width).into_iter().map(|(line, _)| line));
            lines
        }
        RightPane::Closeout => closeout_content_lines(model, width)
            .into_iter()
            .map(|(line, _)| line)
            .collect(),
        RightPane::Tasks => task_entries(model, width)
            .into_iter()
            .map(|(line, _)| line)
            .collect(),
        RightPane::Schedules => schedule_entries(model, width),
        RightPane::Proof => proof::entries(model, width)
            .into_iter()
            .map(|(line, _)| line)
            .collect(),
    }
}

fn pane_line_count(model: &ScreenModel, pane: RightPane, width: usize) -> usize {
    match pane {
        RightPane::Todos => usize::from(!model.todos.is_empty()) + todo_entries(model, width).len(),
        RightPane::Closeout => closeout_content_lines(model, width).len(),
        RightPane::Tasks => task_entries(model, width).len(),
        RightPane::Schedules => model.schedules.len(),
        RightPane::Proof => proof::entries(model, width).len(),
    }
}

fn closeout_marker(model: &ScreenModel, status: CloseoutMark) -> String {
    match status {
        CloseoutMark::NotRequired => "-".to_string(),
        CloseoutMark::Missing => "\u{00b7}".to_string(),
        CloseoutMark::Running => FLUX_SPINNER[model.tick % FLUX_SPINNER.len()].to_string(),
        CloseoutMark::Passed => "\u{2713}".to_string(),
        CloseoutMark::Failed => "\u{2717}".to_string(),
    }
}

fn closeout_word(status: CloseoutMark) -> &'static str {
    match status {
        CloseoutMark::NotRequired => "not required",
        CloseoutMark::Missing => "missing",
        CloseoutMark::Running => "running",
        CloseoutMark::Passed => "passed",
        CloseoutMark::Failed => "failed",
    }
}

fn closeout_style(status: CloseoutMark) -> Style {
    match status {
        CloseoutMark::NotRequired | CloseoutMark::Missing => theme::faint(),
        CloseoutMark::Running => Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
        CloseoutMark::Passed => Style::default().fg(theme::good()),
        CloseoutMark::Failed => Style::default().fg(theme::bad()),
    }
}

fn strip_sgr(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn terminal_lines(text: &str, width: usize) -> Vec<String> {
    let clean = strip_sgr(text);
    if clean.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for line in clean.split('\n') {
        lines.extend(wrap(line, width.max(1)));
    }
    lines
}

fn closeout_content_lines(model: &ScreenModel, width: usize) -> Vec<(Line<'static>, String)> {
    let mut out = Vec::new();
    for item in &model.closeout {
        let marker = closeout_marker(model, item.status);
        let header = format!(
            "{marker} {}  {}  {}",
            item.id,
            item.kind,
            closeout_word(item.status)
        );
        out.push((
            Line::from(Span::styled(
                truncate_cols(&header, width),
                closeout_style(item.status),
            )),
            item.id.clone(),
        ));
        let requirement = if item.required {
            "Required for this turn"
        } else if item.status == CloseoutMark::NotRequired {
            "Not required; won't run"
        } else {
            "Not required"
        };
        for line in wrap(requirement, width.max(1)) {
            out.push((
                Line::from(Span::styled(line, theme::faint())),
                item.id.clone(),
            ));
        }
        if model.open_check.as_deref() != Some(item.id.as_str())
            && item.status != CloseoutMark::Running
        {
            continue;
        }
        if let Some(exit) = item.exit {
            out.push((
                Line::from(Span::styled(
                    truncate_cols(&format!("exit {exit}"), width),
                    theme::faint(),
                )),
                item.id.clone(),
            ));
        }
        for line in terminal_lines(&item.tail, width) {
            out.push((
                Line::from(Span::styled(truncate_cols(&line, width), theme::body())),
                item.id.clone(),
            ));
        }
    }
    out
}

pub fn closeout_pane_rect(model: &ScreenModel, area: Rect) -> Rect {
    pane_rect(model, area, RightPane::Closeout).unwrap_or(Rect::new(0, 0, 0, 0))
}

pub fn closeout_pane_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    right_pane_at(model, area, column, row) == Some(RightPane::Closeout)
}

pub fn closeout_row_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<String> {
    let (index, width) = pane_line_index(model, area, RightPane::Closeout, column, row)?;
    closeout_content_lines(model, width)
        .get(index)
        .map(|(_, id)| id.clone())
}

pub fn closeout_scroll_max(model: &ScreenModel, area: Rect) -> usize {
    pane_scroll_max(model, area, RightPane::Closeout)
}

pub fn pane_scroll_max(model: &ScreenModel, area: Rect, pane: RightPane) -> usize {
    let rect = match pane_rect(model, area, pane) {
        Some(rect) => rect,
        None => return 0,
    };
    let inner = match pane_body(rect) {
        Some(inner) => inner,
        None => return 0,
    };
    pane_line_count(model, pane, usize::from(inner.width)).saturating_sub(usize::from(inner.height))
}

pub fn right_pane_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<RightPane> {
    let point = Position { x: column, y: row };
    right_pane_rects(model, area)
        .into_iter()
        .find(|(_, rect)| rect.contains(point))
        .map(|(pane, _)| pane)
}

pub fn pane_title_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    right_pane_rects(model, area).into_iter().any(|(_, rect)| {
        row == rect.y && column > rect.x && column < rect.x.saturating_add(rect.width)
    })
}

pub fn task_row_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<String> {
    let (index, width) = pane_line_index(model, area, RightPane::Tasks, column, row)?;
    task_entries(model, width)
        .get(index)
        .map(|(_, id)| id.clone())
}

fn task_entries(model: &ScreenModel, width: usize) -> Vec<(Line<'static>, String)> {
    let mut out = Vec::new();
    for task in &model.tasks {
        let header = format!("{}  {}  {}", task.id, task.argv, task.state);
        out.push((
            Line::from(Span::styled(truncate_cols(&header, width), theme::body())),
            task.id.clone(),
        ));
        let Some(open) = model.open_task.as_ref().filter(|open| open.id == task.id) else {
            continue;
        };
        out.push((
            Line::from(Span::styled(
                truncate_cols(&open.argv, width),
                theme::body(),
            )),
            task.id.clone(),
        ));
        out.push((
            Line::from(Span::styled(
                truncate_cols(&open.state, width),
                theme::faint(),
            )),
            task.id.clone(),
        ));
        for line in terminal_lines(&open.tail, width) {
            out.push((
                Line::from(Span::styled(truncate_cols(&line, width), theme::body())),
                task.id.clone(),
            ));
        }
    }
    out
}

fn schedule_entries(model: &ScreenModel, width: usize) -> Vec<Line<'static>> {
    model
        .schedules
        .iter()
        .map(|row| {
            let text = format!("In {} min  {}", row.remaining_min, row.note);
            Line::from(Span::styled(truncate_cols(&text, width), theme::body()))
        })
        .collect()
}

fn progress_line(todos: &[TodoItem], width: usize) -> Line<'static> {
    let (_label, done, total, remaining) = progress_summary(todos);
    let counts = format!("{done}/{total}  {remaining} left");
    let counts_cols = UnicodeWidthStr::width(counts.as_str()).min(width);
    let shown = truncate_cols(&counts, counts_cols);
    let shown_cols = UnicodeWidthStr::width(shown.as_str());
    let bar_cols = width.saturating_sub(shown_cols).saturating_sub(1);
    let filled = (bar_cols * done).checked_div(total).unwrap_or(0);
    let mut bar = String::new();
    for i in 0..bar_cols {
        if i < filled {
            bar.push('\u{2588}');
        } else {
            bar.push('\u{2591}');
        }
    }
    let gap = width
        .saturating_sub(UnicodeWidthStr::width(bar.as_str()))
        .saturating_sub(shown_cols);
    Line::from(vec![
        Span::styled(bar, Style::default().fg(theme::accent())),
        Span::raw(" ".repeat(gap)),
        Span::styled(shown, theme::faint()),
    ])
}

pub fn progress_summary(todos: &[TodoItem]) -> (String, usize, usize, usize) {
    let total = todos.len();
    let done = todos
        .iter()
        .filter(|item| item.status == TodoStatus::Done)
        .count();
    let remaining = todos
        .iter()
        .filter(|item| item.status != TodoStatus::Done)
        .count();
    let label = todos
        .iter()
        .find(|item| item.status == TodoStatus::InProgress)
        .or_else(|| todos.iter().find(|item| item.status == TodoStatus::Pending))
        .map(|item| item.title.clone())
        .unwrap_or_default();
    (label, done, total, remaining)
}

fn truncate_cols(text: &str, cols: usize) -> String {
    if UnicodeWidthStr::width(text) <= cols {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > cols {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        text.chars().take(width).collect()
    } else {
        format!("{text}{}", " ".repeat(width - len))
    }
}

pub fn image_chip_name(image: &crate::attachment::ImageAttachment, width: u16) -> String {
    truncate_cols(&image.name, width.saturating_sub(9) as usize)
}

pub fn composer_framed(model: &ScreenModel, height: u16) -> bool {
    height >= 10
        && model.bottom_kind == Bottom::Prompt
        && model
            .selected_session()
            .is_some_and(|row| matches!(row.status, Status::Idle | Status::Working))
}
