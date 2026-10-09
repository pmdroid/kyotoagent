use super::*;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorMove {
    Left,
    Right,
    Home,
    End,
    WordLeft,
    WordRight,
    Up,
    Down,
}

pub(super) fn question(app: &App) -> bool {
    matches!(mode(app), Mode::QuestionText | Mode::Question { .. })
}

pub(super) fn input(app: &App) -> &str {
    if question(app) {
        &app.question_text
    } else {
        &app.ask
    }
}

pub(super) fn cursor(app: &App) -> usize {
    let text = input(app);
    let position = if question(app) {
        app.question_cursor
    } else {
        app.ask_cursor
    };
    let position = position.unwrap_or(text.len()).min(text.len());
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .take_while(|index| *index <= position)
        .last()
        .unwrap_or(0)
}

fn set(app: &mut App, position: usize) {
    let position = (position < input(app).len()).then_some(position);
    if question(app) {
        app.question_cursor = position;
    } else {
        app.ask_cursor = position;
    }
}

pub(super) fn replace(app: &mut App, range: Range<usize>, text: &str) {
    let question = question(app);
    let input = if question {
        &mut app.question_text
    } else {
        &mut app.ask
    };
    app.pastes.retain_mut(|paste| {
        if paste.question != question {
            return true;
        }
        if !paste_matches(input, paste) {
            return false;
        }
        if paste.start >= range.end {
            paste.start = paste.start - range.len() + text.len();
        } else if paste.start + paste.text.len() > range.start {
            return false;
        }
        true
    });
    input.replace_range(range.clone(), text);
    set(app, range.start + text.len());
    if !question {
        after_ask_edit(app);
    }
}

pub(super) fn insert(app: &mut App, text: &str, paste: bool) {
    let start = cursor(app);
    replace(app, start..start, text);
    if paste && (text.chars().count() > 2048 || text.lines().count() > 12) {
        app.pastes.push(PastedInput {
            start,
            text: text.to_string(),
            question: question(app),
        });
        app.pastes.sort_by_key(|paste| paste.start);
    }
}

fn adjacent(app: &App, forward: bool) -> usize {
    let text = input(app);
    let position = cursor(app);
    if let Some(paste) = app.pastes.iter().find(|paste| {
        paste.question == question(app)
            && paste_matches(text, paste)
            && if forward {
                paste.start == position
            } else {
                paste.start + paste.text.len() == position
            }
    }) {
        return if forward {
            paste.start + paste.text.len()
        } else {
            paste.start
        };
    }
    if forward {
        text.grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|i| *i > position)
            .unwrap_or(text.len())
    } else {
        text[..position]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }
}

pub(super) fn delete(app: &mut App, effect: Effect) {
    let position = cursor(app);
    let start = match effect {
        Effect::DeleteForward => position,
        Effect::Backspace => adjacent(app, false),
        _ => {
            let mut prefix = input(app)[..position].to_string();
            apply::delete_tail(&mut prefix, effect == Effect::DeleteLine);
            prefix.len()
        }
    };
    let end = if effect == Effect::DeleteForward {
        adjacent(app, true)
    } else {
        position
    };
    let mut range = start..end;
    for paste in &app.pastes {
        if paste.question == question(app)
            && paste_matches(input(app), paste)
            && paste.start < range.end
            && paste.start + paste.text.len() > range.start
        {
            range.start = range.start.min(paste.start);
            range.end = range.end.max(paste.start + paste.text.len());
        }
    }
    replace(app, range, "");
}

pub(super) fn move_cursor(app: &mut App, movement: CursorMove) {
    let position = cursor(app);
    let text = input(app);
    let target = match movement {
        CursorMove::Left => adjacent(app, false),
        CursorMove::Right => adjacent(app, true),
        CursorMove::Home => text[..position].rfind('\n').map_or(0, |i| i + 1),
        CursorMove::End => text[position..]
            .find('\n')
            .map_or(text.len(), |i| position + i),
        CursorMove::WordLeft => {
            let mut prefix = text[..position].to_string();
            apply::delete_tail(&mut prefix, false);
            prefix.len()
        }
        CursorMove::WordRight => text[position..]
            .split_word_bound_indices()
            .find(|(_, word)| !word.chars().all(char::is_whitespace))
            .map_or(text.len(), |(i, word)| position + i + word.len()),
        CursorMove::Up | CursorMove::Down => {
            let model = layout_model(app);
            let target =
                screen::move_input_vertical(&model, app.area, movement == CursorMove::Down);
            set_display_cursor(app, target);
            return;
        }
    };
    let target = app
        .pastes
        .iter()
        .find(|paste| {
            paste.question == question(app)
                && paste_matches(text, paste)
                && paste.start < target
                && target < paste.start + paste.text.len()
        })
        .map_or(target, |paste| {
            if target < position {
                paste.start
            } else {
                paste.start + paste.text.len()
            }
        });
    set(app, target);
}

pub(super) fn display(app: &App) -> (String, usize, Vec<Range<usize>>) {
    let text = input(app);
    let mut bottom = String::new();
    let mut copied = 0;
    let mut position = cursor(app);
    let mut ranges = Vec::new();
    for paste in &app.pastes {
        if paste.question != question(app) || paste.start < copied || !paste_matches(text, paste) {
            continue;
        }
        bottom.push_str(&text[copied..paste.start]);
        let start = bottom.len();
        let label = format!("[Pasted input: {} chars]", paste.text.chars().count());
        bottom.push_str(&label);
        ranges.push(start..bottom.len());
        copied = paste.start + paste.text.len();
        if cursor(app) >= copied {
            position = position - paste.text.len() + label.len();
        }
    }
    bottom.push_str(&text[copied..]);
    (bottom, position, ranges)
}

pub(super) fn set_display_cursor(app: &mut App, position: usize) {
    let (_, _, ranges) = display(app);
    let mut target = position;
    for (paste, range) in app
        .pastes
        .iter()
        .filter(|paste| paste.question == question(app) && paste_matches(input(app), paste))
        .zip(ranges)
    {
        if position >= range.end {
            target = target - range.len() + paste.text.len();
        } else if position > range.start {
            target = paste.start;
            break;
        }
    }
    set(app, target.min(input(app).len()));
    app.select = None;
}

pub(super) fn active(app: &App) -> bool {
    !app.overlay
        && screen_model(app).overlay.is_none()
        && !choosing(app)
        && app.picker.is_none()
        && app.command_ui.is_none()
        && !app.queue_open
        && app.delete_confirm.is_none()
        && app.open_file.is_none()
        && app.open_text.is_none()
        && app.open_image.is_none()
        && (!matches!(mode(app), Mode::Permission) || app.ask.starts_with('/'))
}
