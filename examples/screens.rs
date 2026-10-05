//! Run the three session frames in a real terminal, one after another.
//!
//! This is a demo, not product code. It exists so the frames can be watched on
//! a real terminal instead of only through `TestBackend`.
//! `cargo run --example screens`, then Enter for the next frame and `q` to
//! leave. The `kyotoagent` binary opens the live screen; this example still walks
//! the hardcoded frames.
//!
//! The terminal wants to be 76 columns by 24 rows, the size the golden frames
//! are drawn at.

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use kyotoagent::mock;
use kyotoagent::screen::render;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::Terminal;

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let result = run(&mut Terminal::new(CrosstermBackend::new(stdout))?);
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    for (name, model) in mock::all() {
        if !show(terminal, name, &model)? {
            return Ok(());
        }
    }
    if !show(terminal, "todos", &mock::todos())? {
        return Ok(());
    }
    // Leave the last frame on screen for a moment so a capture or a person
    // looking over a shoulder sees it before the terminal is restored.
    thread_sleep(Duration::from_millis(2500));
    Ok(())
}

/// Draw `model` until the next key. Returns false when the viewer wants out.
fn show(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    name: &str,
    model: &kyotoagent::screen::ScreenModel,
) -> io::Result<bool> {
    // The window title names the frame, which labels a screen capture.
    let _ = execute!(io::stdout(), SetTitle(format!("kyotoagent - {name}")));
    loop {
        terminal.draw(|frame| {
            render(model, frame_area(frame.area()), frame);
        })?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Enter | KeyCode::Char(' ') => return Ok(true),
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(false),
                        _ => {}
                    }
                }
            }
        }
    }
}

/// The 76 by 24 area the golden frames use, centred in whatever the terminal
/// gives us, so a larger window shows the frame with a margin rather than
/// stretching it.
fn frame_area(area: Rect) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(76)])
        .flex(ratatui::layout::Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(24)])
        .flex(ratatui::layout::Flex::Center)
        .areas(area);
    area.inner(Margin {
        horizontal: 0,
        vertical: 0,
    })
}

fn thread_sleep(duration: Duration) {
    std::thread::sleep(duration);
}
