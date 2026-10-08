use ratatui::layout::Position;

use super::*;

pub(super) fn overlay_inner_width(pane: Rect) -> usize {
    let width = pane.width.saturating_sub(2).min(pane.width);
    usize::from(width.saturating_sub(2)).max(1)
}

pub(super) fn overlay_rect(pane: Rect, line_count: usize) -> Rect {
    let width = pane.width.saturating_sub(2).min(pane.width);
    let height = u16::try_from(line_count)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(pane.height.saturating_sub(2))
        .max(3.min(pane.height))
        .min(pane.height);
    let area = Rect {
        x: pane.x + pane.width.saturating_sub(width) / 2,
        y: pane.y + pane.height.saturating_sub(height) / 2,
        width,
        height,
    };
    area.intersection(pane)
}

pub(super) fn render_overlay(model: &ScreenModel, screen: Rect, pane: Rect, frame: &mut Frame) {
    let Some(overlay) = model.overlay.as_ref() else {
        return;
    };
    let inner_width = match overlay {
        Overlay::Menu { .. } => {
            let rect = menu_rect(overlay, pane);
            usize::from(Block::bordered().inner(rect).width).max(1)
        }
        _ => overlay_inner_width(pane),
    };
    if let Overlay::Image { image } = overlay {
        dim_outside(frame, screen, crate::image_preview::popup(pane));
        crate::image_preview::render(image, pane, frame);
        return;
    }
    if matches!(overlay, Overlay::VisualQuestion { .. }) {
        dim_outside(frame, screen, crate::image_preview::popup(pane));
        super::question::render_visual_question(overlay, pane, frame);
        return;
    }
    let mut lines = overlay_lines(overlay, inner_width);
    if let Overlay::Pairing { title, qr } = overlay {
        if lines.len() > usize::from(pane.height.saturating_sub(4))
            || qr.lines().any(|line| line.chars().count() > inner_width)
        {
            lines = wrapped(title, inner_width, theme::body());
            lines.extend(wrapped(
                "Enlarge the terminal to display the QR code.",
                inner_width,
                theme::body(),
            ));
        }
    }
    let area = match overlay {
        Overlay::Menu { .. } => menu_rect(overlay, pane),
        _ => overlay_rect(pane, lines.len()),
    };
    let origin = if matches!(overlay, Overlay::File { .. } | Overlay::Text { .. }) {
        let room = usize::from(Block::bordered().inner(area).height);
        model.file_scroll.min(lines.len().saturating_sub(room))
    } else {
        0
    };
    paint_selection(model.select, SelectPlace::Overlay, 0, &mut lines);
    if area.width < 2 || area.height < 2 {
        return;
    }
    dim_outside(frame, screen, area);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border_focused());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(origin)
                .take(usize::from(inner.height))
                .collect::<Vec<_>>(),
        ),
        inner,
    );
    if !matches!(overlay, Overlay::Menu { .. }) {
        paint_close(frame, area);
    }
}

fn dim_outside(frame: &mut Frame, screen: Rect, popup: Rect) {
    let buf = frame.buffer_mut();
    for y in screen.y..screen.y.saturating_add(screen.height) {
        for x in screen.x..screen.x.saturating_add(screen.width) {
            if popup.contains(Position { x, y }) {
                continue;
            }
            if let Some(cell) = buf.cell_mut(Position { x, y }) {
                cell.set_style(Style::default().add_modifier(Modifier::DIM));
            }
        }
    }
}

pub(super) fn paint_close(frame: &mut Frame, area: Rect) {
    let x = area.x.saturating_add(area.width.saturating_sub(1));
    if let Some(cell) = frame.buffer_mut().cell_mut(Position { x, y: area.y }) {
        cell.set_symbol("x");
        cell.set_style(theme::key());
    }
}

pub(in crate::screen) fn overlay_lines(overlay: &Overlay, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match overlay {
        Overlay::Queue {
            rows,
            highlight,
            removing,
        } => {
            lines.push(Line::from(Span::styled("Queued messages", theme::body())));
            for (index, row) in rows.iter().enumerate() {
                let mark = if index == *highlight { "▸ " } else { "  " };
                lines.push(Line::from(Span::styled(
                    format!("{mark}{}", truncate_cols(row, width.saturating_sub(2))),
                    theme::body(),
                )));
            }
            lines.push(Line::from(""));
            lines.push(key_hint_line(if *removing {
                "Removing…"
            } else {
                "↑↓ select   Delete remove   Esc close"
            }));
        }
        Overlay::Pairing { title, qr } => {
            lines.extend(wrapped(title, width, theme::body()));
            lines.extend(qr.lines().map(|line| Line::from(line.to_string())));
        }
        Overlay::Image { .. } | Overlay::VisualQuestion { .. } => {}
        Overlay::Question {
            text,
            choices,
            prompt,
        } => {
            lines.extend(markdown_lines(text, width));
            for (index, choice) in choices.iter().enumerate() {
                lines.push(Line::from(Span::styled(
                    format!("{}  {}", index + 1, choice.label),
                    theme::body(),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    " \u{203a} ",
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(prompt.clone(), theme::input()),
                Span::styled("\u{2588}", theme::cursor()),
            ]));
        }
        Overlay::Permission { action, diff, argv } => {
            lines.extend(wrapped(action, width, theme::body()));
            for line in diff {
                lines.extend(wrapped(line, width, diff_style(line)));
            }
            lines.extend(argv_lines(argv, width));
            lines.push(Line::from(""));
            lines.push(key_hint_line("a once   s session   d deny"));
        }
        Overlay::Text { text } | Overlay::Proof { text, .. } => {
            if !text.is_empty() {
                lines.extend(markdown_lines(text, width));
            }
            if let Overlay::Proof { items, .. } = overlay {
                for item in items
                    .iter()
                    .filter(|item| item.outcome == Outcome::PassedEarlier)
                {
                    lines.extend(wrapped(
                        &format!("✓ {} (earlier)", item.id),
                        width,
                        Style::default().fg(theme::good()),
                    ));
                }
            }
        }
        Overlay::Pull { url } => {
            lines.extend(wrapped(url, width, theme::body()));
        }
        Overlay::Model { rows, highlight } | Overlay::Effort { rows, highlight } => {
            for (index, row) in rows.iter().enumerate() {
                let selected = index == *highlight;
                let style = if selected {
                    Style::default()
                        .fg(theme::text())
                        .add_modifier(Modifier::BOLD)
                } else {
                    theme::body()
                };
                let mark = if selected { "\u{25b8} " } else { "  " };
                lines.push(Line::from(Span::styled(format!("{mark}{row}"), style)));
            }
        }
        Overlay::File {
            path,
            text,
            truncated,
        } => {
            lines.extend(wrapped(path, width, theme::faint()));
            lines.extend(file::file_lines(path, text, width));
            if *truncated {
                lines.extend(wrapped("File preview truncated", width, theme::faint()));
            }
        }
        Overlay::Thinking { text } => {
            lines.extend(wrapped(text, width, theme::body()));
        }
        Overlay::Palette {
            rows,
            highlight,
            scroll,
            query,
        } => {
            lines.push(palette_query_line(query, width));
            let start = if rows.is_empty() {
                0
            } else {
                (*scroll).min(rows.len() - 1)
            };
            for (index, row) in rows.iter().enumerate().skip(start) {
                lines.push(command_row_line(row, width, index == *highlight));
            }
        }
        Overlay::Help { rows, scroll } => {
            let start = if rows.is_empty() {
                0
            } else {
                (*scroll).min(rows.len() - 1)
            };
            for row in rows.iter().skip(start) {
                lines.push(command_row_line(row, width, false));
            }
        }
        Overlay::Context {
            percent,
            used,
            reported_prompt_tokens,
            window,
            buckets,
        } => {
            let title = format!(
                "CONTEXT  {percent}%  \u{00b7}  {} / {}",
                grouped(*used),
                grouped(*window)
            );
            lines.extend(wrapped(&title, width, theme::body()));
            lines.extend(wrapped("Estimated current request", width, theme::body()));
            if let Some(tokens) = reported_prompt_tokens {
                lines.extend(wrapped(
                    &format!("Last request  {}", grouped(*tokens)),
                    width,
                    theme::body(),
                ));
            }
            for bucket in buckets {
                let text = match bucket.tokens {
                    Some(tokens) => format!("{}  {}", bucket.id, grouped(tokens)),
                    None => bucket.id.clone(),
                };
                lines.push(Line::from(Span::styled(text, theme::body())));
            }
        }
        Overlay::Menu { items, .. } => {
            for item in items {
                lines.push(Line::from(Span::styled(
                    truncate_cols(item, width),
                    theme::body(),
                )));
            }
        }
        Overlay::Delete {
            path,
            highlight,
            remove_workspace,
            confirm_dirty,
            warning,
            loading,
        } => {
            lines.extend(wrapped(DELETE_PROMPT, width, theme::body()));
            if let Some(path) = path {
                lines.extend(wrapped(path, width, theme::body()));
                if *remove_workspace {
                    lines.extend(wrapped(DELETE_DIRECTORY, width, theme::body()));
                }
            }
            lines.push(Line::from(""));
            if *loading {
                lines.extend(wrapped(
                    "Checking workspace or deleting session…",
                    width,
                    theme::faint(),
                ));
            }
            if let Some(warning) = warning {
                lines.extend(wrapped(warning, width, theme::body()));
            }
            lines.extend(wrapped(
                "↑↓ select · Space toggle workspace · Enter confirm",
                width,
                theme::faint(),
            ));
            for (index, label) in delete_labels(path.as_deref(), *remove_workspace, *confirm_dirty)
                .into_iter()
                .enumerate()
            {
                let selected = index == *highlight;
                let mark = if selected { "\u{25b8} " } else { "  " };
                let style = if selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    theme::body()
                };
                lines.push(Line::from(Span::styled(format!("{mark}{label}"), style)));
            }
        }
        Overlay::Enhance {
            source,
            text,
            error,
        } => {
            lines.extend(wrapped(source, width, theme::faint()));
            if !text.is_empty() {
                lines.extend(wrapped(text, width, theme::body()));
            }
            if let Some(error) = error {
                lines.extend(wrapped(error, width, theme::faint()));
            }
            lines.push(Line::from(""));
            let hint = if error.is_some() {
                "u use   e edit   x discard   r retry"
            } else {
                "u use   e edit   x discard"
            };
            lines.push(key_hint_line(hint));
        }
    }
    lines
}

pub(in crate::screen) fn menu_rect(overlay: &Overlay, bounds: Rect) -> Rect {
    let Overlay::Menu {
        items, column, row, ..
    } = overlay
    else {
        return Rect {
            x: bounds.x,
            y: bounds.y,
            width: 0,
            height: 0,
        };
    };
    let text = items
        .iter()
        .map(|item| item.chars().count())
        .max()
        .unwrap_or(1);
    let width = u16::try_from(text)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(bounds.width)
        .max(2.min(bounds.width));
    let height = u16::try_from(items.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(bounds.height)
        .max(2.min(bounds.height));
    let max_x = bounds.x.saturating_add(bounds.width.saturating_sub(width));
    let max_y = bounds
        .y
        .saturating_add(bounds.height.saturating_sub(height));
    Rect {
        x: (*column).clamp(bounds.x, max_x),
        y: (*row).clamp(bounds.y, max_y),
        width,
        height,
    }
}

fn palette_query_line(query: &str, width: usize) -> Line<'static> {
    let text = format!("\u{203a} {query}");
    Line::from(Span::styled(truncate_cols(&text, width), theme::input()))
}

pub(super) fn command_row_line(row: &CommandLine, width: usize, selected: bool) -> Line<'static> {
    let mark = if selected { "\u{25b8} " } else { "  " };
    let keys = if row.keys.is_empty() || row.keys == row.name {
        String::new()
    } else {
        format!("{}  ", row.keys)
    };
    let text = format!("{mark}{keys}{}  {}", row.name, row.hint);
    let style = if selected {
        Style::default()
            .fg(theme::text())
            .add_modifier(Modifier::BOLD)
    } else {
        theme::body()
    };
    Line::from(Span::styled(truncate_cols(&text, width), style))
}

pub(super) fn key_hint_line(text: &str) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, part) in text.split("   ").enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        match part.split_once(' ') {
            Some((key, word)) => {
                spans.push(Span::styled(key.to_string(), theme::key()));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(word.to_string(), theme::hint()));
            }
            None => spans.push(Span::raw(part.to_string())),
        }
    }
    Line::from(spans)
}

pub fn file_scroll_max(model: &ScreenModel, area: Rect) -> usize {
    let Some(overlay @ (Overlay::File { .. } | Overlay::Text { .. })) = model.overlay.as_ref()
    else {
        return 0;
    };
    let pane = split_of(model, area).session;
    let count = overlay_lines(overlay, overlay_inner_width(pane)).len();
    let room = Block::bordered().inner(overlay_rect(pane, count)).height;
    count.saturating_sub(usize::from(room))
}
