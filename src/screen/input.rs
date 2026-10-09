use super::*;
use unicode_segmentation::UnicodeSegmentation;

struct PromptLayout {
    lines: Vec<Line<'static>>,
    stops: Vec<(usize, usize, usize)>,
    cursor_row: usize,
}

fn prompt_layout(model: &ScreenModel, width: u16) -> PromptLayout {
    let width = usize::from(width).max(1);
    let cursor = model.bottom_cursor.unwrap_or(model.bottom.len());
    let mut layout = PromptLayout {
        lines: vec![Line::default()],
        stops: Vec::new(),
        cursor_row: 0,
    };
    let mut x = 0;
    let mut y = 0;
    let prefix = " › ".graphemes(true).map(|part| (None, part));
    let text = model
        .bottom
        .grapheme_indices(true)
        .map(|(i, part)| (Some(i), part));
    for (index, part) in prefix
        .chain(text)
        .chain(std::iter::once((Some(model.bottom.len()), " ")))
    {
        let cells =
            UnicodeWidthStr::width(part).max(usize::from(part == "\n" && index == Some(cursor)));
        if x > 0 && x + cells > width {
            layout.lines.push(Line::default());
            x = 0;
            y += 1;
        }
        if let Some(index) = index {
            layout.stops.push((index, x, y));
            if index == cursor {
                layout.cursor_row = y;
            }
        }
        let at_cursor = index == Some(cursor);
        if part == "\n" {
            if at_cursor {
                layout.lines[y]
                    .spans
                    .push(Span::styled("█", theme::cursor().bg(theme::selected_bg())));
            }
            layout.lines.push(Line::default());
            x = 0;
            y += 1;
            continue;
        }
        let style = if at_cursor {
            if index == Some(model.bottom.len()) {
                theme::cursor().bg(theme::selected_bg())
            } else {
                theme::input().add_modifier(Modifier::REVERSED)
            }
        } else if index.is_none() {
            Style::default()
                .fg(theme::accent())
                .bg(theme::selected_bg())
                .add_modifier(Modifier::BOLD)
        } else {
            theme::input()
        };
        let part = if at_cursor && index == Some(model.bottom.len()) {
            "█"
        } else {
            part
        };
        layout.lines[y]
            .spans
            .push(Span::styled(part.to_string(), style));
        x += cells;
    }
    layout
}

pub(super) fn prompt_lines(model: &ScreenModel, width: u16) -> Vec<Line<'static>> {
    prompt_layout(model, width).lines
}

fn content_area(model: &ScreenModel, area: Rect) -> Rect {
    let input = split_of(model, area).input;
    if composer_framed(model, area.height) {
        Block::bordered().inner(input)
    } else {
        input
    }
}

pub(super) fn input_start(model: &ScreenModel, content: Rect, total: usize) -> usize {
    let tail = total.saturating_sub(usize::from(content.height));
    if model.bottom_cursor.is_some() {
        tail.min(prompt_layout(model, content.width).cursor_row)
    } else {
        tail
    }
}

fn nearest(layout: &PromptLayout, x: usize, y: usize) -> usize {
    layout
        .stops
        .iter()
        .filter(|(_, _, row)| *row == y)
        .take_while(|(_, col, _)| *col <= x)
        .last()
        .or_else(|| layout.stops.iter().find(|(_, _, row)| *row == y))
        .or_else(|| layout.stops.last())
        .map_or(0, |(index, _, _)| *index)
}

pub fn input_cursor_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<usize> {
    if model.bottom_cursor.is_none() || model.overlay.is_some() {
        return None;
    }
    let content = content_area(model, area);
    if !content.contains(Position { x: column, y: row }) {
        return None;
    }
    let layout = prompt_layout(model, content.width);
    let start = input_start(
        model,
        content,
        layout.lines.len() + model.pending_images.len(),
    );
    let y = usize::from(row - content.y) + start;
    (y < layout.lines.len()).then(|| nearest(&layout, usize::from(column - content.x), y))
}

pub fn move_input_vertical(model: &ScreenModel, area: Rect, down: bool) -> usize {
    let content = content_area(model, area);
    let layout = prompt_layout(model, content.width);
    let position = model.bottom_cursor.unwrap_or(model.bottom.len());
    let (_, x, y) = layout
        .stops
        .iter()
        .find(|(index, _, _)| *index == position)
        .copied()
        .unwrap_or_default();
    let target = if down {
        (y + 1).min(layout.lines.len() - 1)
    } else {
        y.saturating_sub(1)
    };
    nearest(&layout, x, target)
}

pub fn input_image_at(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<(usize, Rect)> {
    let content = content_area(model, area);
    if !content.contains(Position { x: column, y: row }) {
        return None;
    }
    let lines = prompt_layout(model, content.width).lines.len();
    let start = input_start(model, content, lines + model.pending_images.len());
    let index = (usize::from(row - content.y) + start).checked_sub(lines)?;
    (index < model.pending_images.len()).then_some((index, content))
}
