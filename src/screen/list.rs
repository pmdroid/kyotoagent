use std::collections::HashSet;

use super::*;

pub(super) fn render_list(model: &ScreenModel, area: Rect, frame: &mut Frame) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(Line::from(Span::styled(" sessions ", theme::title())));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = usize::from(inner.width);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut bands: Vec<bool> = Vec::new();
    for piece in list_pieces(model) {
        match piece.hit {
            ListHit::Header(key) => {
                let selected = model.list_header.as_deref() == Some(key.as_str());
                let mark = if model.collapsed.contains(&key) {
                    "\u{25b8}"
                } else {
                    "\u{25be}"
                };
                let text = format!("{mark} {}", header_label(model, &key));
                let style = Style::default()
                    .fg(theme::text())
                    .add_modifier(Modifier::BOLD);
                let mut spans = Vec::new();
                let mut room = width;
                let badge = model
                    .projects
                    .iter()
                    .find(|row| row.id == key)
                    .and_then(|row| row.server.as_ref())
                    .map(|server| format!(" [{server}]"));
                let reserved = badge.as_ref().map_or(0, |badge| {
                    UnicodeWidthStr::width(badge.as_str()).min(width.saturating_sub(4))
                });
                room -= reserved;
                push_cols(&mut spans, &text, style, &mut room);
                room += reserved;
                if let Some(badge) = badge {
                    push_cols(&mut spans, &badge, theme::faint(), &mut room);
                }
                spans.push(Span::raw(" ".repeat(room)));
                lines.push(Line::from(spans));
                bands.push(selected);
            }
            ListHit::Session(id) => {
                let Some(row) = model.sessions.iter().find(|row| row.id == id) else {
                    continue;
                };
                let is_selected = model.list_header.is_none() && model.selected == row.id;
                let base = if is_selected {
                    theme::selected_row()
                } else {
                    Style::default()
                };
                let name_style = if is_selected {
                    Style::default()
                        .fg(theme::text())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::text())
                };
                lines.push(session_name_line(row, is_selected, name_style, width));
                let status = Span::styled(
                    row.status_text(),
                    Style::default()
                        .fg(row.status.color())
                        .add_modifier(if is_selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                );
                let mut detail_spans = Vec::new();
                if row.parent_id.is_some() {
                    detail_spans.push(Span::styled("  ", theme::faint()));
                }
                detail_spans.push(Span::styled("  ", theme::faint()));
                detail_spans.push(status);
                if let Some(mark) = row.pull_mark() {
                    detail_spans.push(Span::styled(format!("  {mark}"), theme::faint()));
                }
                let detail = Line::from(detail_spans);
                let used = detail.width();
                let mut detail = detail;
                detail.spans.push(Span::styled(
                    " ".repeat(width.saturating_sub(used)),
                    Style::default(),
                ));
                lines.push(detail);
                lines.push(Line::from(Span::styled(" ".repeat(width), base)));
                bands.extend([is_selected, is_selected, is_selected]);
            }
        }
    }
    if inner.height > 0 {
        let filled = inner.height.saturating_sub(1) as usize;
        let hints = Line::from(vec![
            Span::styled("ctrl-n/p", theme::quiet_key()),
            Span::raw("  "),
            Span::styled("ctrl-k", theme::quiet_key()),
            Span::raw("  "),
            Span::styled("commands", theme::hint()),
        ]);
        let content = lines.len().min(filled);
        let mut body = lines;
        while body.len() < filled {
            body.push(Line::from(""));
        }
        body.truncate(filled);
        body.push(hints);
        for (index, line) in body.iter_mut().enumerate() {
            if index >= content || !bands.get(index).copied().unwrap_or(false) {
                continue;
            }
            for span in line.spans.iter_mut() {
                span.style = theme::selected_row().patch(span.style);
            }
        }
        frame.render_widget(Paragraph::new(body), inner);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListHit {
    Header(String),
    Session(String),
}

pub(super) struct ListPiece {
    pub(in crate::screen) hit: ListHit,
}

pub(super) fn list_pieces(model: &ScreenModel) -> Vec<ListPiece> {
    if !sessions_grouped(model) {
        return nest_rows(&model.sessions)
            .into_iter()
            .map(|row| ListPiece {
                hit: ListHit::Session(row.id.clone()),
            })
            .collect();
    }
    let mut keys: Vec<_> = model.projects.iter().map(|row| row.id.clone()).collect();
    for row in &model.sessions {
        let key = project_key(row).unwrap_or_else(|| OTHER_PROJECT.to_string());
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if let Some(index) = keys.iter().position(|key| key == OTHER_PROJECT) {
        let key = keys.remove(index);
        keys.push(key);
    }
    let mut pieces = Vec::new();
    for key in keys {
        pieces.push(ListPiece {
            hit: ListHit::Header(key.clone()),
        });
        if model.collapsed.contains(&key) {
            continue;
        }
        let group: Vec<&SessionRow> = model
            .sessions
            .iter()
            .filter(|row| project_key(row).unwrap_or_else(|| OTHER_PROJECT.to_string()) == key)
            .collect();
        for row in nest_rows_ref(&group) {
            pieces.push(ListPiece {
                hit: ListHit::Session(row.id.clone()),
            });
        }
    }
    pieces
}

fn nest_rows(rows: &[SessionRow]) -> Vec<&SessionRow> {
    let refs: Vec<&SessionRow> = rows.iter().collect();
    nest_rows_ref(&refs)
}

fn nest_rows_ref<'a>(rows: &[&'a SessionRow]) -> Vec<&'a SessionRow> {
    let ids: HashSet<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let nested = row
            .parent_id
            .as_deref()
            .is_some_and(|parent| ids.contains(parent));
        if nested || !seen.insert(row.id.as_str()) {
            continue;
        }
        out.push(*row);
        for child in rows {
            if child.parent_id.as_deref() == Some(row.id.as_str()) && seen.insert(child.id.as_str())
            {
                out.push(*child);
            }
        }
    }
    for row in rows {
        if seen.insert(row.id.as_str()) {
            out.push(*row);
        }
    }
    out
}

pub(super) fn sessions_grouped(model: &ScreenModel) -> bool {
    !model.projects.is_empty() || model.sessions.iter().any(|row| project_key(row).is_some())
}

pub(super) fn project_key(row: &SessionRow) -> Option<String> {
    row.project
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

pub(super) fn header_label(model: &ScreenModel, key: &str) -> String {
    if let Some(project) = model.projects.iter().find(|row| row.id == key) {
        return project.name.clone();
    }
    if key == OTHER_PROJECT {
        return OTHER_PROJECT.to_string();
    }
    model
        .sessions
        .iter()
        .find(|row| project_key(row).as_deref() == Some(key))
        .and_then(|row| {
            row.project_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| key.to_string())
}

pub fn session_group(sessions: &[SessionRow], id: &str) -> Option<String> {
    let grouped = sessions.iter().any(|row| project_key(row).is_some());
    if !grouped {
        return None;
    }
    let row = sessions.iter().find(|row| row.id == id)?;
    Some(project_key(row).unwrap_or_else(|| OTHER_PROJECT.to_string()))
}

pub(super) fn render_rail(area: Rect, frame: &mut Frame) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let line = Line::from(Span::styled("\u{2502}", theme::border()));
    let body: Vec<Line> = (0..area.height).map(|_| line.clone()).collect();
    frame.render_widget(Paragraph::new(body), area);
}

pub(super) fn session_name_line(
    row: &SessionRow,
    is_selected: bool,
    name_style: Style,
    width: usize,
) -> Line<'static> {
    let mark = if row.parent_id.is_some() {
        if is_selected {
            "  \u{25b8} "
        } else {
            "    "
        }
    } else if is_selected {
        "\u{25b8} "
    } else {
        "  "
    };
    let mut spans = Vec::new();
    let mut room = width;
    push_cols(&mut spans, mark, name_style, &mut room);
    if let Some((glyph, color)) = list_mark(row.status) {
        let mut icon = Style::default().fg(color);
        if is_selected {
            icon = icon.add_modifier(Modifier::BOLD);
        }
        push_cols(&mut spans, glyph, icon, &mut room);
        push_cols(&mut spans, " ", name_style, &mut room);
    }
    push_cols(&mut spans, &row.name(), name_style, &mut room);
    if room > 0 {
        spans.push(Span::styled(" ".repeat(room), Style::default()));
    }
    Line::from(spans)
}

pub(super) fn list_mark(status: Status) -> Option<(&'static str, Color)> {
    match status {
        Status::Idle => Some(("\u{2713}", theme::good())),
        Status::Waiting => Some(("!", theme::attention())),
        Status::Working => None,
    }
}

pub(super) fn push_cols(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    style: Style,
    room: &mut usize,
) {
    if *room == 0 || text.is_empty() {
        return;
    }
    let text = truncate_cols(text, *room);
    let used = UnicodeWidthStr::width(text.as_str());
    *room = room.saturating_sub(used);
    if !text.is_empty() {
        spans.push(Span::styled(text, style));
    }
}
