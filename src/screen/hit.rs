use super::overlay::{menu_rect, overlay_lines};
use super::*;

pub fn waiting_card_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    card_at_point(model, area, column, row).is_some_and(Card::is_waiting)
}

pub fn preview_text_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<String> {
    if model.overlay.is_some() {
        return None;
    }
    card_at_point(model, area, column, row)?
        .preview_text()
        .map(str::to_string)
}

pub fn artifact_at(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<crate::proof::ProofFile> {
    if model.overlay.is_some() {
        return None;
    }
    match card_at_point(model, area, column, row)? {
        Card::Artifact { file, .. } => Some(file.clone()),
        _ => None,
    }
}

pub fn overlay_card_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    card_at_point(model, area, column, row).is_some_and(card_opens_overlay)
}

pub fn pull_target_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    let Some(url) = model
        .selected_session()
        .and_then(|session| session.pull_url.as_deref())
    else {
        return false;
    };
    let inner = session_inner_of(model, area);
    let title_y = inner.y.saturating_sub(1);
    let title_x0 = inner.x.saturating_sub(1);
    let title_x1 = inner.x.saturating_add(inner.width).saturating_add(1);
    if row == title_y && column >= title_x0 && column < title_x1 {
        return true;
    }
    if !inner.contains(Position { x: column, y: row }) {
        return false;
    }
    let Some(index) = column_index(model, inner, row) else {
        return false;
    };
    let opened = wrap(&crate::session::opened_line(url), usize::from(inner.width));
    (1..1 + opened.len()).contains(&index)
}

pub fn thinking_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    if model.phase != Some(Phase::Thinking) {
        return false;
    }
    let inner = session_inner_of(model, area);
    if inner.height == 0 || inner.width == 0 {
        return false;
    }
    let y = inner.y.saturating_add(inner.height.saturating_sub(1));
    row == y && column >= inner.x && column < inner.x.saturating_add(inner.width)
}

pub fn task_line_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<String> {
    task_row_at(model, area, column, row)
}

pub fn left_rail_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    if model.left_open {
        return false;
    }
    let list = split_of(model, area).list;
    list.width > 0 && list.contains(Position { x: column, y: row })
}

pub fn left_border_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    if !model.left_open {
        return false;
    }
    let list = split_of(model, area).list;
    list.height > 0
        && row >= list.y
        && row < list.y.saturating_add(list.height)
        && column == list.x.saturating_add(list.width.saturating_sub(1))
}

pub fn session_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<String> {
    match list_at(model, area, column, row) {
        Some(ListHit::Session(id)) => Some(id),
        _ => None,
    }
}

pub fn list_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<ListHit> {
    if !model.left_open {
        return None;
    }
    let list = split_of(model, area).list;
    let inner = Block::bordered().inner(list);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    if !inner.contains(Position { x: column, y: row }) {
        return None;
    }
    let hint = inner.y.saturating_add(inner.height.saturating_sub(1));
    if row == hint {
        return None;
    }
    let filter = inner.height > 1;
    let room = usize::from(inner.height.saturating_sub(1 + u16::from(filter)));
    let mut skip = super::list_scroll_of(model, area);
    let mut y = inner.y;
    if filter {
        if row == y {
            return Some(ListHit::Filter);
        }
        y = y.saturating_add(1);
    }
    let body = y;
    for piece in list_pieces(model) {
        let lines = match piece.hit {
            ListHit::Filter | ListHit::Header(_) => 1,
            ListHit::Session(_) => 3,
        };
        if skip >= lines {
            skip -= lines;
            continue;
        }
        let visible = lines - skip;
        let next = y.saturating_add(visible as u16);
        if row >= y && row < next && usize::from(row.saturating_sub(body)) < room {
            return Some(piece.hit);
        }
        if usize::from(next.saturating_sub(body)) >= room {
            return None;
        }
        y = next;
        skip = 0;
    }
    None
}

pub fn file_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<String> {
    match todo_target(model, area, column, row) {
        Some(TodoTarget::File(path)) => return Some(path),
        Some(TodoTarget::Item(_) | TodoTarget::Link(_)) => return None,
        None => {}
    }
    if model.overlay.is_some() && overlay_line_at(model, area, column, row).is_some() {
        return None;
    }
    wrote_path_at(model, area, column, row)
}

pub fn link_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<String> {
    match todo_target(model, area, column, row) {
        Some(TodoTarget::Link(url)) => return http_url(&url),
        Some(TodoTarget::Item(_) | TodoTarget::File(_)) => return None,
        None => {}
    }
    if model.overlay.is_some() {
        let (index, width, col) = overlay_cell(model, area, column, row)?;
        return overlay_link_at(model.overlay.as_ref()?, width, index, col);
    }
    card_link_at(model, area, column, row)
}

pub fn delete_choice_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<usize> {
    let Overlay::Delete {
        path,
        remove_workspace,
        confirm_dirty,
        ..
    } = model.overlay.as_ref()?
    else {
        return None;
    };
    let (index, _, _) = overlay_cell(model, area, column, row)?;
    let overlay = model.overlay.as_ref()?;
    let rect = overlay_frame(model, area, overlay);
    let width = usize::from(Block::bordered().inner(rect).width).max(1);
    let lines = overlay_lines(overlay, width);
    let start = lines
        .len()
        .checked_sub(delete_labels(path.as_deref(), *remove_workspace, *confirm_dirty).len())?;
    (index >= start).then_some(index - start)
}

pub fn menu_item_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<usize> {
    let Overlay::Menu { items, .. } = model.overlay.as_ref()? else {
        return None;
    };
    let rect = menu_rect(model.overlay.as_ref()?, area);
    let inner = Block::bordered().inner(rect);
    if inner.width == 0 || inner.height == 0 || !inner.contains(Position { x: column, y: row }) {
        return None;
    }
    let index = usize::from(row.saturating_sub(inner.y));
    (index < items.len()).then_some(index)
}

pub fn menu_contains(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    let Some(overlay) = model.overlay.as_ref() else {
        return false;
    };
    matches!(overlay, Overlay::Menu { .. })
        && menu_rect(overlay, area).contains(Position { x: column, y: row })
}

pub fn overlay_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    let Some(overlay) = model.overlay.as_ref() else {
        return false;
    };
    overlay_frame(model, area, overlay).contains(Position { x: column, y: row })
}

pub fn overlay_close_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    let Some(overlay) = model.overlay.as_ref() else {
        return false;
    };
    if matches!(overlay, Overlay::Menu { .. }) {
        return false;
    }
    let frame = overlay_frame(model, area, overlay);
    frame.width > 0
        && frame.height > 0
        && row == frame.y
        && column == frame.x.saturating_add(frame.width.saturating_sub(1))
}

fn overlay_frame(model: &ScreenModel, area: Rect, overlay: &Overlay) -> Rect {
    if matches!(overlay, Overlay::Menu { .. }) {
        return menu_rect(overlay, area);
    }
    let pane = if matches!(overlay, Overlay::Pairing { .. }) {
        area
    } else {
        split_of(model, area).session
    };
    if matches!(
        overlay,
        Overlay::Image { .. } | Overlay::VisualQuestion { .. }
    ) {
        return crate::image_preview::popup(pane);
    }
    let width = overlay_inner_width(pane);
    let lines = overlay_lines(overlay, width);
    overlay_rect(pane, lines.len())
}

pub(super) fn overlay_line_at(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<(usize, usize)> {
    let (index, width, _) = overlay_cell(model, area, column, row)?;
    Some((index, width))
}

pub(super) fn overlay_cell(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<(usize, usize, usize)> {
    let overlay = model.overlay.as_ref()?;
    let rect = overlay_frame(model, area, overlay);
    let width = usize::from(Block::bordered().inner(rect).width).max(1);
    let lines = overlay_lines(overlay, width);
    let inner = Block::bordered().inner(rect);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    if !inner.contains(Position { x: column, y: row }) {
        return None;
    }
    let origin = if matches!(overlay, Overlay::File { .. } | Overlay::Text { .. }) {
        model.file_scroll.min(file_scroll_max(model, area))
    } else {
        0
    };
    let index = origin + usize::from(row.saturating_sub(inner.y));
    if index >= lines.len() {
        return None;
    }
    Some((index, width, usize::from(column.saturating_sub(inner.x))))
}

pub(super) fn overlay_link_at(
    overlay: &Overlay,
    width: usize,
    index: usize,
    column: usize,
) -> Option<String> {
    match overlay {
        Overlay::Pull { url } => {
            let rows = wrap(url, width).len().max(1);
            if index < rows {
                http_url(url)
            } else {
                None
            }
        }
        Overlay::File { path, text, .. } if file::is_markdown(path) => {
            let heading = wrapped(path, width, theme::faint()).len();
            let index = index.checked_sub(heading)?;
            markdown_link_at(&file::safe_text(text), width, index, column)
        }
        Overlay::Text { text } | Overlay::Proof { text, .. } | Overlay::Question { text, .. } => {
            if text.is_empty() {
                return None;
            }
            let rows = markdown_lines(text, width).len();
            if index >= rows {
                return None;
            }
            markdown_link_at(text, width, index, column)
        }
        _ => None,
    }
}

pub(super) fn card_link_at(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<String> {
    let inner = session_inner_of(model, area);
    if !inner.contains(Position { x: column, y: row }) {
        return None;
    }
    let index = column_index(model, inner, row)?;
    let width = usize::from(inner.width);
    let mut cursor = 1usize;
    if let Some(url) = model
        .selected_session()
        .and_then(|session| session.pull_url.as_deref())
    {
        cursor = cursor
            .saturating_add(wrap(&crate::session::opened_line(url), width).len())
            .saturating_add(1);
    }
    for (card_index, card) in model.cards.iter().enumerate() {
        let height = card.lines(width).len();
        if index >= cursor && index < cursor + height {
            return link_in_card(card, width, index - cursor, column, inner.x);
        }
        cursor = cursor.saturating_add(height);
        if card_index + 1 < model.cards.len() {
            cursor = cursor.saturating_add(1);
        }
    }
    None
}

pub(super) fn link_in_card(
    card: &Card,
    pane_width: usize,
    line_index: usize,
    column: u16,
    origin_x: u16,
) -> Option<String> {
    let text = match card {
        Card::Result { text } | Card::Proof { text, .. } | Card::Question { text, .. } => text,
        _ => return None,
    };
    if line_index == 0 || (card.preview_text().is_some() && line_index > 8) {
        return None;
    }
    let prose = column_width(card, pane_width).saturating_sub(cols(RAIL));
    let table = pane_width.saturating_sub(cols(RAIL));
    let col = usize::from(column.saturating_sub(origin_x));
    let rail = cols(RAIL);
    if col < rail {
        return None;
    }
    markdown_link_between(text, prose, table, line_index - 1, col - rail)
}

pub(super) fn http_url(value: &str) -> Option<String> {
    let value = value.trim();
    let rest = value
        .strip_prefix("https:")
        .or_else(|| value.strip_prefix("http:"))?;
    let bytes = rest.as_bytes();
    if bytes.first() == Some(&b'/') && bytes.get(1) == Some(&b'/') {
        Some(value.to_string())
    } else {
        None
    }
}

pub(super) fn wrote_path_at(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<String> {
    let inner = session_inner_of(model, area);
    if !inner.contains(Position { x: column, y: row }) {
        return None;
    }
    let index = column_index(model, inner, row)?;
    let width = usize::from(inner.width);
    let mut cursor = 1usize;
    if let Some(url) = model
        .selected_session()
        .and_then(|session| session.pull_url.as_deref())
    {
        cursor = cursor
            .saturating_add(wrap(&crate::session::opened_line(url), width).len())
            .saturating_add(1);
    }
    for (card_index, card) in model.cards.iter().enumerate() {
        let height = card.lines(width).len();
        if index >= cursor && index < cursor + height {
            return wrote_path_in_card(card, width, index - cursor);
        }
        cursor = cursor.saturating_add(height);
        if card_index + 1 < model.cards.len() {
            cursor = cursor.saturating_add(1);
        }
    }
    None
}

pub(super) fn wrote_path_in_card(card: &Card, width: usize, line_index: usize) -> Option<String> {
    let Card::Proof { text, .. } = card else {
        return None;
    };
    if line_index == 0 {
        return None;
    }
    let text_width = column_width(card, width).saturating_sub(RAIL.chars().count());
    let mut n = 1usize;
    for line in text.lines() {
        let rows = wrap(line, text_width).len().max(1);
        if line_index >= n && line_index < n + rows {
            return line
                .strip_prefix("Wrote ")
                .filter(|path| !path.is_empty())
                .map(str::to_string);
        }
        n += rows;
    }
    None
}

pub fn todo_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<String> {
    match todo_target(model, area, column, row) {
        Some(TodoTarget::Item(id)) => Some(id),
        _ => None,
    }
}

pub fn right_border_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    if !right_column_visible(model) || !model.right_open {
        return false;
    }
    let todos = split_of(model, area).todos;
    todos.height > 0
        && row >= todos.y
        && row < todos.y.saturating_add(todos.height)
        && column == todos.x
}

pub fn width_from_left_drag(column: u16) -> u16 {
    column.saturating_add(1).max(2)
}

pub fn width_from_right_drag(area_width: u16, column: u16) -> u16 {
    area_width.saturating_sub(column).max(2)
}

pub(super) fn card_opens_overlay(card: &Card) -> bool {
    match card {
        Card::Proof { text, .. } => !text.is_empty(),
        _ => card.is_waiting(),
    }
}

pub fn image_at(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<crate::attachment::ImageAttachment> {
    let inner = session_inner_of(model, area);
    let index = column_index(model, inner, row)?;
    if !inner.contains(Position { x: column, y: row }) {
        return None;
    }
    let width = usize::from(inner.width);
    let mut cursor = 1;
    if let Some(url) = model.selected_session().and_then(|s| s.pull_url.as_deref()) {
        cursor += wrap(&crate::session::opened_line(url), width).len() + 1;
    }
    for card in &model.cards {
        let height = card.lines(width).len();
        if let Card::Ask { images, .. } = card {
            let start = cursor + height - images.len();
            if index >= start && index < cursor + height {
                return images.get(index - start).cloned();
            }
        }
        cursor += height + 1;
    }
    None
}

pub fn queue_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> bool {
    if model.queue == 0 || model.overlay.is_some() {
        return false;
    }
    let Some(action) = session_action(model) else {
        return false;
    };
    let inner = session_inner_of(model, area);
    if inner.height == 0 {
        return false;
    }
    let start = inner
        .x
        .saturating_add(2 + UnicodeWidthStr::width(action) as u16);
    let end = start
        .saturating_add(format!(" queued {}", model.queue).len() as u16)
        .min(inner.right());
    row == inner.bottom().saturating_sub(1) && column >= start && column < end
}

pub fn queue_choice_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<usize> {
    let Overlay::Queue { rows, .. } = model.overlay.as_ref()? else {
        return None;
    };
    let (line, _, _) = overlay_cell(model, area, column, row)?;
    let index = line.checked_sub(1)?;
    (index < rows.len()).then_some(index)
}
