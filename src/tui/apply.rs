use super::*;

pub fn apply_pane(app: &mut App, effect: Effect) -> bool {
    match effect {
        Effect::TogglePane(pane) => {
            toggle_pane(app, pane);
            true
        }
        Effect::ToggleLeft => {
            app.left_open = !app.left_open;
            if app.left_open && app.left_width < 2 {
                app.left_width = LIST_WIDTH;
            }
            app.drag = None;
            true
        }
        Effect::ToggleRight => {
            app.right_open = !app.right_open;
            if app.right_open {
                if app.right_width < 2 {
                    app.right_width = TODOS_WIDTH;
                }
                if app.right_panes.is_empty() {
                    app.right_panes.insert(RightPane::Todos);
                }
            }
            app.drag = None;
            true
        }
        Effect::DragLeft(column) => {
            app.drag = Some(Drag::Left);
            app.drag_origin = column;
            true
        }
        Effect::DragRight(column) => {
            app.drag = Some(Drag::Right);
            app.drag_origin = column;
            true
        }
        Effect::SetLeftWidth(width) => {
            app.drag = Some(Drag::Left);
            let mut model = layout_model(app);
            model.left_open = true;
            model.left_width = width;
            let (left, _) = screen::side_widths(&model, app.area.width);
            app.left_open = left > 1;
            app.left_width = if app.left_open { left } else { LIST_WIDTH };
            true
        }
        Effect::SetRightWidth(width) => {
            app.drag = Some(Drag::Right);
            let mut model = layout_model(app);
            model.right_open = true;
            model.right_width = width;
            let (_, right) = screen::side_widths(&model, app.area.width);
            app.right_open = right > 1;
            app.right_width = if app.right_open { right } else { TODOS_WIDTH };
            true
        }
        Effect::EndDrag => {
            app.drag = None;
            true
        }
        Effect::ArmSelect(sel) => {
            app.select = Some(sel);
            true
        }
        Effect::MoveSelect(point) => {
            if let Some(sel) = &mut app.select {
                if sel.anchor.place == point.place {
                    sel.end = point;
                }
            }
            true
        }
        Effect::CopyText(text, end) => {
            if let Some(sel) = &mut app.select {
                if sel.anchor.place == end.place {
                    sel.end = end;
                }
                sel.held = false;
            }
            commit_copy(app, &text);
            true
        }
        Effect::ToggleHeader(id) => {
            app.list_header = Some(id.clone());
            if !app.collapsed.remove(&id) {
                app.collapsed.insert(id);
            }
            true
        }
        Effect::CollapseHeader => {
            if let Some(id) = app.list_header.clone() {
                app.collapsed.insert(id);
            }
            true
        }
        Effect::ExpandHeader => {
            if let Some(id) = app.list_header.clone() {
                app.collapsed.remove(&id);
            }
            true
        }
        Effect::SetListFilter(filter) => {
            app.list_filter = filter;
            app.list_scroll = 0;
            true
        }
        Effect::CycleListFilter => {
            app.list_filter = app.list_filter.next();
            app.list_scroll = 0;
            true
        }
        Effect::OpenMenu { id, column, row } => {
            let archived = app
                .sessions
                .iter()
                .find(|session| session.id == id)
                .is_some_and(|session| session.archived);
            app.menu = Some(SessionMenu {
                id,
                column,
                row,
                items: screen::session_menu_items(archived),
            });
            true
        }
        _ => false,
    }
}

pub async fn apply(app: &mut App, client: &Client, effect: Effect) -> Result<bool, String> {
    Box::pin(apply_action(app, client, effect)).await
}

async fn apply_action(app: &mut App, client: &Client, effect: Effect) -> Result<bool, String> {
    providers::advance(app, client).await;
    catalog::advance(app).await;
    if proof_files::handle(app, client, &effect) {
        return Ok(true);
    }
    if projects::handle(app, client, &effect) {
        return Ok(true);
    }
    if catalog::handle(app, client, &effect) {
        return Ok(true);
    }
    if providers::handle(app, client, &effect) {
        return Ok(true);
    }
    if connections::handle(app, client, &effect) {
        return Ok(true);
    }
    if apply_pane(app, effect.clone()) {
        return Ok(true);
    }
    match effect {
        Effect::SaveLayout => layout::save(app, client),
        Effect::DismissNotice => app.notice = None,
        Effect::OpenProjects => {
            release_command_surfaces(app);
            catalog::open(app, client, catalog::Target::ManageProjects);
        }
        Effect::OpenQueue => queue::open(app),
        Effect::RemoveQueued => queue::remove(app, client),
        Effect::SelectQueued(index) => {
            app.queue_highlight = app.queue_items.get(index).map(|item| item.id.clone());
        }
        Effect::Exit => return Ok(false),
        Effect::SelectNext => {
            if let Some(index) = app.sessions.iter().position(|row| row.id == app.selected) {
                if let Some(next) = app.sessions.get(index + 1) {
                    select_session(app, next.id.clone());
                }
            }
        }
        Effect::SelectPrev => {
            if let Some(index) = app.sessions.iter().position(|row| row.id == app.selected) {
                if index > 0 {
                    select_session(app, app.sessions[index - 1].id.clone());
                }
            }
        }
        Effect::SelectSession(id) => {
            select_session(app, id);
        }
        Effect::ScrollList { up } => scroll_list(app, up, 1),
        Effect::ScrollUp => {
            if app.queue_open {
                queue::nudge(app, true);
                return Ok(true);
            }
            if nudge_delete(app, true) {
                return Ok(true);
            }
            if !scroll_file(app, true, 1)
                && !scroll_closeout(app, true, 1)
                && !scroll_command_ui(app, true, 1)
            {
                if let Some(picker) = &mut app.picker {
                    match picker {
                        Picker::Model { highlight, .. } | Picker::Effort { highlight, .. } => {
                            *highlight = highlight.saturating_sub(1);
                        }
                    }
                } else if picker_is_open(app) {
                    app.skill_highlight = app.skill_highlight.saturating_sub(1);
                } else {
                    scroll_lines(app, true, 1);
                }
            }
        }
        Effect::ScrollDown => {
            if app.queue_open {
                queue::nudge(app, false);
                return Ok(true);
            }
            if nudge_delete(app, false) {
                return Ok(true);
            }
            if !scroll_file(app, false, 1)
                && !scroll_closeout(app, false, 1)
                && !scroll_command_ui(app, false, 1)
            {
                if let Some(picker) = &mut app.picker {
                    let last = match picker {
                        Picker::Model { rows, query, .. } => {
                            filtered_model_indices(rows, query).len().saturating_sub(1)
                        }
                        Picker::Effort { rows, .. } => rows.len().saturating_sub(1),
                    };
                    match picker {
                        Picker::Model { highlight, .. } | Picker::Effort { highlight, .. } => {
                            *highlight = (*highlight + 1).min(last);
                        }
                    }
                } else if picker_is_open(app) {
                    let last = picker_matches(app).len().saturating_sub(1);
                    app.skill_highlight = (app.skill_highlight + 1).min(last);
                } else {
                    scroll_lines(app, false, 1);
                }
            }
        }
        Effect::PageUp => {
            let step = closeout_page(app);
            if !scroll_file(app, true, pane_rows(app).saturating_sub(4).max(1))
                && !scroll_closeout(app, true, step)
                && !scroll_command_ui(app, true, pane_rows(app))
            {
                scroll_lines(app, true, pane_rows(app));
            }
        }
        Effect::PageDown => {
            let step = closeout_page(app);
            if !scroll_file(app, false, pane_rows(app).saturating_sub(4).max(1))
                && !scroll_closeout(app, false, step)
                && !scroll_command_ui(app, false, pane_rows(app))
            {
                scroll_lines(app, false, pane_rows(app));
            }
        }
        Effect::OpenImage(image) => {
            release_command_surfaces(app);
            app.context_open = false;
            app.open_image = Some(image);
            app.overlay = true;
        }
        Effect::RemoveImage(index) => {
            if let Some(images) = app.images.get_mut(&app.selected) {
                if index < images.len() {
                    images.remove(index);
                }
            }
        }
        Effect::Paste(text) => {
            proof::focus(app, None);
            if app.queue_open
                || app
                    .open_image
                    .as_ref()
                    .is_some_and(|image| visual_question(app, image).is_none())
                || app.open_text.is_some()
                || app.open_file.is_some()
                || app.delete_confirm.is_some()
                || choosing(app)
                || app.command_ui.is_some()
                || app.picker.is_some()
            {
                return Ok(true);
            }
            let question = matches!(mode(app), Mode::QuestionText | Mode::Question { .. });
            if !question
                && matches!(mode(app), Mode::Idle | Mode::Working)
                && images::attach_drop(app, &text)
            {
                return Ok(true);
            }
            let input = if question {
                &mut app.question_text
            } else {
                &mut app.ask
            };
            app.pastes
                .retain(|paste| paste.question != question || paste_matches(input, paste));
            let start = input.len();
            input.push_str(&text);
            if text.chars().count() > 2048 || text.lines().count() > 12 {
                app.pastes.push(PastedInput {
                    start,
                    text,
                    question,
                });
            }
            if !question {
                after_ask_edit(app);
            }
        }
        Effect::Type(c) => {
            proof::focus(app, None);
            if app.queue_open || app.delete_confirm.is_some() || choosing(app) {
                return Ok(true);
            }
            if edit_palette_query(app, Some(c)) || edit_open_model_query(app, Some(c)) {
                return Ok(true);
            }
            if app.picker.is_some() || matches!(app.command_ui, Some(CommandUi::Help { .. })) {
                return Ok(true);
            }
            match mode(app) {
                Mode::QuestionText | Mode::Question { .. } => app.question_text.push(c),
                _ => {
                    app.ask.push(c);
                    after_ask_edit(app);
                }
            }
        }
        Effect::DeleteWord | Effect::DeleteLine => edit_delete(app, effect),
        Effect::Backspace => {
            if app.queue_open || app.delete_confirm.is_some() || choosing(app) {
                return Ok(true);
            }
            if edit_palette_query(app, None) || edit_open_model_query(app, None) {
                return Ok(true);
            }
            if app.picker.is_some() || matches!(app.command_ui, Some(CommandUi::Help { .. })) {
                return Ok(true);
            }
            match mode(app) {
                Mode::QuestionText | Mode::Question { .. } => {
                    if !delete_pasted_tail(&mut app.question_text, &mut app.pastes, true) {
                        app.question_text.pop();
                    }
                }
                _ => {
                    if !delete_pasted_tail(&mut app.ask, &mut app.pastes, false) {
                        app.ask.pop();
                    }
                    after_ask_edit(app);
                }
            }
        }
        Effect::Submit => {
            if app.delete_confirm.is_some() {
                deletion::decide(app, client);
                return Ok(true);
            }
            if matches!(app.command_ui, Some(CommandUi::Palette { .. })) {
                run_palette(app, client).await;
                return Ok(true);
            }
            if matches!(app.command_ui, Some(CommandUi::Help { .. })) {
                return Ok(true);
            }
            if app.picker.is_some() {
                apply_picker(app, client).await;
                return Ok(true);
            }
            if choosing(app) {
                return Ok(true);
            }
            if fill_skill_picker(app) {
                return Ok(true);
            }
            submit(app, client).await?;
        }
        Effect::AllowOnce => answer_permission(app, client, "allow_once").await?,
        Effect::AllowSession => answer_permission(app, client, "allow_session").await?,
        Effect::Deny => {
            if choosing(app) {
                close_overlay(app);
            } else {
                answer_permission(app, client, "deny").await?;
            }
        }
        Effect::Choose(index) => {
            if app.proof_popup.is_some() {
                proof::choose_visible(app, client, index);
                return Ok(true);
            }
            if app.server_step.is_some() {
                connections::choose_server(app, client, index);
            } else if profile_open(&app.profile_step) {
                finish_profile_prompt(app, client, index).await?;
            } else if workspace_open(&app.workspace_step) {
                finish_workspace_prompt(app, client, index).await?;
            } else {
                answer_choice(app, client, index).await?;
            }
        }
        Effect::NewSession => begin_new_session(app, client).await?,
        Effect::Cancel => cancel_turn(app, client).await?,
        Effect::ToggleYolo => {
            let next = !app.yolo;
            post_yolo(app, client, next).await?;
            poll(app, client).await?;
        }
        Effect::ProofFileMove | Effect::ProofFileChoose(_) => {}
        Effect::OpenProof => proof::open(app),
        Effect::OpenCheckTranscripts => proof::open_checks(app),
        Effect::OpenArtifact(file) => {
            release_command_surfaces(app);
            proof_files::open(app, client, file);
        }
        Effect::FocusArtifact(id) => proof::focus(app, id),
        Effect::ProofMove(up) => proof::move_selection(app, up),
        Effect::ProofChoose(index) => proof::choose(app, client, index),
        Effect::ProofAction(action) => proof::activate(app, client, action),
        Effect::OpenText(text) => {
            release_command_surfaces(app);
            app.context_open = false;
            app.open_image = None;
            app.open_text = Some(text);
            app.file_scroll = 0;
            app.thinking_open = false;
            app.open_file = None;
            app.overlay = true;
            app.select = None;
        }
        Effect::OpenOverlay => {
            app.thinking_open = false;
            app.overlay_pull = false;
            app.open_todo = None;
            app.open_file = None;
            app.open_text = None;
            if overlay_from(&app.cards, &app.question_text).is_some() {
                app.overlay = true;
            }
        }
        Effect::OpenPull => {
            app.thinking_open = false;
            if app
                .selected_session()
                .and_then(|row| row.pull_url.as_ref())
                .is_some()
            {
                app.overlay_pull = true;
                app.open_todo = None;
                app.open_file = None;
                app.open_text = None;
                app.overlay = true;
            }
        }
        Effect::OpenTodo(id) => focus_todo(app, id),
        Effect::OpenCloseout => toggle_pane(app, RightPane::Closeout),
        Effect::OpenCheck(id) => {
            app.right_panes.insert(RightPane::Closeout);
            show_column(app);
            if app.open_check.as_deref() == Some(id.as_str()) {
                app.open_check = None;
            } else {
                app.open_check = Some(id);
            }
            app.closeout_scroll = 0;
        }
        Effect::OpenFile(path) => open_file_overlay(app, client, &path).await?,
        Effect::OpenLink(url) => open_link(app, &url),
        Effect::OpenTasks => open_tasks_overlay(app),
        Effect::OpenTask(id) => open_task_overlay(app, client, &id).await?,
        Effect::OpenSchedules => open_schedules_overlay(app),
        Effect::OpenThinking => open_thinking_overlay(app),
        Effect::OpenContext => open_context_overlay(app),
        Effect::CloseOverlay => {
            if enhance_is_esc_target(app) {
                answer_enhance(app, client, "discard", None).await?;
            } else {
                close_overlay(app);
            }
        }
        Effect::EnhanceUse => answer_enhance(app, client, "use", None).await?,
        Effect::EnhanceDiscard => answer_enhance(app, client, "discard", None).await?,
        Effect::EnhanceRetry => answer_enhance(app, client, "retry", None).await?,
        Effect::EnhanceEdit => {
            if let Some((text, source)) = enhance_draft(app) {
                app.ask = if text.is_empty() { source } else { text };
            }
        }
        Effect::MenuItem(index) => run_menu(app, client, index).await?,
        Effect::OpenMenu { .. } => {}
        Effect::ToggleDeleteWorkspace => {
            deletion::toggle_workspace(app);
        }
        Effect::ConfirmDelete => {
            let id = app.selected.clone();
            open_delete_confirm(app, client, &id);
        }
        Effect::ChooseDelete(index) => {
            if let Some(confirm) = &mut app.delete_confirm {
                confirm.highlight = index;
            }
            deletion::decide(app, client);
        }
        Effect::ArchiveSession => archive_session(app, client, true).await?,
        Effect::UnarchiveSession => archive_session(app, client, false).await?,
        Effect::OpenModel => {
            open_model_picker(app, client).await;
        }
        Effect::OpenProviders => providers::open(app, client),
        Effect::OpenPalette => open_palette(app),
        Effect::OpenHelp => open_help(app),
        Effect::TogglePane(_)
        | Effect::ToggleLeft
        | Effect::ToggleRight
        | Effect::DragLeft(_)
        | Effect::DragRight(_)
        | Effect::SetLeftWidth(_)
        | Effect::SetRightWidth(_)
        | Effect::EndDrag
        | Effect::ArmSelect(_)
        | Effect::MoveSelect(_)
        | Effect::CopyText(_, _)
        | Effect::ToggleHeader(_)
        | Effect::CollapseHeader
        | Effect::ExpandHeader
        | Effect::SetListFilter(_)
        | Effect::CycleListFilter => unreachable!("pane effects return from apply_pane"),
    }
    Ok(true)
}

pub(super) async fn run_menu(app: &mut App, client: &Client, index: usize) -> Result<(), String> {
    let Some(menu) = app.menu.clone() else {
        return Ok(());
    };
    app.menu = None;
    match menu.items.get(index).map(String::as_str) {
        Some(screen::MENU_CLOSE) => open_delete_confirm(app, client, &menu.id),
        Some(screen::MENU_ARCHIVE) => archive_id(app, client, &menu.id, true).await?,
        Some(screen::MENU_UNARCHIVE) => archive_id(app, client, &menu.id, false).await?,
        _ => {}
    }
    Ok(())
}

pub(super) async fn archive_session(
    app: &mut App,
    client: &Client,
    archived: bool,
) -> Result<(), String> {
    if app.selected.is_empty() {
        return Ok(());
    }
    archive_id(app, client, &app.selected.clone(), archived).await
}

async fn archive_id(
    app: &mut App,
    client: &Client,
    id: &str,
    archived: bool,
) -> Result<(), String> {
    if app
        .sessions
        .iter()
        .find(|row| row.id == id)
        .is_some_and(|row| row.archived == archived)
    {
        return Ok(());
    }
    let id = id.to_string();
    let body = serde_json::json!({ "archived": archived }).to_string();
    let (status, response) = client
        .request("POST", &format!("/v1/sessions/{id}/archive"), Some(&body))
        .await?;
    if status != 204 {
        app.notice = Some(error_text(&response, status));
        return Ok(());
    }
    if archived && app.selected == id {
        select_session(
            app,
            next_list_id(
                &app.sessions
                    .iter()
                    .filter(|row| !row.archived && row.id != id)
                    .cloned()
                    .collect::<Vec<_>>(),
                &id,
            )
            .unwrap_or_default(),
        );
    }
    app.collapsed.remove(screen::ARCHIVED_GROUP);
    poll(app, client).await?;
    Ok(())
}

fn nudge_delete(app: &mut App, up: bool) -> bool {
    let Some(confirm) = &mut app.delete_confirm else {
        return false;
    };
    if up {
        confirm.highlight = confirm.highlight.saturating_sub(1);
    } else {
        let worktree = app
            .sessions
            .iter()
            .any(|row| row.id == confirm.id && row.worktree);
        confirm.highlight = (confirm.highlight + 1).min(if worktree { 2 } else { 1 });
    }
    true
}

pub(super) fn open_delete_confirm(app: &mut App, client: &Client, id: &str) {
    let Some(row) = app.sessions.iter().find(|row| row.id == id) else {
        return;
    };
    let worktree = row.worktree;
    release_command_surfaces(app);
    app.delete_confirm = Some(DeleteConfirm {
        id: id.to_string(),
        highlight: usize::from(worktree),
        status: None,
        remove_workspace: false,
        confirm_dirty: false,
        error: None,
        busy: worktree,
    });
    if worktree {
        deletion::fetch_status(app, client, id);
    }
}

pub(super) fn release_command_surfaces(app: &mut App) {
    app.proof_popup = None;
    proof_files::close(app);
    app.queue_open = false;
    providers::close(app);
    catalog::close(app);
    projects::close(app);
    connections::close(app);
    app.server_step = None;
    deletion::close(app);
    app.menu = None;
    app.picker = None;
    app.thinking_open = false;
    app.overlay = false;
    app.overlay_pull = false;
    app.workspace_step = WorkspaceStep::Off;
    app.profile_step = None;
    app.open_file = None;
    app.open_text = None;
    app.command_ui = None;
}

pub(super) fn open_palette(app: &mut App) {
    release_command_surfaces(app);
    app.command_ui = Some(CommandUi::Palette {
        query: String::new(),
        highlight: 0,
        scroll: 0,
    });
}

pub(super) fn open_help(app: &mut App) {
    release_command_surfaces(app);
    app.command_ui = Some(CommandUi::Help { scroll: 0 });
}

pub(super) fn scroll_command_ui(app: &mut App, up: bool, step: usize) -> bool {
    let Some(ui) = app.command_ui.clone() else {
        return false;
    };
    match ui {
        CommandUi::Palette {
            highlight,
            query,
            scroll,
        } => {
            let len = filtered_commands(&app.skills, &query).len();
            let last = len.saturating_sub(1);
            let highlight = if up {
                highlight.saturating_sub(step)
            } else {
                highlight.saturating_add(step).min(last)
            };
            let (highlight, scroll) =
                fit_palette_window(highlight, scroll, len, palette_visible_rows(app));
            app.command_ui = Some(CommandUi::Palette {
                query,
                highlight,
                scroll,
            });
        }
        CommandUi::Help { scroll } => {
            let last = command_catalog(&app.skills).len().saturating_sub(1);
            let scroll = if up {
                scroll.saturating_sub(step)
            } else {
                scroll.saturating_add(step).min(last)
            };
            app.command_ui = Some(CommandUi::Help { scroll });
        }
    }
    true
}

pub(super) fn edit_palette_query(app: &mut App, typed: Option<char>) -> bool {
    let Some(CommandUi::Palette {
        query,
        highlight,
        scroll,
    }) = &mut app.command_ui
    else {
        return false;
    };
    match typed {
        Some(c) => query.push(c),
        None => {
            query.pop();
        }
    }
    *highlight = 0;
    *scroll = 0;
    true
}

pub(super) async fn run_palette(app: &mut App, client: &Client) {
    let Some(CommandUi::Palette {
        query, highlight, ..
    }) = app.command_ui.clone()
    else {
        return;
    };
    let rows = filtered_commands(&app.skills, &query);
    let Some(row) = rows.get(highlight).cloned() else {
        return;
    };
    app.command_ui = None;
    match row.action {
        CommandAction::NewSession => {
            let _ = begin_new_session(app, client).await;
        }
        CommandAction::OpenModel => open_model_picker(app, client).await,
        CommandAction::OpenEffort => open_effort_picker(app, client).await,
        CommandAction::OpenProfile => open_profile_picker(app, client).await,
        CommandAction::OpenServer => connections::open_server_picker(app),
        CommandAction::OpenProjects => {
            release_command_surfaces(app);
            catalog::open(app, client, catalog::Target::ManageProjects);
        }
        CommandAction::OpenProviders => providers::open(app, client),
        CommandAction::Goal => {
            app.ask = "/goal ".to_string();
        }
        CommandAction::Compact => {
            if app.selected.is_empty() {
                return;
            }
            let _ = client
                .request(
                    "POST",
                    &format!("/v1/sessions/{}/compact", app.selected),
                    None,
                )
                .await;
            let _ = poll(app, client).await;
        }
        CommandAction::ToggleYolo => {
            let next = !app.yolo;
            let _ = post_yolo(app, client, next).await;
            let _ = poll(app, client).await;
        }
        CommandAction::ToggleEnhance => {
            let next = !app.enhance;
            let _ = post_enhance(app, client, next).await;
            let _ = poll(app, client).await;
        }
        CommandAction::ToggleLeft => {
            apply_pane(app, Effect::ToggleLeft);
        }
        CommandAction::CycleListFilter => {
            apply_pane(app, Effect::CycleListFilter);
        }
        CommandAction::ToggleRight => {
            apply_pane(app, Effect::ToggleRight);
        }
        CommandAction::SaveLayout => layout::save(app, client),
        CommandAction::TogglePane(pane) => toggle_pane(app, pane),
        CommandAction::Cancel => {
            let _ = cancel_turn(app, client).await;
        }
        CommandAction::CloseSession => {
            let id = app.selected.clone();
            if id.is_empty() {
                return;
            }
            open_delete_confirm(app, client, &id);
        }
        CommandAction::ArchiveSession => {
            let _ = archive_session(app, client, true).await;
        }
        CommandAction::UnarchiveSession => {
            let _ = archive_session(app, client, false).await;
        }
        CommandAction::OpenQueue => queue::open(app),
        CommandAction::OpenProof => proof::open(app),
        CommandAction::OpenCheckTranscripts => proof::open_checks(app),
        CommandAction::CloseOverlay => close_overlay(app),
        CommandAction::Help => open_help(app),
    }
}

pub(super) async fn open_model_picker(app: &mut App, client: &Client) {
    release_command_surfaces(app);
    catalog::open(app, client, catalog::Target::Models);
}

pub(super) fn show_model_picker(app: &mut App, rows: Vec<ModelRow>) -> Result<(), String> {
    if rows.is_empty() {
        return Err("No models are available on this server.".into());
    }
    let highlight = rows
        .iter()
        .position(|row| row.id == app.model && row.provider.is_none())
        .unwrap_or(0);
    app.overlay = false;
    app.overlay_pull = false;
    app.workspace_step = WorkspaceStep::Off;
    app.profile_step = None;
    app.open_todo = None;
    app.open_file = None;
    app.open_text = None;
    app.thinking_open = false;
    app.picker = Some(Picker::Model {
        rows,
        highlight,
        query: String::new(),
    });
    Ok(())
}

pub(super) fn show_effort_picker(app: &mut App, rows: Vec<String>) {
    app.command_ui = None;
    if rows.is_empty() {
        app.picker = None;
        app.notice = Some("This model has no available reasoning effort options.".into());
        return;
    }
    let highlight = app
        .effort
        .as_ref()
        .and_then(|effort| rows.iter().position(|row| row == effort))
        .unwrap_or(0);
    app.thinking_open = false;
    app.picker = Some(Picker::Effort { rows, highlight });
}

pub(super) async fn effort_rows(app: &App, client: &Client) -> Result<Vec<String>, String> {
    if app.server.is_none() {
        return Ok(EFFORTS.iter().map(|row| (*row).to_string()).collect());
    }
    let (status, body) = client.request("GET", "/v1/models", None).await?;
    if status != 200 {
        return Err(error_text(&body, status));
    }
    let rows: Vec<ModelRow> =
        serde_json::from_str(&body).map_err(|_| "Invalid model list from server.")?;
    Ok(rows
        .into_iter()
        .find(|row| row.matches(&app.model) && row.provider.is_none())
        .map(|row| row.reasoning_efforts)
        .unwrap_or_default())
}

pub(super) async fn open_effort_picker(app: &mut App, client: &Client) {
    release_command_surfaces(app);
    if app.server.is_none() {
        show_effort_picker(app, EFFORTS.iter().map(|row| (*row).to_string()).collect());
    } else {
        catalog::open(app, client, catalog::Target::Effort(app.model.clone()));
    }
}

pub(super) fn row_takes_effort(row: &ModelRow) -> bool {
    row.takes_effort() || row.provider.as_deref() == Some("grok")
}

pub(super) async fn apply_picker(app: &mut App, client: &Client) {
    let Some(picker) = app.picker.take() else {
        return;
    };
    match picker {
        Picker::Model {
            rows,
            highlight,
            query,
        } => {
            let Some(row) = filtered_model_indices(&rows, &query)
                .get(highlight)
                .map(|&index| rows[index].clone())
            else {
                app.picker = Some(Picker::Model {
                    rows,
                    highlight,
                    query,
                });
                return;
            };
            apply_model_row(app, client, row).await;
        }
        Picker::Effort { rows, highlight } => {
            let Some(effort) = rows.get(highlight) else {
                return;
            };
            apply_effort(app, client, effort).await;
        }
    }
}

pub(super) async fn post_model(
    app: &mut App,
    client: &Client,
    model: &str,
    effort: Option<&str>,
    provider: Option<&str>,
) -> bool {
    if app.selected.is_empty() {
        if app.server.is_some() {
            app.notice = Some("Create a session before changing its model.".into());
            return false;
        }
        app.poll_revision = app.poll_revision.wrapping_add(1);
        app.model = model.to_string();
        app.effort = effort.map(str::to_string);
        return true;
    }
    let body =
        serde_json::json!({ "model": model, "effort": effort, "provider": provider }).to_string();
    let Ok((status, response)) = client
        .request(
            "POST",
            &format!("/v1/sessions/{}/model", app.selected),
            Some(&body),
        )
        .await
    else {
        return false;
    };
    if status == 204 || status == 200 {
        app.poll_revision = app.poll_revision.wrapping_add(1);
        app.model = model.to_string();
        app.effort = effort.map(str::to_string);
        if let Some(row) = app.sessions.iter_mut().find(|row| row.id == app.selected) {
            row.model = model.to_string();
            row.effort = effort.map(str::to_string);
        }
        true
    } else {
        app.notice = Some(error_text(&response, status));
        false
    }
}

pub(super) async fn apply_model_row(app: &mut App, client: &Client, row: ModelRow) {
    let keep_effort = if app.server.is_some() {
        row.takes_effort()
    } else {
        row_takes_effort(&row)
    };
    let effort = if keep_effort {
        app.effort
            .clone()
            .filter(|effort| app.server.is_none() || row.reasoning_efforts.contains(effort))
    } else {
        None
    };
    if !post_model(
        app,
        client,
        &row.id,
        effort.as_deref(),
        row.provider.as_deref(),
    )
    .await
    {
        app.picker = None;
        return;
    }
    if keep_effort {
        if app.server.is_some() {
            show_effort_picker(app, row.reasoning_efforts);
        } else {
            open_effort_picker(app, client).await;
        }
    } else {
        app.picker = None;
    }
}

pub(super) async fn apply_effort(app: &mut App, client: &Client, effort: &str) {
    match effort_rows(app, client).await {
        Ok(rows) if rows.iter().any(|row| row == effort) => {}
        Ok(_) => {
            app.notice = Some("Reasoning effort is not available for this model.".into());
            return;
        }
        Err(error) => {
            app.notice = Some(error);
            return;
        }
    }
    let model = app.model.clone();
    let _ = post_model(app, client, &model, Some(effort), None).await;
    app.picker = None;
}

pub(super) async fn apply_model_id(app: &mut App, client: &Client, id: &str) {
    if app.server.is_some() {
        let reply = client.request("GET", "/v1/models", None).await;
        match reply {
            Ok((200, body)) => match serde_json::from_str::<Vec<ModelRow>>(&body) {
                Ok(rows) => {
                    if let Some(row) = rows.into_iter().find(|row| row.matches(id)) {
                        apply_model_row(app, client, row).await;
                    } else {
                        app.notice = Some("Model is not available on this server.".into());
                    }
                }
                Err(_) => app.notice = Some("Invalid model list from server.".into()),
            },
            Ok((status, body)) => app.notice = Some(error_text(&body, status)),
            Err(error) => app.notice = Some(error),
        }
        return;
    }
    apply_model_row(
        app,
        client,
        ModelRow {
            id: id.to_string(),
            aliases: Vec::new(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            provider: None,
        },
    )
    .await;
}

pub(super) fn slash_command(ask: &str) -> Option<SlashCommand<'_>> {
    let rest = ask.trim().strip_prefix('/')?;
    let (name, arg) = match rest.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, Some(rest.trim())),
        None => (rest, None),
    };
    match (name, arg) {
        ("model", None | Some("")) => Some(SlashCommand::OpenModel),
        ("model", Some(id)) => Some(SlashCommand::SetModel(id)),
        ("effort", None | Some("")) => Some(SlashCommand::OpenEffort),
        ("effort", Some(effort)) => Some(SlashCommand::SetEffort(effort)),
        ("compact", None | Some("")) => Some(SlashCommand::Compact),
        ("yolo", None | Some("")) => Some(SlashCommand::Yolo(None)),
        ("yolo", Some("on")) => Some(SlashCommand::Yolo(Some(true))),
        ("yolo", Some("off")) => Some(SlashCommand::Yolo(Some(false))),
        ("enhance", None | Some("")) => Some(SlashCommand::Enhance(None)),
        ("enhance", Some("on")) => Some(SlashCommand::Enhance(Some(true))),
        ("enhance", Some("off")) => Some(SlashCommand::Enhance(Some(false))),
        ("proof" | "artifacts", None | Some("")) => {
            Some(SlashCommand::TogglePane(RightPane::Proof))
        }
        ("todos", None | Some("")) => Some(SlashCommand::TogglePane(RightPane::Todos)),
        ("tasks", None | Some("")) => Some(SlashCommand::TogglePane(RightPane::Tasks)),
        ("schedules", None | Some("")) => Some(SlashCommand::TogglePane(RightPane::Schedules)),
        ("closeout", None | Some("")) => Some(SlashCommand::TogglePane(RightPane::Closeout)),
        ("closeout", Some("on")) => Some(SlashCommand::ShowCloseout(true)),
        ("closeout", Some("off")) => Some(SlashCommand::ShowCloseout(false)),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SlashCommand<'a> {
    OpenModel,
    SetModel(&'a str),
    OpenEffort,
    SetEffort(&'a str),
    Compact,
    Yolo(Option<bool>),
    ShowCloseout(bool),
    Enhance(Option<bool>),
    TogglePane(RightPane),
}

pub(super) fn run_pane_command(app: &mut App, pane: RightPane) {
    app.ask.clear();
    app.pastes.retain(|paste| paste.question);
    toggle_pane(app, pane);
}

fn pane_from_slash(ask: &str) -> Option<RightPane> {
    match slash_command(ask) {
        Some(SlashCommand::TogglePane(pane)) => Some(pane),
        _ => None,
    }
}

pub(super) async fn apply_enhance_slash(
    app: &mut App,
    client: &Client,
    flag: Option<bool>,
) -> Result<(), String> {
    let next = flag.unwrap_or(!app.enhance);
    app.ask.clear();
    app.pastes.retain(|paste| paste.question);
    post_enhance(app, client, next).await?;
    poll(app, client).await?;
    Ok(())
}

pub(super) async fn cancel_turn(app: &mut App, client: &Client) -> Result<(), String> {
    let restored = app.enhance_source.clone();
    if !app.selected.is_empty() {
        let _ = client
            .request(
                "POST",
                &format!("/v1/sessions/{}/cancel", app.selected),
                None,
            )
            .await?;
        poll(app, client).await?;
    }
    if let Some(text) = restored {
        if app
            .cards
            .iter()
            .any(|card| matches!(card, Card::Enhance { .. }))
        {
            app.ask.clear();
            app.pastes.retain(|paste| paste.question);
        } else {
            app.ask = text;
            app.overlay = false;
        }
        app.enhance_source = None;
    }
    Ok(())
}

pub(super) async fn apply_yolo_slash(
    app: &mut App,
    client: &Client,
    flag: Option<bool>,
) -> Result<(), String> {
    let next = flag.unwrap_or(!app.yolo);
    app.ask.clear();
    app.pastes.retain(|paste| paste.question);
    post_yolo(app, client, next).await?;
    poll(app, client).await?;
    Ok(())
}

pub(super) async fn apply_closeout_slash(
    app: &mut App,
    client: &Client,
    show: bool,
) -> Result<(), String> {
    app.ask.clear();
    app.pastes.retain(|paste| paste.question);
    post_show_closeout(app, client, show).await?;
    poll(app, client).await?;
    Ok(())
}

pub(super) async fn submit(app: &mut App, client: &Client) -> Result<(), String> {
    let question = matches!(mode(app), Mode::QuestionText | Mode::Question { .. });
    let input = if question {
        &app.question_text
    } else {
        &app.ask
    };
    if input.trim() == "/server" {
        if question {
            app.question_text.clear();
        } else {
            app.ask.clear();
        }
        connections::open_server_picker(app);
        return Ok(());
    }
    if input
        .trim()
        .strip_prefix("/goal")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        if app.selected.is_empty() {
            return Ok(());
        }
        let body = serde_json::json!({ "text": input, "enhance": false }).to_string();
        let (status, response) = client
            .request(
                "POST",
                &format!("/v1/sessions/{}/messages", app.selected),
                Some(&body),
            )
            .await?;
        if status >= 300 {
            return Err(error_text(&response, status));
        }
        app.ask.clear();
        app.question_text.clear();
        poll(app, client).await?;
        return Ok(());
    }
    match mode(app) {
        Mode::Idle => {
            if app.selected.is_empty() {
                return Ok(());
            }
            let ask = app.ask.clone();
            match slash_command(&ask) {
                Some(SlashCommand::OpenModel) => {
                    app.ask.clear();
                    app.pastes.retain(|paste| paste.question);
                    open_model_picker(app, client).await;
                    return Ok(());
                }
                Some(SlashCommand::SetModel(id)) => {
                    app.ask.clear();
                    app.pastes.retain(|paste| paste.question);
                    apply_model_id(app, client, id).await;
                    return Ok(());
                }
                Some(SlashCommand::SetEffort(effort)) => {
                    app.ask.clear();
                    app.pastes.retain(|paste| paste.question);
                    apply_effort(app, client, effort).await;
                    return Ok(());
                }
                Some(SlashCommand::OpenEffort) => {
                    app.ask.clear();
                    app.pastes.retain(|paste| paste.question);
                    open_effort_picker(app, client).await;
                    return Ok(());
                }
                Some(SlashCommand::Compact) => {
                    app.ask.clear();
                    app.pastes.retain(|paste| paste.question);
                    let _ = client
                        .request(
                            "POST",
                            &format!("/v1/sessions/{}/compact", app.selected),
                            None,
                        )
                        .await?;
                    poll(app, client).await?;
                    return Ok(());
                }
                Some(SlashCommand::Yolo(flag)) => {
                    apply_yolo_slash(app, client, flag).await?;
                    return Ok(());
                }
                Some(SlashCommand::ShowCloseout(show)) => {
                    apply_closeout_slash(app, client, show).await?;
                    return Ok(());
                }
                Some(SlashCommand::Enhance(flag)) => {
                    apply_enhance_slash(app, client, flag).await?;
                    return Ok(());
                }
                Some(SlashCommand::TogglePane(pane)) => {
                    run_pane_command(app, pane);
                    return Ok(());
                }
                None => {}
            }
            if app.ask.trim().is_empty() && images::pending(app).is_empty() {
                if app
                    .selected_session()
                    .and_then(|row| row.pull_url.as_ref())
                    .is_some()
                {
                    app.overlay_pull = true;
                    app.overlay = true;
                    return Ok(());
                }
                if overlay_from(&app.cards, &app.question_text).is_some() {
                    app.overlay_pull = false;
                    app.overlay = true;
                }
                return Ok(());
            }
            let sent = app.ask.clone();
            let body =
                serde_json::json!({ "text": app.ask, "images": images::pending(app) }).to_string();
            let (status, response) = client
                .request(
                    "POST",
                    &format!("/v1/sessions/{}/messages", app.selected),
                    Some(&body),
                )
                .await?;
            finish_send(app, &sent, status, &response, true);
            poll(app, client).await?;
        }
        Mode::Working => {
            if let Some(pane) = pane_from_slash(&app.ask) {
                run_pane_command(app, pane);
                return Ok(());
            }
            if let Some(SlashCommand::Yolo(flag)) = slash_command(&app.ask) {
                apply_yolo_slash(app, client, flag).await?;
                return Ok(());
            }
            if let Some(SlashCommand::ShowCloseout(show)) = slash_command(&app.ask) {
                apply_closeout_slash(app, client, show).await?;
                return Ok(());
            }
            if let Some(SlashCommand::Enhance(flag)) = slash_command(&app.ask) {
                apply_enhance_slash(app, client, flag).await?;
                return Ok(());
            }
            if app.selected.is_empty()
                || (app.ask.trim().is_empty() && images::pending(app).is_empty())
            {
                return Ok(());
            }
            let sent = app.ask.clone();
            let body =
                serde_json::json!({ "text": app.ask, "images": images::pending(app) }).to_string();
            let (status, response) = client
                .request(
                    "POST",
                    &format!("/v1/sessions/{}/messages", app.selected),
                    Some(&body),
                )
                .await?;
            finish_send(app, &sent, status, &response, false);
            poll(app, client).await?;
        }
        Mode::Enhance { .. } => {
            if app.selected.is_empty() || app.ask.trim().is_empty() {
                return Ok(());
            }
            let text = app.ask.clone();
            answer_enhance(app, client, "revise", Some(&text)).await?;
        }
        Mode::QuestionText | Mode::Question { .. } => {
            if app.selected.is_empty() || app.question_text.trim().is_empty() {
                return Ok(());
            }
            let Some(event_id) = app.question_id.clone() else {
                return Ok(());
            };
            let body =
                serde_json::json!({ "id": event_id, "choice": app.question_text }).to_string();
            let (status, _) = client
                .request(
                    "POST",
                    &format!("/v1/sessions/{}/answers", app.selected),
                    Some(&body),
                )
                .await?;
            if status == 204 {
                app.question_text.clear();
                app.pastes.retain(|paste| !paste.question);
                app.overlay = false;
                app.open_image = None;
            }
            poll(app, client).await?;
        }
        Mode::Permission => {
            if let Some(pane) = pane_from_slash(&app.ask) {
                run_pane_command(app, pane);
                return Ok(());
            }
            if let Some(SlashCommand::Yolo(flag)) = slash_command(&app.ask) {
                apply_yolo_slash(app, client, flag).await?;
            }
            if let Some(SlashCommand::ShowCloseout(show)) = slash_command(&app.ask) {
                apply_closeout_slash(app, client, show).await?;
            }
            if let Some(SlashCommand::Enhance(flag)) = slash_command(&app.ask) {
                apply_enhance_slash(app, client, flag).await?;
            }
        }
    }
    Ok(())
}

fn sent_message(status: u16, response: &str) -> Result<bool, String> {
    if status == 202 {
        return Ok(false);
    }
    if status == 200 {
        if let Ok(json) = serde_json::from_str::<Value>(response) {
            if json["state"].as_str() == Some("enhancing") {
                return Ok(true);
            }
        }
    }
    Err(error_text(response, status))
}

fn finish_send(app: &mut App, sent: &str, status: u16, response: &str, clear_overlay: bool) {
    match sent_message(status, response) {
        Ok(false) => {
            if serde_json::from_str::<Value>(response)
                .ok()
                .is_some_and(|body| body.get("turnId").is_some() || body["queued"] == true)
            {
                app.images.remove(&app.selected);
            }
            app.ask.clear();
            app.pastes.retain(|paste| paste.question);
            app.notice = None;
            app.enhance_source = None;
            if clear_overlay {
                app.overlay = false;
            }
        }
        Ok(true) => {
            app.images.remove(&app.selected);
            app.enhance_source = Some(sent.to_string());
            app.ask.clear();
            app.pastes.retain(|paste| paste.question);
            app.notice = None;
            app.overlay = false;
        }
        Err(text) => {
            app.notice = Some(text);
        }
    }
}

pub(super) fn enhance_draft(app: &App) -> Option<(String, String)> {
    app.cards.iter().rev().find_map(|card| match card {
        Card::Enhance { text, source, .. } if card.is_waiting() => {
            Some((text.clone(), source.clone()))
        }
        _ => None,
    })
}

pub(super) fn enhance_event_id(app: &App) -> Option<String> {
    app.cards.iter().rev().find_map(|card| match card {
        Card::Enhance { event_id, .. } if card.is_waiting() => Some(event_id.clone()),
        _ => None,
    })
}

pub(super) async fn answer_enhance(
    app: &mut App,
    client: &Client,
    choice: &str,
    text: Option<&str>,
) -> Result<(), String> {
    if app.selected.is_empty() {
        return Ok(());
    }
    let Some(event_id) = enhance_event_id(app) else {
        return Ok(());
    };
    let body = serde_json::json!({
        "id": event_id,
        "choice": choice,
        "text": text.unwrap_or(""),
    })
    .to_string();
    let (status, response) = client
        .request(
            "POST",
            &format!("/v1/sessions/{}/answers", app.selected),
            Some(&body),
        )
        .await?;
    if status == 204 {
        app.ask.clear();
        app.pastes.retain(|paste| paste.question);
        app.overlay = false;
        app.enhance_source = None;
        app.notice = None;
    } else {
        app.notice = Some(error_text(&response, status));
    }
    poll(app, client).await?;
    Ok(())
}

pub(super) async fn answer_permission(
    app: &mut App,
    client: &Client,
    choice: &str,
) -> Result<(), String> {
    if app.selected.is_empty() {
        return Ok(());
    }
    let Some(event_id) = open_event_id_of(client, &app.selected, "permission").await? else {
        return Ok(());
    };
    let body = serde_json::json!({ "id": event_id, "choice": choice }).to_string();
    let _ = client
        .request(
            "POST",
            &format!("/v1/sessions/{}/answers", app.selected),
            Some(&body),
        )
        .await?;
    app.overlay = false;
    poll(app, client).await?;
    Ok(())
}

pub(super) async fn answer_choice(
    app: &mut App,
    client: &Client,
    index: usize,
) -> Result<(), String> {
    let Some(label) = open_question_choices(app).get(index).cloned() else {
        return Ok(());
    };
    if app.selected.is_empty() {
        return Ok(());
    }
    let Some(event_id) = app.question_id.clone() else {
        return Ok(());
    };
    let body = serde_json::json!({ "id": event_id, "choice": label }).to_string();
    let (status, response) = client
        .request(
            "POST",
            &format!("/v1/sessions/{}/answers", app.selected),
            Some(&body),
        )
        .await?;
    if status == 204 {
        app.overlay = false;
        app.open_image = None;
        app.question_text.clear();
        app.pastes.retain(|paste| !paste.question);
    } else {
        app.notice = Some(response);
    }
    poll(app, client).await?;
    Ok(())
}

pub(super) fn clear_for_workspace(app: &mut App) {
    app.command_ui = None;
    app.thinking_open = false;
    app.picker = None;
    app.overlay_pull = false;
    app.open_todo = None;
    app.open_file = None;
    app.open_text = None;
    app.profile_step = None;
    app.overlay = true;
}

pub(super) fn open_workspace_prompt(app: &mut App) {
    clear_for_workspace(app);
    app.workspace_step = WorkspaceStep::Worktree(app.workspace.clone());
}

pub(super) fn open_project_prompt(app: &mut App, projects: Vec<ListedProject>) {
    clear_for_workspace(app);
    app.workspace_step = WorkspaceStep::Projects(projects);
}

pub(super) async fn begin_new_session(app: &mut App, client: &Client) -> Result<(), String> {
    release_command_surfaces(app);
    catalog::open(app, client, catalog::Target::Projects);
    Ok(())
}

pub(super) fn workspace_choice(app: &mut App, index: usize) -> Option<(PathBuf, bool)> {
    match app.workspace_step.clone() {
        WorkspaceStep::Projects(projects) => {
            let offset = usize::from(app.server.is_none());
            if index == projects.len() + offset {
                projects::add(app, true);
                return None;
            }
            let workspace = if offset == 1 && index == 0 {
                app.workspace.clone()
            } else {
                let project = projects.get(index.checked_sub(offset)?)?;
                PathBuf::from(&project.path)
            };
            app.workspace_step = WorkspaceStep::Worktree(workspace);
            app.overlay = true;
            None
        }
        WorkspaceStep::AllProjects(projects) => {
            let offset = usize::from(app.server.is_none());
            if offset == 1 && index == 0 {
                app.workspace_step = WorkspaceStep::Worktree(app.workspace.clone());
                return None;
            }
            if index == projects.len() + offset {
                projects::add(app, true);
            } else if let Some(project) = projects.get(index.saturating_sub(offset)) {
                app.workspace_target =
                    Some((project.server.clone(), PathBuf::from(&project.row.path)));
            }
            None
        }
        WorkspaceStep::Worktree(workspace) => {
            let worktree = match index {
                0 => false,
                1 => true,
                _ => return None,
            };
            Some((workspace, worktree))
        }
        WorkspaceStep::Off => None,
    }
}

pub(super) async fn finish_workspace_prompt(
    app: &mut App,
    client: &Client,
    index: usize,
) -> Result<(), String> {
    let Some((workspace, worktree)) = workspace_choice(app, index) else {
        return Ok(());
    };
    ask_profile(
        app,
        client,
        Some(PendingSession {
            workspace,
            worktree,
        }),
    )
    .await
}

pub(super) fn show_profile_prompt(
    app: &mut App,
    names: Vec<String>,
    pending: Option<PendingSession>,
) {
    clear_for_workspace(app);
    app.workspace_step = WorkspaceStep::Off;
    app.profile_step = Some(ProfileStep { names, pending });
}

pub(super) async fn ask_profile(
    app: &mut App,
    client: &Client,
    pending: Option<PendingSession>,
) -> Result<(), String> {
    catalog::open(app, client, catalog::Target::Profiles(pending));
    Ok(())
}

pub(super) async fn open_profile_picker(app: &mut App, client: &Client) {
    if app.selected.is_empty() {
        return;
    }
    let _ = ask_profile(app, client, None).await;
}

pub(super) fn profile_choice(names: &[String], index: usize) -> Option<Option<String>> {
    if index == 0 {
        return Some(Some(String::new()));
    }
    names.get(index - 1).cloned().map(Some)
}

pub(super) async fn finish_profile_prompt(
    app: &mut App,
    client: &Client,
    index: usize,
) -> Result<(), String> {
    let Some(step) = app.profile_step.clone() else {
        return Ok(());
    };
    let Some(profile) = profile_choice(&step.names, index) else {
        return Ok(());
    };
    match step.pending {
        Some(pending) => {
            create_session(
                app,
                client,
                &pending.workspace,
                pending.worktree,
                profile.as_deref(),
            )
            .await
        }
        None => post_profile(app, client, profile.as_deref()).await,
    }
}

pub(super) async fn post_profile(
    app: &mut App,
    client: &Client,
    profile: Option<&str>,
) -> Result<(), String> {
    let name = profile.unwrap_or("");
    let body = serde_json::json!({ "profile": name }).to_string();
    if app.selected.is_empty() {
        close_overlay(app);
        return Ok(());
    }
    let (status, response) = client
        .request(
            "POST",
            &format!("/v1/sessions/{}/profile", app.selected),
            Some(&body),
        )
        .await?;
    close_overlay(app);
    if status != 204 && status != 200 {
        app.notice = Some(error_text(&response, status));
    }
    poll(app, client).await?;
    Ok(())
}

pub(super) fn create_body(workspace: &str, worktree: bool, profile: Option<&str>) -> Value {
    let mut body = serde_json::json!({
        "workspace": workspace,
        "worktree": worktree,
    });
    if let Some(name) = profile {
        body["profile"] = Value::String(name.to_string());
    }
    body
}

pub(super) async fn create_session(
    app: &mut App,
    client: &Client,
    workspace: &Path,
    worktree: bool,
    profile: Option<&str>,
) -> Result<(), String> {
    let workspace = if app.server.is_some() {
        workspace.to_path_buf()
    } else {
        std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf())
    };
    let body = create_body(&workspace.display().to_string(), worktree, profile).to_string();
    let (status, response) = client.request("POST", "/v1/sessions", Some(&body)).await?;
    close_overlay(app);
    if status == 201 {
        let json: Value = serde_json::from_str(&response).map_err(|source| source.to_string())?;
        if let Some(id) = json["id"].as_str() {
            select_session(app, id.to_string());
            app.ask.clear();
            app.pastes.retain(|paste| paste.question);
        }
    } else {
        app.notice = Some(error_text(&response, status));
    }
    poll(app, client).await?;
    Ok(())
}

pub(super) async fn open_event_id_of(
    client: &Client,
    session_id: &str,
    kind: &str,
) -> Result<Option<String>, String> {
    let (status, body) = client
        .request("GET", &format!("/v1/sessions/{session_id}/events"), None)
        .await?;
    if status != 200 {
        return Ok(None);
    }
    Ok(open_event_id(&body, kind))
}

pub(super) fn open_event_id(log: &str, kind: &str) -> Option<String> {
    let (open_kind, close_kind) = match kind {
        "permission" => ("permission", "permission_answer"),
        "question" => ("question", "question_answer"),
        _ => return None,
    };
    let mut open = None;
    for line in log.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event["kind"].as_str() {
            Some(name) if name == open_kind => {
                open = event["id"].as_str().map(str::to_string);
            }
            Some(name) if name == close_kind => open = None,
            _ => {}
        }
    }
    open
}

fn default_show() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListRow {
    id: String,
    workspace: String,
    status: Status,
    #[serde(default)]
    waiting: Option<String>,
    #[serde(default)]
    pull_url: Option<String>,
    #[serde(default)]
    compacting: bool,
    #[serde(default = "default_show")]
    show_closeout: bool,
    #[serde(default)]
    yolo: bool,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    enhance: bool,
    #[serde(default)]
    model: String,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    isolation: Option<String>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    worktree: bool,
    #[serde(default)]
    archived: bool,
}

pub(super) fn into_row(row: ListRow) -> SessionRow {
    SessionRow {
        id: row.id,
        workspace: PathBuf::from(row.workspace),
        title: row.title,
        status: row.status,
        waiting: match row.waiting.as_deref() {
            Some("permission") => Some(Wait::Permission),
            Some("question") => Some(Wait::Question),
            Some("enhance") => Some(Wait::Enhance),
            _ => None,
        },
        pull_url: row.pull_url,
        compacting: row.compacting,
        yolo: row.yolo,
        show_closeout: row.show_closeout,
        profile: row.profile,
        enhance: row.enhance,
        model: row.model,
        effort: row.effort,
        project: row.project,
        project_name: None,
        parent_id: row.parent_id,
        isolation: row.isolation,
        hidden: row.hidden,
        worktree: row.worktree,
        archived: row.archived,
    }
}

pub(super) async fn listed_projects(client: &Client) -> Vec<ListedProject> {
    if let Some(cached) = client.projects.lock().unwrap().as_ref() {
        if cached.refreshed.elapsed() < Duration::from_secs(2) {
            return cached.rows.clone();
        }
    }
    let Ok((200, body)) = client.request("GET", "/v1/projects", None).await else {
        return Vec::new();
    };
    let Ok(rows) = serde_json::from_str::<Vec<ListedProject>>(&body) else {
        return Vec::new();
    };
    *client.projects.lock().unwrap() = Some(CachedProjects {
        refreshed: Instant::now(),
        rows: rows.clone(),
    });
    rows
}

pub(super) fn to_closeout_check(row: &view::CloseoutRow) -> screen::CloseoutCheck {
    screen::CloseoutCheck {
        runs: row.runs.clone(),
        id: row.id.clone(),
        kind: row.kind.clone(),
        required: row.required,
        status: match row.status {
            view::CloseoutStatus::NotRequired => screen::CloseoutMark::NotRequired,
            view::CloseoutStatus::Missing => screen::CloseoutMark::Missing,
            view::CloseoutStatus::Running => screen::CloseoutMark::Running,
            view::CloseoutStatus::Passed => screen::CloseoutMark::Passed,
            view::CloseoutStatus::Failed => screen::CloseoutMark::Failed,
        },
        exit: row.exit,
        attempt: row.attempt,
        tail: row.tail.clone(),
    }
}

pub(super) fn to_screen_card(card: &view::Card) -> Option<Card> {
    let screen = match card.kind {
        CardKind::Ask => Card::Ask {
            text: text_field(&card.body, "text"),
            images: serde_json::from_value(card.body["images"].clone()).unwrap_or_default(),
        },
        CardKind::Result => Card::Result {
            text: text_field(&card.body, "text"),
        },
        CardKind::Question => {
            let text = text_field(&card.body, "text");
            let answer = card
                .body
                .get("answer")
                .and_then(Value::as_str)
                .map(str::to_string);
            let choices = if answer.is_some() {
                Vec::new()
            } else {
                string_list(&card.body, "choices")
                    .into_iter()
                    .map(|label| Choice {
                        marked: false,
                        label,
                    })
                    .collect()
            };
            Card::Question {
                text,
                choices,
                answer,
                visuals: serde_json::from_value(card.body["visuals"].clone()).unwrap_or_default(),
            }
        }
        CardKind::Answer => Card::Answer {
            text: text_field(&card.body, "text"),
        },
        CardKind::Permission => Card::Permission {
            action: text_field(&card.body, "action"),
            decision: card
                .body
                .get("decision")
                .and_then(Value::as_str)
                .map(str::to_string),
            diff: string_list(&card.body, "diff"),
            argv: string_list(&card.body, "argv"),
        },
        CardKind::Artifact => Card::Artifact {
            file: serde_json::from_value(card.body.get("file")?.clone()).ok()?,
            caption: card
                .body
                .get("caption")
                .and_then(Value::as_str)
                .map(str::to_string),
            focused: false,
        },
        CardKind::Proof => Card::Proof {
            text: text_field(&card.body, "text"),
            items: proof_items(&card.body),
        },
        CardKind::Enhance => Card::Enhance {
            source: text_field(&card.body, "source"),
            text: text_field(&card.body, "text"),
            error: card
                .body
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
            event_id: text_field(&card.body, "eventId"),
        },
    };
    Some(screen)
}

pub(super) fn proof_items(body: &Value) -> Vec<ItemRun> {
    body.get("items")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(proof_item).collect())
        .unwrap_or_default()
}

pub(super) fn proof_item(value: &Value) -> Option<ItemRun> {
    Some(ItemRun {
        id: value.get("id").and_then(Value::as_str)?.to_string(),
        kind: ItemKind::from_label(value.get("kind").and_then(Value::as_str)?)?,
        outcome: Outcome::from_label(value.get("outcome").and_then(Value::as_str)?)?,
        argv: string_list(value, "argv"),
        exit: value.get("exit").and_then(Value::as_i64).map(|n| n as i32),
        tail: text_field(value, "tail"),
    })
}

pub(super) fn text_field(body: &Value, key: &str) -> String {
    body.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn string_list(body: &Value, key: &str) -> Vec<String> {
    body.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn newest_for_server(
    sessions: &[SessionRow],
    workspace: &Path,
    remote: bool,
) -> Option<String> {
    if remote {
        return sessions
            .iter()
            .find(|row| !row.archived && row.workspace == workspace)
            .map(|row| row.id.clone());
    }
    newest_for(sessions, workspace)
}

pub(super) fn newest_for(sessions: &[SessionRow], workspace: &Path) -> Option<String> {
    let current = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    sessions.iter().filter(|row| !row.archived).find_map(|row| {
        let path = PathBuf::from(&row.workspace);
        let same = path == current
            || std::fs::canonicalize(&path)
                .map(|path| path == current)
                .unwrap_or(false);
        same.then(|| row.id.clone())
    })
}

pub(super) fn current_workspace() -> Result<PathBuf, String> {
    let dir = std::env::current_dir().map_err(|source| source.to_string())?;
    Ok(std::fs::canonicalize(&dir).unwrap_or(dir))
}

pub(super) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub(super) fn error_text(body: &str, status: u16) -> String {
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    match json["error"].as_str() {
        Some(error) if !error.is_empty() => error.to_string(),
        _ => format!("the server answered {status}"),
    }
}

pub(super) fn encode_query_path(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub(super) async fn open_file_overlay(
    app: &mut App,
    client: &Client,
    path: &str,
) -> Result<(), String> {
    if app.selected.is_empty() {
        return Ok(());
    }
    let encoded = encode_query_path(path);
    let (status, body) = client
        .request(
            "GET",
            &format!("/v1/sessions/{}/file?path={encoded}", app.selected),
            None,
        )
        .await?;
    if status != 200 {
        app.notice = Some(error_text(&body, status));
        return Ok(());
    }
    let reply: FileReply = match serde_json::from_str(&body) {
        Ok(reply) => reply,
        Err(_) => {
            app.notice = Some("the file is not text".to_string());
            return Ok(());
        }
    };
    app.file_scroll = 0;
    app.select = None;
    app.open_text = None;
    app.open_file = Some(FileView {
        path: reply.path,
        text: reply.text,
        truncated: reply.truncated,
    });
    app.thinking_open = false;
    app.overlay = true;
    app.notice = None;
    Ok(())
}

pub(super) fn open_tasks_overlay(app: &mut App) {
    toggle_pane(app, RightPane::Tasks);
}

pub(super) fn open_schedules_overlay(app: &mut App) {
    toggle_pane(app, RightPane::Schedules);
}

pub(super) async fn fetch_task(app: &App, client: &Client, id: &str) -> Result<TaskDetail, String> {
    fetch_task_for(client, &app.selected, id).await
}

pub(super) async fn fetch_task_for(
    client: &Client,
    selected: &str,
    id: &str,
) -> Result<TaskDetail, String> {
    if selected.is_empty() {
        return Err("no session".to_string());
    }
    let (status, body) = client
        .request("GET", &format!("/v1/sessions/{selected}/tasks/{id}"), None)
        .await?;
    if status != 200 {
        return Err(error_text(&body, status));
    }
    let reply: TaskReply = serde_json::from_str(&body).map_err(|_| "the task could not be read")?;
    let state = match reply.exit {
        Some(code) => format!("{} {code}", reply.state),
        None => reply.state,
    };
    Ok(TaskDetail {
        id: reply.id,
        argv: reply.argv.join(" "),
        state,
        tail: reply.tail,
    })
}

pub(super) async fn open_task_overlay(
    app: &mut App,
    client: &Client,
    id: &str,
) -> Result<(), String> {
    if app
        .task_detail
        .as_ref()
        .is_some_and(|detail| detail.id == id)
    {
        app.task_detail = None;
        return Ok(());
    }
    match fetch_task(app, client, id).await {
        Ok(detail) => {
            app.task_detail = Some(detail);
            app.right_panes.insert(RightPane::Tasks);
            show_column(app);
            app.notice = None;
        }
        Err(text) => {
            if text != "no session" {
                app.notice = Some(text);
            }
        }
    }
    Ok(())
}

pub(super) struct OpenUrlState {
    pub(super) stubbed: bool,
    pub(super) last: Option<String>,
    pub(super) fail: bool,
    pub(super) missing: bool,
    pub(super) browser: Option<String>,
    pub(super) macos: bool,
}

pub(super) static OPEN_URL: Mutex<OpenUrlState> = Mutex::new(OpenUrlState {
    stubbed: false,
    last: None,
    fail: false,
    missing: false,
    browser: None,
    macos: false,
});

pub(super) static OPEN_URL_GATE: Mutex<()> = Mutex::new(());

pub struct OpenUrlCapture {
    _gate: MutexGuard<'static, ()>,
}

impl Drop for OpenUrlCapture {
    fn drop(&mut self) {
        if let Ok(mut slot) = OPEN_URL.lock() {
            *slot = OpenUrlState {
                stubbed: false,
                last: None,
                fail: false,
                missing: false,
                browser: None,
                macos: false,
            };
        }
    }
}

pub fn capture_open_url() -> OpenUrlCapture {
    let gate = OPEN_URL_GATE.lock().expect("open url gate");
    if let Ok(mut slot) = OPEN_URL.lock() {
        *slot = OpenUrlState {
            stubbed: true,
            last: None,
            fail: false,
            missing: false,
            browser: None,
            macos: false,
        };
    }
    OpenUrlCapture { _gate: gate }
}

pub fn last_opened_url() -> Option<String> {
    OPEN_URL.lock().ok().and_then(|slot| slot.last.clone())
}

pub(super) struct CopyState {
    pub(super) stubbed: bool,
    pub(super) last: Option<String>,
    pub(super) fail: bool,
}

pub(super) static COPY_TEXT: Mutex<CopyState> = Mutex::new(CopyState {
    stubbed: false,
    last: None,
    fail: false,
});

pub(super) static COPY_GATE: Mutex<()> = Mutex::new(());

pub struct CopyCapture {
    _gate: MutexGuard<'static, ()>,
}

impl Drop for CopyCapture {
    fn drop(&mut self) {
        if let Ok(mut slot) = COPY_TEXT.lock() {
            *slot = CopyState {
                stubbed: false,
                last: None,
                fail: false,
            };
        }
    }
}

pub fn capture_copy() -> CopyCapture {
    let gate = COPY_GATE.lock().expect("copy gate");
    if let Ok(mut slot) = COPY_TEXT.lock() {
        *slot = CopyState {
            stubbed: true,
            last: None,
            fail: false,
        };
    }
    CopyCapture { _gate: gate }
}

pub fn last_copied() -> Option<String> {
    COPY_TEXT.lock().ok().and_then(|slot| slot.last.clone())
}

#[cfg(test)]
pub(super) fn stub_missing_opener(browser: Option<&str>, macos: bool) {
    if let Ok(mut slot) = OPEN_URL.lock() {
        slot.missing = true;
        slot.browser = browser.map(str::to_string);
        slot.macos = macos;
    }
}

#[cfg(test)]
pub(super) fn fail_captured_copy() {
    if let Ok(mut slot) = COPY_TEXT.lock() {
        slot.fail = true;
    }
}

pub(super) fn commit_copy(app: &mut App, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Err(source) = write_clipboard(text) {
        app.notice = Some(format!("could not copy: {source}"));
    }
}

pub(super) fn osc52(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\u{1b}]52;c;{encoded}\u{7}")
}

pub(super) fn write_clipboard(text: &str) -> io::Result<()> {
    let payload = osc52(text);
    let mut slot = COPY_TEXT.lock().expect("copy slot");
    if slot.stubbed {
        if slot.fail {
            return Err(io::Error::other("copy failed"));
        }
        slot.last = Some(payload);
        return Ok(());
    }
    drop(slot);
    let mut out = io::stdout().lock();
    out.write_all(payload.as_bytes())?;
    out.flush()
}

pub(super) struct NoticeState {
    pub(super) stubbed: bool,
    pub(super) bytes: Vec<u8>,
}

pub(super) static NOTICES: Mutex<NoticeState> = Mutex::new(NoticeState {
    stubbed: false,
    bytes: Vec::new(),
});

pub(super) static NOTICE_GATE: Mutex<()> = Mutex::new(());

pub struct NoticeCapture {
    _gate: MutexGuard<'static, ()>,
}

impl Drop for NoticeCapture {
    fn drop(&mut self) {
        if let Ok(mut slot) = NOTICES.lock() {
            *slot = NoticeState {
                stubbed: false,
                bytes: Vec::new(),
            };
        }
    }
}

pub fn capture_notices() -> NoticeCapture {
    let gate = NOTICE_GATE.lock().expect("notice gate");
    if let Ok(mut slot) = NOTICES.lock() {
        *slot = NoticeState {
            stubbed: true,
            bytes: Vec::new(),
        };
    }
    NoticeCapture { _gate: gate }
}

pub fn take_notices() -> String {
    let mut slot = NOTICES.lock().expect("notices");
    let bytes = std::mem::take(&mut slot.bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

pub(super) fn open_link(app: &mut App, url: &str) {
    if let Err(source) = open_url(url) {
        app.notice = Some(format!("could not open {url}: {source}"));
    }
}

pub(super) fn open_url(url: &str) -> io::Result<()> {
    let mut slot = OPEN_URL.lock().expect("open url slot");
    if slot.stubbed {
        if slot.fail {
            return Err(io::Error::other("open failed"));
        }
        if slot.missing {
            let browser = slot.browser.clone();
            let macos = slot.macos;
            drop(slot);
            return open_in_browser(url, browser.as_deref(), macos, |_, _| {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "No such file or directory",
                ))
            });
        }
        slot.last = Some(url.to_string());
        return Ok(());
    }
    drop(slot);
    spawn_browser(url)
}

pub(super) fn open_file(path: &Path) -> io::Result<()> {
    let target = path.to_string_lossy();
    if OPEN_URL.lock().expect("open url slot").stubbed {
        return open_url(&target);
    }
    open_in_browser(&target, None, cfg!(target_os = "macos"), launch_opener)
}

pub(super) struct BrowserOpener {
    pub(super) program: String,
    pub(super) leading: &'static [&'static str],
}

pub(super) fn browser_openers(browser: Option<&str>, macos: bool) -> Vec<BrowserOpener> {
    let mut openers = Vec::new();
    if let Some(program) = browser.filter(|program| !program.is_empty()) {
        openers.push(BrowserOpener {
            program: program.to_string(),
            leading: &[],
        });
    }
    if macos {
        openers.push(BrowserOpener {
            program: "open".to_string(),
            leading: &[],
        });
    }
    openers.push(BrowserOpener {
        program: "xdg-open".to_string(),
        leading: &[],
    });
    openers.push(BrowserOpener {
        program: "gio".to_string(),
        leading: &["open"],
    });
    openers
}

pub(super) fn open_in_browser(
    url: &str,
    browser: Option<&str>,
    macos: bool,
    mut spawn: impl FnMut(&BrowserOpener, &str) -> io::Result<()>,
) -> io::Result<()> {
    let openers = browser_openers(browser, macos);
    let first = openers
        .first()
        .map(|opener| opener.program.clone())
        .expect("an opener");
    let mut blocked = None;
    for opener in &openers {
        match spawn(opener, url) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                blocked = Some(err);
                break;
            }
        }
    }
    match blocked {
        Some(err) => Err(err),
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{first} not found"),
        )),
    }
}

fn browser_override() -> Option<String> {
    std::env::var("BROWSER")
        .ok()
        .filter(|value| !value.is_empty())
}

fn launch_opener(opener: &BrowserOpener, url: &str) -> io::Result<()> {
    Command::new(&opener.program)
        .args(opener.leading)
        .arg(url)
        .spawn()
        .map(|_| ())
}

pub(super) fn spawn_browser(url: &str) -> io::Result<()> {
    open_in_browser(
        url,
        browser_override().as_deref(),
        cfg!(target_os = "macos"),
        launch_opener,
    )
}

pub(super) fn head_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|window| window == b"\r\n\r\n")
}

pub(super) fn content_length(head: &str) -> usize {
    head.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok())
        .unwrap_or(0)
}

pub(super) fn split_response(text: &str) -> (u16, String) {
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text, ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, body.to_string())
}

pub(super) fn scroll_file(app: &mut App, up: bool, step: usize) -> bool {
    if (app.open_file.is_none() && app.open_text.is_none())
        || app.command_ui.is_some()
        || app.picker.is_some()
        || app.thinking_open
        || app.menu.is_some()
        || app.context_open
    {
        return false;
    }
    let model = screen_model(app);
    let max = screen::file_scroll_max(&model, app.area);
    app.file_scroll = if up {
        app.file_scroll.saturating_sub(step)
    } else {
        app.file_scroll.saturating_add(step).min(max)
    };
    app.select = None;
    true
}
fn delete_tail(text: &mut String, line: bool) {
    let start = if line {
        text.rfind('\n').map_or(0, |i| i + 1)
    } else {
        use unicode_segmentation::UnicodeSegmentation;
        let mut words = text.grapheme_indices(true).rev().peekable();
        let mut end = text.len();
        while let Some(&(index, part)) = words.peek() {
            if !part.chars().all(char::is_whitespace) {
                break;
            }
            end = index;
            words.next();
        }
        let category = words.peek().map(|(_, part)| {
            part.chars()
                .next()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
        });
        for (index, part) in words {
            let class = part
                .chars()
                .next()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_');
            if part.chars().all(char::is_whitespace) || Some(class) != category {
                break;
            }
            end = index;
        }
        end
    };
    text.truncate(start);
}
pub(super) fn edit_delete(app: &mut App, effect: Effect) {
    if workspace_open(&app.workspace_step) || matches!(app.command_ui, Some(CommandUi::Help { .. }))
    {
        return;
    }
    let line = effect == Effect::DeleteLine;
    if let Some(CommandUi::Palette {
        query,
        highlight,
        scroll,
    }) = &mut app.command_ui
    {
        delete_tail(query, line);
        *highlight = 0;
        *scroll = 0;
        return;
    }
    if let Some(Picker::Model {
        query, highlight, ..
    }) = &mut app.picker
    {
        delete_tail(query, line);
        *highlight = 0;
        return;
    }
    if app.picker.is_some() || app.open_file.is_some() {
        return;
    }
    match mode(app) {
        Mode::QuestionText | Mode::Question { .. } => {
            if !delete_pasted_tail(&mut app.question_text, &mut app.pastes, true) {
                delete_tail(&mut app.question_text, line);
            }
        }
        Mode::Permission if app.ask.is_empty() => {}
        _ => {
            if !delete_pasted_tail(&mut app.ask, &mut app.pastes, false) {
                delete_tail(&mut app.ask, line);
            }
            after_ask_edit(app);
        }
    }
}

#[cfg(test)]
mod image_send_tests {
    use super::*;

    #[test]
    fn accepted_controls_keep_images_until_an_ask_starts_or_queues() {
        let mut app = App::new(PathBuf::new(), PathBuf::new(), "one".into());
        let image =
            crate::attachment::ImageAttachment::from_bytes("dog.png", crate::splash::PNG).unwrap();
        app.images.insert("one".into(), vec![image.clone()]);
        finish_send(&mut app, "/goal status", 202, "{}", true);
        assert_eq!(images::pending(&app), std::slice::from_ref(&image));
        finish_send(&mut app, "", 202, r#"{"turnId":"t-1"}"#, true);
        assert!(images::pending(&app).is_empty());
        app.images.insert("one".into(), vec![image]);
        finish_send(&mut app, "", 202, r#"{"queued":true}"#, false);
        assert!(images::pending(&app).is_empty());
    }
}
