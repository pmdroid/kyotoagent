use super::*;

pub fn mouse(event: MouseEvent, model: &ScreenModel, area: Rect) -> Option<Effect> {
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => left_down(model, area, event.column, event.row),
        MouseEventKind::Down(MouseButton::Right) => {
            match screen::list_at(model, area, event.column, event.row) {
                Some(screen::ListHit::Session(id)) => Some(Effect::OpenMenu {
                    id,
                    column: event.column,
                    row: event.row,
                }),
                _ => None,
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => match model.drag {
            Some(Drag::Left) => Some(Effect::SetLeftWidth(screen::width_from_left_drag(
                event.column,
            ))),
            Some(Drag::Right) => Some(Effect::SetRightWidth(screen::width_from_right_drag(
                area.width,
                event.column,
            ))),
            None => drag_select(model, area, event.column, event.row),
        },
        MouseEventKind::Up(MouseButton::Left) => match model.drag {
            Some(Drag::Left) if event.column == model.drag_origin => Some(Effect::ToggleLeft),
            Some(Drag::Right) if event.column == model.drag_origin => Some(Effect::ToggleRight),
            Some(_) => Some(Effect::EndDrag),
            None => release_select(model, area, event.column, event.row),
        },
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let list = screen::split_of(model, area).list;
            if list.contains(Position {
                x: event.column,
                y: event.row,
            }) {
                return Some(Effect::ScrollList {
                    up: matches!(event.kind, MouseEventKind::ScrollUp),
                });
            }
            if screen::right_pane_at(model, area, event.column, event.row).is_some() {
                return Some(match event.kind {
                    MouseEventKind::ScrollUp => Effect::ScrollUp,
                    _ => Effect::ScrollDown,
                });
            }
            if screen::overlay_at(model, area, event.column, event.row) {
                if matches!(
                    model.overlay,
                    Some(
                        screen::Overlay::Palette { .. }
                            | screen::Overlay::Help { .. }
                            | screen::Overlay::File { .. }
                            | screen::Overlay::Text { .. }
                    )
                ) {
                    return Some(match event.kind {
                        MouseEventKind::ScrollUp => Effect::ScrollUp,
                        _ => Effect::ScrollDown,
                    });
                }
                return None;
            }
            let inner = screen::session_inner_of(model, area);
            if !inner.contains(Position {
                x: event.column,
                y: event.row,
            }) {
                return None;
            }
            Some(match event.kind {
                MouseEventKind::ScrollUp => Effect::ScrollUp,
                _ => Effect::ScrollDown,
            })
        }
        _ => None,
    }
}

fn left_down(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<Effect> {
    if screen::toast_close_at(model, area, column, row) {
        return Some(Effect::DismissNotice);
    }
    if screen::toast_rect(model, area)
        .is_some_and(|rect| rect.contains(Position { x: column, y: row }))
    {
        return None;
    }
    if screen::queue_at(model, area, column, row) {
        return Some(Effect::OpenQueue);
    }
    if screen::overlay_close_at(model, area, column, row) {
        return Some(Effect::CloseOverlay);
    }
    if let Some(index) = screen::queue_choice_at(model, area, column, row) {
        return Some(Effect::SelectQueued(index));
    }
    if let Some(index) = screen::delete_choice_at(model, area, column, row) {
        return Some(Effect::ChooseDelete(index));
    }
    if let Some(index) = screen::menu_item_at(model, area, column, row) {
        return Some(Effect::MenuItem(index));
    }
    if screen::menu_contains(model, area, column, row) {
        return None;
    }
    if model.overlay.is_some() && !screen::overlay_at(model, area, column, row) {
        return None;
    }
    plain_left_down(model, area, column, row)
}

fn plain_left_down(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<Effect> {
    if let Some(effect) = image_press(model, area, column, row) {
        return Some(effect);
    }
    if let Some(point) = screen::selection_anchor(model, area, column, row) {
        return Some(Effect::ArmSelect(TextSelect::begin(point, column, row)));
    }
    press_at(model, area, column, row)
}

pub(super) fn press_at(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<Effect> {
    if screen::split_of(model, area)
        .input
        .contains(Position { x: column, y: row })
    {
        if let Some(text) = &model.pasted_text {
            return Some(Effect::OpenText(text.clone()));
        }
    }
    if screen::queue_at(model, area, column, row) {
        return Some(Effect::OpenQueue);
    }
    if let Some(file) = screen::artifact_at(model, area, column, row) {
        return Some(Effect::OpenArtifact(file));
    }
    if let Some(action) = screen::proof_action_at(model, area, column, row) {
        return Some(Effect::ProofAction(action));
    }
    if screen::context_at(model, area, column, row) {
        return Some(Effect::OpenContext);
    }
    if screen::list_glyph_at(model, area, column, row) {
        return Some(Effect::ToggleLeft);
    }
    if screen::panes_glyph_at(model, area, column, row) {
        return Some(Effect::ToggleRight);
    }
    if let Some(pane) = screen::pane_close_at(model, area, column, row) {
        return Some(Effect::TogglePane(pane));
    }
    if model.overlay.is_none() && screen::thinking_at(model, area, column, row) {
        return Some(Effect::OpenThinking);
    }
    match screen::todo_target(model, area, column, row) {
        Some(screen::TodoTarget::File(path)) => return Some(Effect::OpenFile(path)),
        Some(screen::TodoTarget::Item(id)) => return Some(Effect::OpenTodo(id)),
        Some(screen::TodoTarget::Link(_)) | None => {}
    }
    if let Some(url) = screen::link_at(model, area, column, row) {
        return Some(Effect::OpenLink(url));
    }
    if let Some(text) = screen::preview_text_at(model, area, column, row) {
        return Some(Effect::OpenText(text));
    }
    if model.overlay.is_some() {
        if let Some(path) = screen::file_at(model, area, column, row) {
            return Some(Effect::OpenFile(path));
        }
    }
    if let Some(id) = screen::task_row_at(model, area, column, row) {
        return Some(Effect::OpenTask(id));
    }
    if let Some(id) = screen::closeout_row_at(model, area, column, row) {
        return Some(Effect::OpenCheck(id));
    }
    if screen::pane_title_at(model, area, column, row) {
        return None;
    }
    if screen::pull_target_at(model, area, column, row) {
        Some(Effect::OpenPull)
    } else if screen::overlay_card_at(model, area, column, row) {
        Some(Effect::OpenOverlay)
    } else if screen::left_rail_at(model, area, column, row) {
        Some(Effect::ToggleLeft)
    } else if screen::left_border_at(model, area, column, row) {
        Some(Effect::DragLeft(column))
    } else if screen::right_border_at(model, area, column, row) {
        Some(Effect::DragRight(column))
    } else {
        match screen::list_at(model, area, column, row) {
            Some(screen::ListHit::Session(id)) => Some(Effect::SelectSession(id)),
            Some(screen::ListHit::Header(id)) => Some(Effect::ToggleHeader(id)),
            Some(screen::ListHit::Filter) => {
                let list = screen::split_of(model, area).list;
                let inner = ratatui::widgets::Block::bordered().inner(list);
                let column = usize::from(column.saturating_sub(inner.x));
                screen::filter_at(column).map(Effect::SetListFilter)
            }
            None => None,
        }
    }
}

pub(super) fn drag_select(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<Effect> {
    let sel = model.select.filter(|sel| sel.held)?;
    let point = screen::selection_anchor(model, area, column, row)?;
    if point.place != sel.anchor.place {
        return None;
    }
    Some(Effect::MoveSelect(point))
}

pub(super) fn release_select(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<Effect> {
    let mut sel = model.select.filter(|sel| sel.held)?;
    if let Some(point) = screen::selection_anchor(model, area, column, row) {
        if point.place == sel.anchor.place {
            sel.end = point;
        }
    }
    if sel.is_empty() {
        return press_at(model, area, sel.x, sel.y);
    }
    let text = screen::selection_text(model, area, sel);
    Some(Effect::CopyText(text, sel.end))
}

pub fn track_mouse(app: &mut App, area: Rect, event: MouseEvent) -> Option<Effect> {
    let effect = mouse(event, &screen_model(app), area);
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if !matches!(effect, Some(Effect::ArmSelect(_))) {
                app.select = None;
            }
            effect
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if !matches!(effect, Some(Effect::CopyText(_, _)))
                && app.select.is_some_and(|sel| sel.held)
            {
                app.select = None;
            }
            effect
        }
        _ => effect,
    }
}

pub(super) fn pointer(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

pub fn released_click(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<Effect> {
    let down = pointer(MouseEventKind::Down(MouseButton::Left), column, row);
    match mouse(down, model, area) {
        Some(Effect::ArmSelect(sel)) => {
            let mut held = model.clone();
            held.select = Some(sel);
            let up = pointer(MouseEventKind::Up(MouseButton::Left), column, row);
            match mouse(up, &held, area) {
                Some(Effect::CopyText(_, _)) => None,
                other => other,
            }
        }
        other => other,
    }
}

fn image_press(model: &ScreenModel, area: Rect, column: u16, row: u16) -> Option<Effect> {
    if model.overlay.is_some() {
        return None;
    }
    let input = screen::split_of(model, area).input;
    let input = if screen::composer_framed(model, area.height) {
        ratatui::widgets::Block::bordered().inner(input)
    } else {
        input
    };
    let first = input.y
        + input
            .height
            .saturating_sub(model.pending_images.len() as u16);
    if row >= first && input.contains(Position { x: column, y: row }) {
        let index = usize::from(row - first);
        let image = model.pending_images.get(index)?;
        let name = screen::image_chip_name(image, input.width);
        let end = input.x + 3 + unicode_width::UnicodeWidthStr::width(name.as_str()) as u16;
        return if column >= end && column < end + 4 {
            Some(Effect::RemoveImage(index))
        } else if column < end {
            Some(Effect::OpenImage(image.clone()))
        } else {
            None
        };
    }
    screen::image_at(model, area, column, row).map(Effect::OpenImage)
}
