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
            ListHit::Filter => continue,
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
        let filter = inner.height > 1;
        let filled = inner.height.saturating_sub(1 + u16::from(filter)) as usize;
        let hints = Line::from(vec![
            Span::styled("ctrl-n/p", theme::quiet_key()),
            Span::raw("  "),
            Span::styled("ctrl-k", theme::quiet_key()),
            Span::raw("  "),
            Span::styled("commands", theme::hint()),
        ]);
        let scroll = model.list_scroll.min(lines.len().saturating_sub(filled));
        let content = lines.len().saturating_sub(scroll).min(filled);
        let mut body: Vec<Line<'static>> = lines.into_iter().skip(scroll).take(filled).collect();
        let shown_bands: Vec<bool> = bands.into_iter().skip(scroll).take(filled).collect();
        while body.len() < filled {
            body.push(Line::from(""));
        }
        if filter {
            body.insert(0, filter_line(model, width));
        }
        body.push(hints);
        for (index, line) in body.iter_mut().enumerate() {
            if index >= content || !shown_bands.get(index).copied().unwrap_or(false) {
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
    Filter,
    Header(String),
    Session(String),
}

fn filter_line(model: &ScreenModel, width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let mut room = width;
    for (index, filter) in ListFilter::ORDER.iter().enumerate() {
        if index > 0 {
            push_cols(&mut spans, " ", theme::faint(), &mut room);
        }
        let style = if model.list_filter == *filter {
            Style::default()
                .fg(theme::text())
                .add_modifier(Modifier::BOLD)
        } else {
            theme::faint()
        };
        push_cols(&mut spans, filter.label(), style, &mut room);
    }
    if room > 0 {
        spans.push(Span::styled(" ".repeat(room), Style::default()));
    }
    Line::from(spans)
}

pub fn filter_at(column: usize) -> Option<ListFilter> {
    let mut cursor = 0usize;
    for (index, filter) in ListFilter::ORDER.iter().enumerate() {
        if index > 0 {
            if column == cursor {
                return None;
            }
            cursor += 1;
        }
        let label = filter.label();
        let end = cursor + UnicodeWidthStr::width(label);
        if column >= cursor && column < end {
            return Some(*filter);
        }
        cursor = end;
    }
    None
}

pub(super) struct ListPiece {
    pub(in crate::screen) hit: ListHit,
}

pub(super) fn list_pieces(model: &ScreenModel) -> Vec<ListPiece> {
    let shown: Vec<SessionRow> = model
        .sessions
        .iter()
        .filter(|row| !row.hidden && row_matches_filter(row, model.list_filter))
        .cloned()
        .collect();
    if !sessions_grouped(model) || model.list_filter != ListFilter::All {
        return nest_rows(&shown)
            .into_iter()
            .map(|row| ListPiece {
                hit: ListHit::Session(row.id.clone()),
            })
            .collect();
    }
    let mut keys: Vec<_> = model.projects.iter().map(|row| row.id.clone()).collect();
    for row in &shown {
        let key = project_key(row).unwrap_or_else(|| OTHER_PROJECT.to_string());
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    for key in [OTHER_PROJECT, ARCHIVED_GROUP] {
        if let Some(index) = keys.iter().position(|stored| stored == key) {
            let key = keys.remove(index);
            keys.push(key);
        }
    }
    let mut pieces = Vec::new();
    for key in keys {
        pieces.push(ListPiece {
            hit: ListHit::Header(key.clone()),
        });
        if model.collapsed.contains(&key) {
            continue;
        }
        let group: Vec<&SessionRow> = shown
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
    if row.archived {
        return Some(ARCHIVED_GROUP.to_string());
    }
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
    if key == ARCHIVED_GROUP {
        return ARCHIVED_GROUP.to_string();
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
    if let Some((glyph, color)) = session_mark(row) {
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

pub(super) fn session_mark(row: &SessionRow) -> Option<(&'static str, Color)> {
    if row.archived {
        return Some(("\u{25cb}", theme::muted()));
    }
    list_mark(row.status)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_child_is_left_out_of_the_list_and_a_visible_child_stays() {
        let mut model = crate::mock::children();
        model.sessions.last_mut().unwrap().hidden = true;
        let mut shown = model.sessions.last().unwrap().clone();
        shown.id = "visible1".into();
        shown.hidden = false;
        shown.title = Some("Watch this".into());
        model.sessions.push(shown);
        let pieces = list_pieces(&model);
        let ids: Vec<&str> = pieces
            .iter()
            .filter_map(|piece| match &piece.hit {
                ListHit::Session(id) => Some(id.as_str()),
                ListHit::Header(_) | ListHit::Filter => None,
            })
            .collect();
        assert!(!ids.contains(&"c0ffee00"), "{ids:?}");
        assert!(ids.contains(&"visible1"), "{ids:?}");
        assert!(ids.contains(&"91bc7a1d"), "{ids:?}");
    }

    #[test]
    fn a_list_filter_keeps_running_questions_and_archived_apart() {
        let mut model = crate::mock::children();
        model.sessions[0].status = Status::Working;
        model.sessions.push(SessionRow {
            id: "ask1".into(),
            status: Status::Waiting,
            waiting: Some(Wait::Question),
            title: Some("needs a question".into()),
            ..model.sessions[0].clone()
        });
        model.sessions.push(SessionRow {
            id: "old1".into(),
            archived: true,
            title: Some("old session".into()),
            ..model.sessions[0].clone()
        });
        let mut ids = |filter: ListFilter| {
            model.list_filter = filter;
            list_pieces(&model)
                .into_iter()
                .filter_map(|piece| match piece.hit {
                    ListHit::Session(id) => Some(id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(ids(ListFilter::All).contains(&"91bc7a1d".to_string()));
        assert!(!ids(ListFilter::All).iter().any(|id| id == "old1"));
        let running = ids(ListFilter::Running);
        assert!(running.contains(&"91bc7a1d".to_string()), "{running:?}");
        assert!(!running.iter().any(|id| id == "ask1" || id == "old1"));
        assert_eq!(ids(ListFilter::Questions), vec!["ask1".to_string()]);
        assert!(ids(ListFilter::Finished).contains(&"3f2ae04c".to_string()));
        assert!(!ids(ListFilter::Finished)
            .iter()
            .any(|id| id == "91bc7a1d" || id == "ask1" || id == "old1"));
        assert_eq!(ids(ListFilter::Archived), vec!["old1".to_string()]);
        assert_eq!(filter_at(0), Some(ListFilter::All));
        assert_eq!(filter_at(4), Some(ListFilter::Running));
        assert_eq!(filter_at(8), Some(ListFilter::Questions));
        assert_eq!(filter_at(12), Some(ListFilter::Finished));
        assert_eq!(filter_at(17), Some(ListFilter::Archived));
        assert_eq!(ListFilter::Archived.next(), ListFilter::All);
    }
}
