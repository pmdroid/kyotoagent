//! The three session frames, drawn with ratatui's `TestBackend` and compared
//! with the golden files in `tests/screens`.
//!
//! The states come from `kyotoagent::mock`, which the `screens` example also uses,
//! so a live terminal and these files cannot drift apart.
//!
//! Run `UPDATE_GOLDENS=1 cargo test --test screens` to redraw them after a
//! deliberate layout change, then read the diff before keeping it.

use std::fs;
use std::path::PathBuf;

use kyotoagent::mock;
use kyotoagent::screen::render;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

const FRAME_WIDTH: u16 = 76;
const FRAME_HEIGHT: u16 = 24;

fn draw(model: &kyotoagent::screen::ScreenModel) -> String {
    let backend = TestBackend::new(FRAME_WIDTH, FRAME_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(model, frame.area(), frame))
        .expect("the screen draws into a test backend");
    text_of(terminal.backend().buffer())
}

/// The buffer as text, one line per row, with trailing blanks dropped.
fn text_of(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn assert_golden(name: &str, model: &kyotoagent::screen::ScreenModel) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/screens")
        .join(name);
    let drawn = draw(model);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        fs::create_dir_all(path.parent().expect("a screens directory"))
            .expect("create tests/screens");
        fs::write(&path, &drawn).expect("write the golden frame");
        return;
    }
    let golden = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "{} could not be read: {err}\nrun UPDATE_GOLDENS=1 cargo test --test screens",
            path.display()
        )
    });
    assert_eq!(drawn, golden, "the {name} frame changed");
}

#[test]
fn the_empty_frame_matches_its_golden() {
    assert_golden("empty.txt", &mock::empty());
}

#[test]
fn a_question_popup_dims_a_cell_outside_the_frame_and_paints_x() {
    use ratatui::style::Modifier;

    let model = mock::projects_new();
    let backend = TestBackend::new(FRAME_WIDTH, FRAME_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(&model, frame.area(), frame))
        .expect("the question popup draws");
    let buffer = terminal.backend().buffer().clone();
    let mut close = None;
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if buffer[(x, y)].symbol() == "x" {
                close = Some((x, y));
            }
        }
    }
    let (x, y) = close.expect("the popup paints x");
    assert!(
        !buffer[(x, y)].modifier.contains(Modifier::DIM),
        "x sits inside the frame"
    );
    assert!(
        buffer[(0, 0)].modifier.contains(Modifier::DIM),
        "a cell outside the frame is dim"
    );
    let mut inside = false;
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        if let Some(at) = line.find("Where") {
            let cell = line[..at].chars().count() as u16;
            inside = !buffer[(cell, y)].modifier.contains(Modifier::DIM);
            break;
        }
    }
    assert!(inside, "the question text stays undimmed");

    let mut menu = mock::idle();
    menu.overlay = Some(kyotoagent::screen::Overlay::Menu {
        id: "91bc7a1d".into(),
        items: vec!["Delete session".into()],
        column: 4,
        row: 3,
    });
    let backend = TestBackend::new(FRAME_WIDTH, FRAME_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(&menu, frame.area(), frame))
        .expect("the session menu draws");
    let buffer = terminal.backend().buffer().clone();
    assert!(
        buffer[(0, 0)].modifier.contains(Modifier::DIM),
        "the menu dims the rest of the screen"
    );
    let mut saw_x = false;
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if buffer[(x, y)].symbol() == "x" {
                saw_x = true;
            }
        }
    }
    assert!(!saw_x, "the session menu has no x");
}

#[test]
fn the_projects_new_frame_matches_its_golden() {
    let model = mock::projects_new();
    let drawn = draw(&model);
    assert!(drawn.contains("This directory"), "{drawn}");
    assert!(drawn.contains("kyotoagent"), "{drawn}");
    assert!(drawn.contains("acpbot"), "{drawn}");
    assert_golden("projects-new.txt", &model);
}

#[test]
fn the_splash_rectangle_is_centred_in_the_right_pane() {
    use kyotoagent::screen::session_inner;
    use kyotoagent::splash;
    use ratatui::layout::Rect;

    let screen = Rect::new(0, 0, FRAME_WIDTH, FRAME_HEIGHT);
    let pane = session_inner(screen);
    let splash = splash::rect_in(pane);
    assert!(
        pane.x >= 30,
        "the splash sits in the session pane, not under the list"
    );
    let left = i32::from(splash.x.saturating_sub(pane.x));
    let right = i32::from((pane.x + pane.width).saturating_sub(splash.x + splash.width));
    let top = i32::from(splash.y.saturating_sub(pane.y));
    let bottom = i32::from((pane.y + pane.height).saturating_sub(splash.y + splash.height));
    assert!(
        (left - right).abs() <= 1,
        "left {left} right {right} pane {pane:?} splash {splash:?}"
    );
    assert!((top - bottom).abs() <= 1, "top {top} bottom {bottom}");
}

#[test]
fn a_view_with_one_ask_has_no_splash() {
    let empty = draw(&mock::empty());
    assert!(!empty.contains('\u{2580}'), "the empty pane stays clear");
    let working = draw(&mock::working());
    assert!(
        !working.contains('\u{2580}'),
        "an ask hides the dog: {working}"
    );
    assert!(working.contains("What is the package name in"));
    assert!(working.contains("Cargo.toml?"));
}

#[test]
fn the_waiting_frame_matches_its_golden() {
    assert_golden("waiting.txt", &mock::waiting());
}

#[test]
fn the_idle_frame_matches_its_golden() {
    assert_golden("idle.txt", &mock::idle());
}

#[test]
fn the_closeout_strip_frame_matches_its_golden() {
    let model = mock::closeout_strip();
    let drawn = draw(&model);
    assert!(!drawn.contains("\u{2713} test"), "{drawn}");
    assert_golden("closeout-strip.txt", &model);
}

#[test]
fn the_closeout_pane_frame_matches_its_golden() {
    let model = mock::closeout_pane();
    let drawn = draw(&model);
    assert!(drawn.contains("closeout"), "{drawn}");
    assert!(drawn.contains("test"), "{drawn}");
    assert!(drawn.contains("lint"), "{drawn}");
    assert!(drawn.contains("passed"), "{drawn}");
    assert!(drawn.contains("failed"), "{drawn}");
    assert_golden("closeout-pane.txt", &model);
}

#[test]
fn the_closeout_requirements_frame_matches_its_golden() {
    let model = mock::closeout_requirements();
    let drawn = draw(&model);
    assert!(drawn.contains("Required for this turn"), "{drawn}");
    assert!(drawn.contains("Not required; won't run"), "{drawn}");
    assert_eq!(drawn.matches("missing").count(), 1, "{drawn}");
    assert_golden("closeout-requirements.txt", &model);
}

#[test]
fn the_closeout_check_frame_matches_its_golden() {
    let model = mock::closeout_check();
    let drawn = draw(&model);
    assert!(drawn.contains("\u{2717} lint  command  failed"), "{drawn}");
    assert!(drawn.contains("exit 1"), "{drawn}");
    assert!(drawn.contains("---"), "{drawn}");
    assert!(drawn.contains("\u{2514}\u{2500}\u{2500} fail"), "{drawn}");
    assert_golden("closeout-check.txt", &model);
}

#[test]
fn the_context_overlay_matches_its_golden() {
    use kyotoagent::screen::{ContextLine, Overlay};

    let mut model = mock::idle();
    let cards = model.cards.clone();
    model.context_percent = Some(12);
    model.overlay = Some(Overlay::Context {
        percent: 12,
        used: 31_200,
        reported_prompt_tokens: Some(30_000),
        window: 256_000,
        buckets: vec![
            ContextLine {
                id: "system".into(),
                tokens: Some(2_100),
            },
            ContextLine {
                id: "tools".into(),
                tokens: Some(4_800),
            },
            ContextLine {
                id: "skills".into(),
                tokens: Some(400),
            },
            ContextLine {
                id: "messages".into(),
                tokens: Some(23_900),
            },
            ContextLine {
                id: "free".into(),
                tokens: Some(224_800),
            },
        ],
    });
    let drawn = draw(&model);
    assert!(drawn.contains(kyotoagent::screen::LIST_GLYPH), "{drawn}");
    assert!(drawn.contains(" Kyoto Agent "), "{drawn}");
    assert!(drawn.contains("12%"), "{drawn}");
    assert!(drawn.contains("CONTEXT  12%"), "{drawn}");
    assert!(drawn.contains("31,200 / 256,000"), "{drawn}");
    for id in ["system", "tools", "skills", "messages", "free"] {
        assert!(drawn.contains(id), "{id} missing: {drawn}");
    }
    assert_eq!(model.cards, cards);
    assert_golden("context.txt", &model);
}

#[test]
fn the_result_wide_table_frame_matches_its_golden() {
    let model = mock::result_wide_table();
    let drawn = draw(&model);
    assert!(drawn.contains("Repository"), "{drawn}");
    assert!(drawn.contains("PR"), "{drawn}");
    assert!(drawn.contains("Title"), "{drawn}");
    assert!(drawn.contains("#87"), "{drawn}");
    assert!(!drawn.contains("https://"), "{drawn}");
    assert!(
        drawn
            .lines()
            .any(|line| line.contains("kyotoagent") && line.contains("Install")),
        "{drawn}"
    );
    assert!(
        drawn.lines().any(|line| line.contains("klar-magento")),
        "{drawn}"
    );
    assert!(drawn.contains("Validate"), "{drawn}");
    assert!(
        drawn
            .lines()
            .any(|line| line.contains("#87") && line.contains("None") && line.contains("No")),
        "{drawn}"
    );
    let backend = TestBackend::new(FRAME_WIDTH, FRAME_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(&model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    let mut linked = false;
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width.saturating_sub(1) {
            let cell = &buffer[(x, y)];
            if cell.symbol() == "#"
                && buffer[(x + 1, y)].symbol() == "8"
                && buffer[(x + 2, y)].symbol() == "7"
            {
                linked = cell.modifier.contains(ratatui::style::Modifier::UNDERLINED);
            }
        }
    }
    assert!(linked, "#87 is a link");
    assert_golden("result-wide-table.txt", &model);
}

#[test]
fn the_result_table_frame_matches_its_golden() {
    let model = mock::result_table();
    let drawn = draw(&model);
    assert!(drawn.contains("tool"), "{drawn}");
    assert!(drawn.contains("what"), "{drawn}");
    assert!(drawn.contains("read"), "{drawn}");
    assert!(drawn.contains("a file"), "{drawn}");
    assert!(drawn.contains("grep"), "{drawn}");
    assert!(drawn.contains("a pattern"), "{drawn}");
    assert!(drawn.contains('─'), "{drawn}");
    for line in drawn.lines() {
        assert!(!line.contains('|'), "{line}");
    }
    assert_golden("result-table.txt", &model);
}

#[test]
fn the_result_gfm_frame_matches_its_golden() {
    let model = mock::result_gfm();
    let drawn = draw(&model);
    assert!(drawn.contains("▎ hello"), "{drawn}");
    assert!(drawn.contains("gone"), "{drawn}");
    assert!(!drawn.contains("~~"), "{drawn}");
    assert!(drawn.contains("[x] ship"), "{drawn}");
    assert!(drawn.contains("https://example.com"), "{drawn}");
    assert_golden("result-gfm.txt", &model);
}

#[test]
fn the_result_markdown_frame_matches_its_golden() {
    let model = mock::result_markdown();
    let drawn = draw(&model);
    assert!(drawn.contains("Done"), "{drawn}");
    assert!(drawn.contains("wrote README.md"), "{drawn}");
    assert!(drawn.contains("fn main()"), "{drawn}");
    assert!(drawn.contains("https://example.com"), "{drawn}");
    assert_golden("result-markdown.txt", &model);
    let backend = TestBackend::new(FRAME_WIDTH, FRAME_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(&model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    let mut underlined = false;
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            if cell.symbol() == "d"
                && buffer[(x + 1, y)].symbol() == "o"
                && buffer[(x + 2, y)].symbol() == "c"
                && buffer[(x + 3, y)].symbol() == "s"
            {
                underlined = cell.modifier.contains(ratatui::style::Modifier::UNDERLINED);
            }
        }
    }
    assert!(underlined, "the docs label is underlined");
}

#[test]
fn the_chat_frame_matches_its_golden() {
    let model = mock::chat();
    let drawn = draw(&model);
    assert!(drawn.contains("Add a readme line"));
    assert!(drawn.contains("names the binary."));
    assert!(drawn.contains("RESULT"));
    assert!(drawn.contains("The readme now names"));
    assert_golden("chat.txt", &model);
}

#[test]
fn the_help_frame_matches_its_golden() {
    use kyotoagent::prompt::SkillEntry;
    use kyotoagent::screen::Overlay;
    use kyotoagent::tui::command_lines;

    let rows = command_lines(&[SkillEntry {
        name: "preflight".into(),
        description: "Ship the closeout".into(),
        ..SkillEntry::default()
    }]);
    for name in ["/todos", "/tasks", "/schedules", "/closeout"] {
        assert!(rows.iter().any(|row| row.name == name), "{name}");
    }
    assert!(rows.iter().all(|row| row.name != "preflight"));
    assert!(rows.iter().any(|row| row.name == "/compact"), "{rows:?}");
    assert!(rows.iter().any(|row| row.name == "/enhance"));
    let mut model = mock::idle();
    model.bottom.clear();
    model.overlay = Some(Overlay::Help { rows, scroll: 0 });
    let drawn = draw(&model);
    assert!(drawn.contains("New session"), "{drawn}");
    assert!(drawn.contains("Ctrl-T"), "{drawn}");
    assert!(drawn.contains("Ctrl-B"), "{drawn}");
    assert!(drawn.contains("Ctrl-G"), "{drawn}");
    assert!(drawn.contains("Sessions"), "{drawn}");
    assert!(drawn.contains("Panes"), "{drawn}");

    assert!(drawn.contains("Profile"), "{drawn}");
    assert!(drawn.contains("Server"), "{drawn}");
    assert!(drawn.contains("Delete session"), "{drawn}");
    assert!(drawn.contains("Enhance"), "{drawn}");
    assert!(drawn.contains("rewrite a short prompt"), "{drawn}");
    assert_golden("help.txt", &model);
    if let Some(Overlay::Help { rows, scroll }) = &mut model.overlay {
        *scroll = rows.iter().position(|row| row.name == "/model").unwrap();
    }
    assert!(draw(&model).contains("/model"));
}

#[test]
fn the_enhance_frame_matches_its_golden() {
    let mut model = mock::idle();
    model.enhance = true;
    model.cards = vec![kyotoagent::screen::Card::Enhance {
        source: "ship it".into(),
        text: "Do the thing carefully.".into(),
        error: None,
        event_id: "e1".into(),
    }];
    model.overlay = Some(kyotoagent::screen::Overlay::Enhance {
        source: "ship it".into(),
        text: "Do the thing carefully.".into(),
        error: None,
    });
    model.bottom.clear();
    let drawn = draw(&model);
    assert!(drawn.contains("ship it"), "{drawn}");
    assert!(drawn.contains("Do the thing carefully."), "{drawn}");
    assert!(drawn.contains("u use"), "{drawn}");
    assert!(drawn.contains('x'), "{drawn}");
    assert_golden("enhance.txt", &model);
}

#[test]
fn the_child_row_sits_under_its_parent() {
    let model = mock::children();
    let drawn = draw(&model);
    assert!(drawn.contains("4 sessions"), "{drawn}");
    assert!(drawn.contains("Read the tests"), "{drawn}");
    assert!(!drawn.contains("spawn_subagent"), "{drawn}");
    let list = list_column(&drawn);
    let parent = list.find("kyotoagent").expect("the parent");
    let child = list.find("Read the tests").expect("the child");
    let acpbot = list.find("acpbot").expect("the next session");
    assert!(parent < child && child < acpbot, "{list}");
    assert_golden("children.txt", &model);
}

#[test]
fn the_projects_frame_matches_its_golden() {
    assert_golden("projects.txt", &mock::projects());
}

#[test]
fn two_sessions_under_one_project_share_a_header() {
    let mut model = mock::projects();
    let mut extra = model.sessions[0].clone();
    extra.id = "22aa0000".into();
    extra.workspace = PathBuf::from("/home/u/work/kyotoagent/src");
    extra.title = Some("Nested task".into());
    model.sessions.insert(0, extra);
    let drawn = draw(&model);
    let list = list_column(&drawn);
    assert!(list.contains("kyotoagent"), "{list}");
    assert!(!list.contains("91bc"), "{list}");
    assert!(!list.contains("22aa"), "{list}");
    assert!(list.contains("other"), "{list}");
    assert!(list.contains("notes"), "{list}");
    let header = list
        .lines()
        .position(|line| line.contains('\u{25be}') && line.contains("kyotoagent"))
        .expect("the kyotoagent header");
    let first = list
        .lines()
        .position(|line| line.contains("Nested task"))
        .expect("nested task");
    let second = list
        .lines()
        .enumerate()
        .find(|(index, line)| *index > first && line.contains("kyotoagent"))
        .map(|(index, _)| index)
        .expect("second session");
    let other = list
        .lines()
        .position(|line| line.contains("other"))
        .expect("other");
    assert!(header < first && first < second && second < other, "{list}");
}

fn list_column(frame: &str) -> String {
    frame
        .lines()
        .map(|line| {
            let chars: Vec<char> = line.chars().collect();
            chars.into_iter().take(30).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_working_frame_matches_its_golden() {
    assert_golden("working.txt", &mock::working());
}

#[test]
fn the_thinking_frame_matches_its_golden() {
    let model = mock::thinking();
    let drawn = draw(&model);
    assert!(drawn.contains("Thinking"), "{drawn}");
    assert!(!drawn.contains("ponder the answer"), "{drawn}");
    assert_golden("thinking.txt", &model);
}

#[test]
fn thinking_at_hits_only_the_footer_row() {
    use kyotoagent::screen::{thinking_at, Phase};
    use ratatui::layout::Rect;

    let area = Rect::new(0, 0, FRAME_WIDTH, FRAME_HEIGHT);
    let model = mock::thinking();
    let mut rows = Vec::new();
    for y in 0..FRAME_HEIGHT {
        for x in 0..FRAME_WIDTH {
            if thinking_at(&model, area, x, y) {
                rows.push(y);
                break;
            }
        }
    }
    assert_eq!(rows.len(), 1, "the thinking footer is one row");
    let y = rows[0];
    assert!(!thinking_at(&model, area, 0, y.saturating_sub(1)));
    let working = mock::working();
    assert_eq!(working.phase, Some(Phase::Tool));
    for y in 0..FRAME_HEIGHT {
        for x in 0..FRAME_WIDTH {
            assert!(!thinking_at(&working, area, x, y));
        }
    }
}

#[test]
fn the_todos_frame_matches_its_golden() {
    assert_golden("todos.txt", &mock::todos());
}

#[test]
fn the_wide_todos_frame_matches_its_golden() {
    use kyotoagent::screen::{split_of, CARD_MIN};
    use ratatui::layout::Rect;

    let model = mock::todos_wide();
    assert!(model.open_todo.is_none());
    let split = split_of(&model, Rect::new(0, 0, FRAME_WIDTH, FRAME_HEIGHT));
    assert_eq!(split.session.width, CARD_MIN);
    let drawn = draw(&model);
    assert!(!drawn.contains("Replace content with title"), "{drawn}");
    assert_golden("todos-wide.txt", &model);
}

#[test]
fn the_open_todo_frame_matches_its_golden() {
    let model = mock::todos_open();
    assert_eq!(model.open_todo.as_deref(), Some("write"));
    assert!(model.overlay.is_none());
    let drawn = draw(&model);
    assert!(drawn.contains("Replace content with"), "{drawn}");
    assert!(drawn.contains("src/events.rs"), "{drawn}");
    assert!(drawn.contains("in_progress"), "{drawn}");
    assert_golden("todos-open.txt", &model);
}

#[test]
fn the_tasks_frame_matches_its_golden() {
    assert_golden("tasks.txt", &mock::tasks());
}

#[test]
fn the_schedules_frame_matches_its_golden() {
    let model = mock::schedules();
    let drawn = draw(&model);
    assert!(!drawn.contains("In 10 min"), "{drawn}");
    assert!(!drawn.contains(" due"), "{drawn}");
    assert_golden("schedules.txt", &model);
}

#[test]
fn a_schedule_does_not_add_a_row_above_the_input() {
    use kyotoagent::screen::split_of;
    use ratatui::layout::Rect;

    let area = Rect::new(0, 0, FRAME_WIDTH, FRAME_HEIGHT);
    let model = mock::schedules();
    let split = split_of(&model, area);
    assert_eq!(split.input.y, split.session.y + split.session.height);
    assert!(!draw(&model).contains("In 10 min"));
}

#[test]
fn quiet_frames_keep_the_input_under_the_session() {
    use kyotoagent::screen::split_of;
    use ratatui::layout::Rect;

    let area = Rect::new(0, 0, FRAME_WIDTH, FRAME_HEIGHT);
    for model in [
        mock::idle(),
        mock::waiting(),
        mock::working(),
        mock::todos(),
        mock::tasks(),
        mock::schedules(),
    ] {
        let split = split_of(&model, area);
        assert_eq!(split.input.y, split.session.y + split.session.height);
    }
}

#[test]
fn the_closeout_run_frame_matches_its_golden() {
    assert_golden("closeout-run.txt", &mock::closeout_run());
}

#[test]
fn the_closeout_asked_frame_matches_its_golden() {
    assert_golden("closeout-asked.txt", &mock::closeout_asked());
}

#[test]
fn the_answered_frame_matches_its_golden() {
    assert_golden("answered.txt", &mock::answered());
}

#[test]
fn the_answered_frame_keeps_the_question_and_the_label() {
    let drawn = draw(&mock::answered());
    assert!(
        drawn.contains("Which title should the heading use?"),
        "{drawn}"
    );
    assert!(drawn.contains("QUESTION"), "{drawn}");
    assert!(drawn.contains("ANSWER"), "{drawn}");
    let question = drawn
        .lines()
        .find(|line| line.contains("QUESTION"))
        .expect("the question");
    let answer = drawn
        .lines()
        .find(|line| line.contains("ANSWER"))
        .expect("the answer");
    assert!(answer.find("ANSWER").unwrap() > question.find("QUESTION").unwrap());
    let label = drawn
        .lines()
        .find(|line| line.contains("Kyoto Agent ▎"))
        .expect("the label");
    assert!(label.find("Kyoto Agent").unwrap() > question.find("QUESTION").unwrap());
    assert!(!label.contains('1'), "{label}");
    assert!(!label.contains('\u{25cf}'), "{label}");
    assert!(drawn.contains("The readme now names the"));
}

fn golden(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/screens")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// The lines each frame has to carry, so a regenerated golden cannot quietly
/// lose the thing a reader is looking for.
#[test]
fn the_golden_frames_carry_their_lines() {
    let empty = golden("empty.txt");
    assert!(empty.contains("Kyoto Agent"), "empty heading");
    assert!(!empty.contains("PERMISSION"), "empty has no cards");

    let waiting = golden("waiting.txt");
    assert!(
        waiting.contains("+kyotoagent is the binary."),
        "waiting diff"
    );
    assert!(waiting.contains("a once"), "waiting keys");
    assert!(waiting.contains("s session"), "waiting keys");
    assert!(waiting.contains("d deny"), "waiting keys");

    let idle = golden("idle.txt");
    assert!(
        !idle.contains("Allowed replace of README.md"),
        "idle drops the answered permission"
    );
    assert!(
        !idle.contains("QUESTION"),
        "idle drops the answered question"
    );
    assert!(
        !idle.contains("Which title should the heading use?"),
        "idle drops the answered question"
    );
    assert!(idle.contains("cargo test passed on the"), "idle proof");
    assert!(idle.contains("readme heading."), "idle proof");
    assert!(!idle.contains("M README.md"), "idle proof drops git status");
    assert!(
        !idle.contains("README.md | 1 +"),
        "idle proof drops the diffstat"
    );
    assert!(idle.contains("The readme now names the"), "idle result");
    assert!(idle.contains("under the heading"), "idle result");
    assert!(idle.contains("Kyoto Agent."), "idle result");
    assert!(
        !idle.contains("+kyotoagent is the binary."),
        "idle keeps no diff"
    );
    assert!(
        !idle.contains("\u{2713} test  command  passed"),
        "closeout ticks stay off the card"
    );
    assert!(
        !idle.contains("\u{2717} lint  command  failed"),
        "closeout ticks stay off the card"
    );

    let working = golden("working.txt");
    assert!(working.contains("Reading"), "working indicator");
    assert!(
        working.contains(" \u{203a} \u{2588}"),
        "working draws the empty prompt"
    );
    assert!(!working.contains("permission"), "working has no permission");
    assert!(!working.contains("proof"), "working has no proof");

    // A closeout check waits on a command the harness built, not on a write.
    // The card names the check and the argv it is about to run.
    let run = golden("closeout-run.txt");
    assert!(run.contains("Run closeout test"), "closeout run action");
    assert!(run.contains("sh -c 'cargo test'"), "closeout run argv");
    assert!(
        run.contains("waiting permission"),
        "the session row names the card it waits on"
    );
    assert!(run.contains("a once"), "closeout run keys");
    assert!(run.contains("s session"), "closeout run keys");
    assert!(run.contains("d deny"), "closeout run keys");
    assert!(
        run.contains("Add a readme line"),
        "the ask is still on screen"
    );

    let asked = golden("closeout-asked.txt");
    assert!(
        asked.contains("Check test used all 3 failed attempts."),
        "the question names the check and the attempt count"
    );
    assert!(
        asked.contains("waiting question"),
        "the row names the question"
    );
    assert!(asked.contains("1  continue"), "the first numbered choice");
    assert!(asked.contains("2  stop"), "the second numbered choice");
    assert!(!asked.contains("RESULT"), "no result card");
    assert_eq!(
        asked.lines().last().expect("a bottom row").trim(),
        "\u{203a} \u{2588}"
    );

    let todos = golden("todos.txt");
    assert!(
        todos.contains("Write the todo tool"),
        "progress names the step"
    );
    assert!(todos.contains("Read the crate"), "done item");
    assert!(todos.contains("Draw the right pane"), "pending item");
    assert!(todos.contains("1/3"), "bar counts");
}

/// The bottom row answers a card with keys, laid out the same way whatever the
/// card is: the model carries the pairs, the row draws them.
#[test]
fn both_answer_rows_are_spaced_the_same_way() {
    let bottom = |name: &str| {
        golden(name)
            .lines()
            .last()
            .expect("every frame has a bottom row")
            .trim()
            .to_string()
    };
    assert_eq!(
        bottom("waiting.txt"),
        "\u{203a} a once  s session  d deny\u{2588}"
    );
    assert_eq!(bottom("closeout-asked.txt"), "\u{203a} \u{2588}");
    for (name, pairs) in [("waiting.txt", 3), ("closeout-run.txt", 3)] {
        let row = bottom(name);
        let gaps = row.matches("  ").count();
        assert_eq!(
            gaps,
            pairs - 1,
            "{name} separates {pairs} pairs wrongly: {row:?}"
        );
    }
    assert_eq!(mock::closeout_asked().bottom, "");
    assert_eq!(
        mock::closeout_asked().bottom_kind,
        kyotoagent::screen::Bottom::Prompt
    );
    assert_eq!(mock::waiting().bottom, "a once   s session   d deny");
}

/// The sentence that stops a model calling `finish` before a check passes.
///
/// It is a tool result the model reads, never a card, so it must not appear on
/// any frame. The turn stays open and the screen is unchanged, which is the
/// point: a missing check is the model's problem to fix, not a card for the
/// user to answer.
/// The frames name closeout items the way `.kyotoagent/closeout.yaml` does, so the
/// id the model passes to `run_closeout` is the id the file stored.
#[test]
fn every_pinned_id_is_one_the_file_accepts() {
    assert_eq!(mock::ID_RULE, "^[a-z][a-z0-9-]*$");
    let accepts = |id: &str| {
        let mut chars = id.chars();
        match chars.next() {
            Some(first) if first.is_ascii_lowercase() => {
                chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            }
            _ => false,
        }
    };
    for item in mock::PINNED {
        assert!(accepts(item.id), "{} is not an id the file takes", item.id);
        assert!(!item.run.is_empty(), "{} has no run", item.id);
        assert!(!item.hint.is_empty(), "{} has no hint", item.id);
    }
}

#[test]
fn the_cannot_finish_sentence_stays_off_every_frame() {
    assert_eq!(
        mock::CANNOT_FINISH_MISSING,
        "Cannot finish yet. Check test is missing. Attempts 0 of 3. Hint: cargo test."
    );
    // After a failure the same sentence says failed, and counts the failures.
    assert_eq!(
        mock::CANNOT_FINISH_FAILED,
        "Cannot finish yet. Check test is failed. Attempts 1 of 3. Hint: cargo test."
    );

    for name in mock::FRAME_NAMES {
        let frame = golden(&format!("{name}.txt"));
        assert!(
            !frame.contains("Cannot finish yet"),
            "{name} shows the tool result the model should read instead"
        );
        assert!(
            !frame.contains(mock::CANNOT_FINISH_MISSING),
            "{name} shows the missing-check sentence"
        );
        assert!(
            !frame.contains(mock::CANNOT_FINISH_FAILED),
            "{name} shows the failed-check sentence"
        );
    }
}

/// The frames are drawn in colour, and the colour is part of the design, not
/// decoration: it says which kind of card this is and whether an allow landed.
/// A frame that lost its colour would still pass the text tests, so the buffer
/// is checked for the marks the design depends on.
#[test]
fn the_frames_carry_their_colour() {
    use ratatui::style::Color;

    fn cells(name: &str) -> Vec<ratatui::buffer::Cell> {
        let backend = TestBackend::new(FRAME_WIDTH, FRAME_HEIGHT);
        let mut terminal = Terminal::new(backend).expect("a test terminal");
        let model = match name {
            "waiting" => mock::waiting(),
            "idle" => mock::idle(),
            "working" => mock::working(),
            "closeout-run" => mock::closeout_run(),
            "closeout-asked" => mock::closeout_asked(),
            "closeout-requirements" => mock::closeout_requirements(),
            "result-markdown" => mock::result_markdown(),
            other => panic!("no frame called {other}"),
        };
        terminal
            .draw(|frame| render(&model, frame.area(), frame))
            .expect("the screen draws into a test backend");
        let buffer = terminal.backend().buffer().clone();
        let (width, height) = (buffer.area.width, buffer.area.height);
        let mut out = Vec::with_capacity(usize::from(width) * usize::from(height));
        for y in 0..height {
            for x in 0..width {
                out.push(buffer[(x, y)].clone());
            }
        }
        out
    }

    for name in mock::FRAME_NAMES {
        let frame = cells(name);
        let colors: Vec<Color> = frame.iter().map(|cell| cell.fg).collect();
        assert!(
            colors.len() > 20,
            "{name} has almost no colour: {} cells",
            colors.len()
        );
        // A waiting or a working session is called out, so its status colour
        // has to be on screen.
        if name != "idle" && name != "result-markdown" {
            assert!(
                colors.contains(&Color::Cyan),
                "{name} does not light the working status"
            );
        }
    }

    // The waiting frame's diff is green and its card kind is lit; the idle
    // frame's allowed permission is green too.
    for name in ["waiting", "idle"] {
        assert!(
            cells(name).iter().any(|cell| cell.fg == Color::Green),
            "{name} has nothing in green"
        );
    }
}

/// The selected session is a filled band, not a one-word highlight.
#[test]
fn the_selected_session_is_filled() {
    use ratatui::style::Color;

    let backend = TestBackend::new(FRAME_WIDTH, FRAME_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    let model = mock::idle();
    terminal
        .draw(|frame| render(&model, frame.area(), frame))
        .expect("the screen draws into a test backend");
    let buffer = terminal.backend().buffer();
    let fill = Color::Rgb(0x1c, 0x2c, 0x38);
    let filled = (0..buffer.area.height)
        .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
        .filter(|(x, y)| buffer[(*x, *y)].bg == fill)
        .count();
    assert!(
        filled > 40,
        "the selected row is not a band: only {filled} cells are filled"
    );
}

/// A tool name never reaches a frame. The agent's reads stay quiet.
#[test]
fn no_frame_names_a_tool() {
    for name in mock::FRAME_NAMES {
        let frame = golden(&format!("{name}.txt"));
        assert!(!frame.contains("read_file"), "{name} names a tool");
        assert!(!frame.contains("write_file"), "{name} names a tool");
        assert!(!frame.contains("list_dir"), "{name} names a tool");
        assert!(!frame.contains("tool_call"), "{name} names a tool");
        assert!(!frame.contains("AGENTS.md"), "{name} names AGENTS.md");
    }
}

#[test]
fn a_detail_row_wider_than_the_list_pane_still_draws() {
    let mut model = mock::waiting();
    for row in model.sessions.iter_mut() {
        row.pull_url = Some("https://github.com/pmdroid/kyotoagent/pull/12345".to_string());
    }
    draw(&model);

    model.left_width = 8;
    draw(&model);
}

fn buffer_of(model: &kyotoagent::screen::ScreenModel) -> Buffer {
    let backend = TestBackend::new(FRAME_WIDTH, FRAME_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| render(model, frame.area(), frame))
        .expect("the screen draws");
    terminal.backend().buffer().clone()
}

fn list_line(buffer: &Buffer, y: u16) -> String {
    let mut line = String::new();
    for x in 0..30 {
        line.push_str(buffer[(x, y)].symbol());
    }
    line
}

fn session_name_row(buffer: &Buffer, name: &str, status: &str) -> u16 {
    for y in 0..buffer.area.height.saturating_sub(1) {
        if list_line(buffer, y).contains(name) && list_line(buffer, y + 1).contains(status) {
            return y;
        }
    }
    panic!("no name row for {name}");
}

fn mark_color(buffer: &Buffer, y: u16, symbol: &str) -> ratatui::style::Color {
    for x in 0..30 {
        let cell = &buffer[(x, y)];
        if cell.symbol() == symbol {
            return cell.fg;
        }
    }
    panic!("{symbol} missing on {}", list_line(buffer, y));
}

#[test]
fn the_list_marks_idle_done_and_waiting_needs_help() {
    use ratatui::style::Color;

    let waiting = buffer_of(&mock::waiting());
    let y = session_name_row(&waiting, "kyotoagent", "waiting permission");
    assert_eq!(mark_color(&waiting, y, "!"), Color::Yellow);
    let notes = session_name_row(&waiting, "notes", "idle");
    assert_eq!(mark_color(&waiting, notes, "\u{2713}"), Color::Green);
    let working_row = list_line(&waiting, session_name_row(&waiting, "acpbot", "working"));
    assert!(!working_row.contains('!'), "{working_row}");
    assert!(!working_row.contains('\u{2713}'), "{working_row}");

    let idle = buffer_of(&mock::idle());
    for (name, short) in [
        ("kyotoagent", "idle"),
        ("acpbot", "idle"),
        ("notes", "idle"),
    ] {
        let y = session_name_row(&idle, name, short);
        assert_eq!(mark_color(&idle, y, "\u{2713}"), Color::Green);
    }

    let working = buffer_of(&mock::working());
    let acpbot = list_line(&working, session_name_row(&working, "acpbot", "working"));
    assert!(!acpbot.contains('!'), "{acpbot}");
    assert!(!acpbot.contains('\u{2713}'), "{acpbot}");
    let y = session_name_row(&working, "kyotoagent", "idle");
    assert_eq!(mark_color(&working, y, "\u{2713}"), Color::Green);

    let projects = buffer_of(&mock::projects());
    let y = session_name_row(&projects, "notes", "idle");
    assert_eq!(mark_color(&projects, y, "\u{2713}"), Color::Green);
}

#[test]
fn toast_renders_above_the_composer_without_replacing_its_draft() {
    let mut model = mock::empty();
    model.bottom = "draft kept while saving".into();
    model.toast = Some("Layout saved for all sessions on this server.".into());
    let frame = draw(&model);
    let lines: Vec<_> = frame.lines().collect();
    let toast_row = lines
        .iter()
        .position(|line| line.contains("Layout saved"))
        .unwrap();
    let draft_row = lines
        .iter()
        .position(|line| line.contains("draft kept while saving"))
        .unwrap();
    assert!(toast_row < draft_row);
    assert!(
        draft_row
            >= kyotoagent::screen::split_of(
                &model,
                ratatui::layout::Rect::new(0, 0, FRAME_WIDTH, FRAME_HEIGHT)
            )
            .input
            .y as usize
    );
}

#[test]
fn session_chrome_hides_ids_and_header_status_but_keeps_the_action_footer() {
    let model = mock::working();
    let frame = draw(&model);
    assert!(!frame.lines().next().unwrap().contains("Save layout"));
    for row in &model.sessions {
        assert!(!frame.contains(&row.id));
        assert!(!frame.contains(&kyotoagent::screen::short_id(&row.id)));
    }
    let pane = kyotoagent::screen::split_of(
        &model,
        ratatui::layout::Rect::new(0, 0, FRAME_WIDTH, FRAME_HEIGHT),
    )
    .session;
    let lines: Vec<_> = frame.lines().collect();
    assert!(!lines[pane.y as usize].contains("Reading"));
    assert!(!lines[pane.y as usize].contains("working"));
    assert!(lines[(pane.bottom() - 2) as usize].contains("Reading"));
}
