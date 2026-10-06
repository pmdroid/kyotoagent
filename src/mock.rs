//! The hardcoded states the golden frames are drawn from.
//!
//! The issue hardcodes these on purpose, so they live here as one source of
//! truth: the golden tests and the `screens` example both read them, and the
//! live TUI will replace them with real session data later.

use std::path::PathBuf;

use crate::events::{TodoItem, TodoStatus};
use crate::screen::{
    Bottom, Card, Choice, CloseoutCheck, CloseoutMark, ItemKind, Outcome, Overlay, Phase,
    RightPane, ScheduleLine, ScreenModel, SessionRow, Status, TaskLine, Wait, TODOS_WIDTH,
};

/// The frames, in the order the example walks through them.
pub const FRAME_NAMES: [&str; 7] = [
    "waiting",
    "idle",
    "working",
    "closeout-run",
    "closeout-asked",
    "result-markdown",
    "closeout-requirements",
];

/// A closeout item as `.kyotoagent/closeout.yaml` names it. The frames read these
/// rather than repeating the strings, so the mock and the file cannot drift.
#[derive(Clone, Copy, Debug)]
pub struct PinnedItem {
    /// `id` in the file. It matches `^[a-z][a-z0-9-]*$`.
    pub id: &'static str,
    /// `kind` in the file.
    pub kind: ItemKind,
    /// `run` in the file. The harness runs it under `sh -c`.
    pub run: &'static str,
    /// `hint` in the file: what the model is told when the item is open.
    pub hint: &'static str,
}

/// The items this workspace pins. `command` is the only kind kyotoagent accepts so
/// far, and with no `retry` block every item allows three failed attempts.
pub const PINNED: &[PinnedItem] = &[
    PinnedItem {
        id: "test",
        kind: ItemKind::Command,
        run: "cargo test",
        hint: "cargo test",
    },
    PinnedItem {
        id: "lint",
        kind: ItemKind::Command,
        run: "cargo clippy --all-targets",
        hint: "cargo clippy --all-targets",
    },
];

/// With no `retry` block in the file, an item allows three failed attempts.
pub const CHECK_MAX_FAILURES: u32 = 3;

/// The item the closeout frames are about.
pub const CHECK: PinnedItem = PINNED[0];

/// The ids the file allows: lowercase, then lowercase, digits, and dashes.
pub const ID_RULE: &str = "^[a-z][a-z0-9-]*$";

/// What the model reads when it calls `finish` while a pinned item has no
/// current pass. This is a tool result, not a card: the turn stays open, no
/// result event is appended, and nothing about it reaches a frame. The model
/// needs it to know which id to retry, how many attempts are left, and what the
/// item was meant to do.
pub const CANNOT_FINISH_MISSING: &str =
    "Cannot finish yet. Check test is missing. Attempts 0 of 3. Hint: cargo test.";

/// The same sentence after a failed run: `failed` where `missing` stood, and
/// the count is the failures so far.
pub const CANNOT_FINISH_FAILED: &str =
    "Cannot finish yet. Check test is failed. Attempts 1 of 3. Hint: cargo test.";

fn home() -> PathBuf {
    PathBuf::from("/home/u")
}

fn row(id: &str, dir: &str, status: Status, waiting: Option<Wait>) -> SessionRow {
    SessionRow {
        id: id.to_string(),
        workspace: home().join("work").join(dir),
        title: None,
        status,
        waiting,
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        hidden: false,
        worktree: false,
        archived: false,
    }
}

/// The three sessions every frame shares, with `91bc` selected in `kyotoagent`.
fn kyotoagent_sessions(first: Status, first_waiting: Option<Wait>) -> Vec<SessionRow> {
    vec![
        row("91bc7a1d", "kyotoagent", first, first_waiting),
        row("3f2ae04c", "acpbot", Status::Working, None),
        row("ab1029f6", "notes", Status::Idle, None),
    ]
}

/// The ask the closeout frames share with [`waiting`].
const THE_ASK: &str = "Add a readme line that names the binary.";

/// A session blocked on a write permission, with its diff and the answer keys.
pub fn waiting() -> ScreenModel {
    ScreenModel {
        sessions: kyotoagent_sessions(Status::Waiting, Some(Wait::Permission)),
        selected: "91bc7a1d".to_string(),
        cards: vec![
            Card::ask(THE_ASK),
            Card::permission(
                "Replace README.md",
                None,
                &["@@ -1 +1,2 @@", "+kyotoagent is the binary."],
            ),
        ],
        bottom: "a once   s session   d deny".to_string(),
        bottom_kind: Bottom::Keys,
        home: home(),
        tick: 0,
        yolo: false,
        overlay: None,
        skill_picker: None,
        model: String::new(),
        effort: None,
        compacting: false,
        ..ScreenModel::default()
    }
}

pub fn result_markdown() -> ScreenModel {
    let mut model = idle();
    model.cards = vec![Card::result(
        "# Done\n\n- wrote README.md\n\n```rust\nfn main() {}\n```\n\n[docs](https://example.com)\n",
    )];
    model
}

pub fn result_table() -> ScreenModel {
    let mut model = idle();
    model.cards = vec![Card::result(
        "| tool | what |\n| --- | --- |\n| read | a file |\n| grep | a pattern |\n",
    )];
    model
}

pub fn result_wide_table() -> ScreenModel {
    let mut model = idle();
    model.left_open = false;
    model.cards = vec![Card::result(
        "\
| Repository | PR | Title | Approval | Conflicts | Draft |
| --- | --- | --- | --- | --- | --- |
| pmdroid/kyotoagent | [#87](https://github.com/pmdroid/kyotoagent/pull/87) | Install kyotoagent serve as a user service | None | No | No |
| pmdroid/barkvisor_private | [#4](https://github.com/pmdroid/barkvisor_private/pull/4) | Watch private builds | None | No | No |
| placeholder-tech/klar-magento-1 | [#1](https://github.com/placeholder-tech/klar-magento-1/pull/1) | Validate orders using response | None | No | No |
",
    )];
    model
}

pub fn result_gfm() -> ScreenModel {
    let mut model = idle();
    model.cards = vec![Card::result(
        "> hello\n\n~~gone~~\n\n- [x] ship\n\nsee https://example.com\n",
    )];
    model
}

pub fn chat() -> ScreenModel {
    let mut model = idle();
    model.cards = vec![
        Card::ask(THE_ASK),
        Card::result("The readme now names the binary under the heading Kyoto Agent."),
    ];
    model
}

pub fn answered() -> ScreenModel {
    let mut model = idle();
    model.cards.insert(
        0,
        Card::Answer {
            text: "Kyoto Agent".to_string(),
        },
    );
    model.cards.insert(
        0,
        Card::Question {
            text: "Which title should the heading use?".to_string(),
            choices: Vec::new(),
            answer: Some("Kyoto Agent".to_string()),
        },
    );
    model
}

/// A finished turn: the result and the proof.
pub fn idle() -> ScreenModel {
    let all_idle = |id: &str, dir: &str| row(id, dir, Status::Idle, None);
    ScreenModel {
        sessions: vec![
            all_idle("91bc7a1d", "kyotoagent"),
            all_idle("3f2ae04c", "acpbot"),
            all_idle("ab1029f6", "notes"),
        ],
        selected: "91bc7a1d".to_string(),
        cards: vec![
            Card::result("The readme now names the binary under the heading Kyoto Agent."),
            // Every closeout item the harness ran for this turn, in the order
            // they ran, each with the kind the file named. A finished turn that
            // ran none would list none.
            Card::proof(
                "cargo test passed on the readme heading.",
                &[
                    (PINNED[0].id, PINNED[0].kind, Outcome::Passed),
                    (PINNED[1].id, PINNED[1].kind, Outcome::Failed),
                ],
            ),
        ],
        bottom: ">".to_string(),
        bottom_kind: Bottom::Prompt,
        home: home(),
        tick: 0,
        yolo: false,
        overlay: None,
        skill_picker: None,
        model: String::new(),
        effort: None,
        compacting: false,
        ..ScreenModel::default()
    }
}

pub fn children() -> ScreenModel {
    let mut model = idle();
    let mut child = row("c0ffee00", "kyotoagent", Status::Working, None);
    child.parent_id = Some("91bc7a1d".into());
    child.title = Some("Read the tests".into());
    model.sessions.push(child);
    model
}

/// A session still working. Only its ask and the word `Working` are on screen.
///
/// A `closeout_run` event draws no card, so this is also the screen during a
/// closeout retry: the harness is running the check quietly.
pub fn working() -> ScreenModel {
    ScreenModel {
        sessions: vec![
            row("91bc7a1d", "kyotoagent", Status::Idle, None),
            row("3f2ae04c", "acpbot", Status::Working, None),
            row("ab1029f6", "notes", Status::Idle, None),
        ],
        selected: "3f2ae04c".to_string(),
        cards: vec![Card::ask("What is the package name in Cargo.toml?")],
        bottom: String::new(),
        bottom_kind: Bottom::Prompt,
        home: home(),
        tick: 0,
        yolo: false,
        overlay: None,
        skill_picker: None,
        model: String::new(),
        effort: None,
        compacting: false,
        phase: Some(Phase::Tool),
        action: Some("Reading".into()),
        ..ScreenModel::default()
    }
}

pub fn thinking() -> ScreenModel {
    let mut model = working();
    model.phase = Some(Phase::Thinking);
    model.action = None;
    model.thinking = "ponder the answer".to_string();
    model
}

/// A closeout check waiting on permission. The model asked for the check by id;
/// the harness built the command, so the card names the argv rather than a
/// write. Nothing has run yet, so the answer is the same three keys.
pub fn closeout_run() -> ScreenModel {
    ScreenModel {
        sessions: kyotoagent_sessions(Status::Waiting, Some(Wait::Permission)),
        selected: "91bc7a1d".to_string(),
        cards: vec![
            Card::ask(THE_ASK),
            Card::command(
                &format!("Run closeout {}", CHECK.id),
                None,
                &[&format!("sh -c '{}'", CHECK.run)],
            ),
        ],
        bottom: "a once   s session   d deny".to_string(),
        bottom_kind: Bottom::Keys,
        home: home(),
        tick: 0,
        yolo: false,
        overlay: None,
        skill_picker: None,
        model: String::new(),
        effort: None,
        compacting: false,
        ..ScreenModel::default()
    }
}

/// A closeout check that used all its failed attempts. The harness refuses
/// another run and opens a question instead. Neither choice is marked, because
/// the user has not answered yet, and there is no result card: choosing `stop`
/// ends the turn later, and choosing `continue` keeps it open.
pub fn closeout_asked() -> ScreenModel {
    ScreenModel {
        sessions: kyotoagent_sessions(Status::Waiting, Some(Wait::Question)),
        selected: "91bc7a1d".to_string(),
        cards: vec![
            Card::ask(THE_ASK),
            Card::question(
                &format!(
                    "Check {} used all {CHECK_MAX_FAILURES} failed attempts.",
                    CHECK.id
                ),
                &[("continue", false), ("stop", false)],
            ),
        ],
        bottom: String::new(),
        bottom_kind: Bottom::Prompt,
        home: home(),
        tick: 0,
        yolo: false,
        overlay: None,
        skill_picker: None,
        model: String::new(),
        effort: None,
        compacting: false,
        ..ScreenModel::default()
    }
}

pub fn projects_new() -> ScreenModel {
    let mut model = empty();
    model.overlay = Some(Overlay::Question {
        text: "Where should this session work?".to_string(),
        choices: vec![
            Choice {
                label: "This directory".to_string(),
                marked: false,
            },
            Choice {
                label: "kyotoagent".to_string(),
                marked: false,
            },
            Choice {
                label: "acpbot".to_string(),
                marked: false,
            },
            Choice {
                label: "Add project".to_string(),
                marked: false,
            },
        ],
        prompt: String::new(),
    });
    model
}

pub fn empty() -> ScreenModel {
    ScreenModel {
        sessions: Vec::new(),
        selected: String::new(),
        cards: Vec::new(),
        bottom: String::new(),
        bottom_kind: Bottom::Prompt,
        home: home(),
        tick: 0,
        yolo: false,
        overlay: None,
        skill_picker: None,
        model: String::new(),
        effort: None,
        compacting: false,
        ..ScreenModel::default()
    }
}

pub fn tasks() -> ScreenModel {
    let mut model = idle();
    model.tasks = vec![TaskLine {
        id: "ab12cd34".into(),
        argv: "sleep 30".into(),
        state: "running".into(),
    }];
    model
}

pub fn schedules() -> ScreenModel {
    let mut model = idle();
    model.schedules = vec![ScheduleLine {
        id: "cd34ef56".into(),
        note: "Check gh comments".into(),
        remaining_min: 10,
    }];
    model
}

pub fn todos() -> ScreenModel {
    let mut model = working();
    model.todos = vec![
        TodoItem {
            id: "read".into(),
            title: "Read the crate".into(),
            status: TodoStatus::Done,
            description: None,
            files: Vec::new(),
            links: Vec::new(),
        },
        TodoItem {
            id: "write".into(),
            title: "Write the todo tool".into(),
            status: TodoStatus::InProgress,
            description: Some(
                "Replace content with title. Keep the plan in description so the next turn can reread it.".into(),
            ),
            files: vec!["src/events.rs".into(), "src/turn.rs".into()],
            links: {
                let mut url = String::from("https:");
                url.push('/');
                url.push('/');
                url.push_str("docs.rs/serde");
                vec![url]
            },
        },
        TodoItem {
            id: "draw".into(),
            title: "Draw the right pane".into(),
            status: TodoStatus::Pending,
            description: None,
            files: Vec::new(),
            links: Vec::new(),
        },
    ];
    model.right_open = true;
    model.right_width = TODOS_WIDTH;
    model.right_panes.insert(RightPane::Todos);
    model
}

pub fn projects() -> ScreenModel {
    let mut model = idle();
    model.sessions[0].project = Some("kyotoagent".to_string());
    model.sessions[1].project = Some("acpbot".to_string());
    model
}

pub fn todos_wide() -> ScreenModel {
    let mut model = todos();
    model.right_width = crate::screen::wide_right_width(&model, 76);
    model
}

pub fn todos_open() -> ScreenModel {
    let mut model = todos_wide();
    model.open_todo = Some("write".into());
    model
}

fn pinned_checks() -> Vec<CloseoutCheck> {
    vec![
        CloseoutCheck {
            runs: Vec::new(),
            id: PINNED[0].id.into(),
            kind: PINNED[0].kind.label().into(),
            required: true,
            status: CloseoutMark::Passed,
            exit: Some(0),
            attempt: Some(1),
            tail: "ok".into(),
        },
        CloseoutCheck {
            runs: Vec::new(),
            id: PINNED[1].id.into(),
            kind: PINNED[1].kind.label().into(),
            required: true,
            status: CloseoutMark::Failed,
            exit: Some(1),
            attempt: Some(1),
            tail: "---\n\u{2514}\u{2500}\u{2500} fail".into(),
        },
    ]
}

pub fn closeout_strip() -> ScreenModel {
    let mut model = idle();
    model.closeout = pinned_checks();
    model
}

pub fn closeout_pane() -> ScreenModel {
    let mut model = closeout_strip();
    model.right_open = true;
    model.right_panes.insert(RightPane::Closeout);
    model.right_width = crate::screen::wide_right_width(&model, 76);
    model
}

pub fn closeout_requirements() -> ScreenModel {
    let mut model = closeout_pane();
    model.closeout[0].status = CloseoutMark::Missing;
    model.closeout[0].exit = None;
    model.closeout[0].attempt = None;
    model.closeout[0].tail.clear();
    model.closeout[1].status = CloseoutMark::NotRequired;
    model.closeout[1].required = false;
    model.closeout[1].exit = None;
    model.closeout[1].attempt = None;
    model.closeout[1].tail.clear();
    model
}

pub fn closeout_check() -> ScreenModel {
    let mut model = closeout_pane();
    model.open_check = Some(PINNED[1].id.into());
    model
}

/// The states, in the order the example walks through them.
pub fn all() -> Vec<(&'static str, ScreenModel)> {
    vec![
        (FRAME_NAMES[0], waiting()),
        (FRAME_NAMES[1], idle()),
        (FRAME_NAMES[2], working()),
        (FRAME_NAMES[3], closeout_run()),
        (FRAME_NAMES[4], closeout_asked()),
        (FRAME_NAMES[5], result_markdown()),
        (FRAME_NAMES[6], closeout_requirements()),
    ]
}
