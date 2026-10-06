use super::*;

pub(super) fn column_model(app: &App) -> ScreenModel {
    let mut model = layout_model(app);
    model.cards = app.cards.clone();
    model.sessions = app.sessions.clone();
    model.selected = app.selected.clone();
    model.phase = app.phase;
    model.action = app.action.clone();
    model
}

pub(super) fn follow_tail(app: &App) -> usize {
    if app.cards.is_empty() {
        return 0;
    }
    let model = column_model(app);
    let inner = screen::session_inner_of(&model, app.area);
    screen::session_tail(&model, inner)
}

pub(super) fn display_scroll(app: &App) -> usize {
    let tail = follow_tail(app);
    if app.follow {
        tail
    } else {
        app.scroll.min(tail)
    }
}

pub(super) fn palette_visible_rows(app: &App) -> usize {
    let pane = screen::split_of(&layout_model(app), app.area).session;
    let height = pane
        .height
        .saturating_sub(2)
        .max(3.min(pane.height))
        .min(pane.height);
    let inner = height.saturating_sub(2);
    usize::from(inner.saturating_sub(1)).max(1)
}

pub(super) fn fit_palette_window(
    highlight: usize,
    scroll: usize,
    len: usize,
    window: usize,
) -> (usize, usize) {
    if len == 0 {
        return (0, 0);
    }
    let highlight = highlight.min(len - 1);
    let window = window.max(1).min(len);
    let max_scroll = len - window;
    let mut scroll = scroll.min(max_scroll);
    if highlight < scroll {
        scroll = highlight;
    } else if highlight >= scroll + window {
        scroll = highlight + 1 - window;
    }
    (highlight, scroll.min(max_scroll))
}

pub(super) fn pane_rows(app: &App) -> usize {
    let inner = screen::session_inner_of(&layout_model(app), app.area);
    usize::from(inner.height)
}

pub(super) fn closeout_page(app: &App) -> usize {
    let Some((column, row)) = app.pointer else {
        return 1;
    };
    let model = screen_model(app);
    let Some(pane) = screen::right_pane_at(&model, app.area, column, row) else {
        return 1;
    };
    screen::pane_rect(&model, app.area, pane)
        .map(|rect| rect.height.saturating_sub(2).max(1))
        .unwrap_or(1)
        .into()
}

pub(super) fn reveal_list(app: &mut App) {
    let model = screen_model(app);
    app.list_scroll = screen::list_scroll_for(&model, app.area, app.list_scroll);
}

pub(super) fn scroll_list(app: &mut App, up: bool, step: usize) {
    let model = screen_model(app);
    let max = screen::list_scroll_max(&model, app.area);
    app.list_scroll = if up {
        app.list_scroll.saturating_sub(step)
    } else {
        app.list_scroll.saturating_add(step).min(max)
    };
}

pub(super) fn scroll_closeout(app: &mut App, up: bool, step: usize) -> bool {
    let Some((column, row)) = app.pointer else {
        return false;
    };
    let model = screen_model(app);
    let Some(pane) = screen::right_pane_at(&model, app.area, column, row) else {
        return false;
    };
    let max = screen::pane_scroll_max(&model, app.area, pane);
    let slot = match pane {
        screen::RightPane::Todos => &mut app.todo_scroll,
        screen::RightPane::Closeout => &mut app.closeout_scroll,
        screen::RightPane::Tasks => &mut app.task_scroll,
        screen::RightPane::Schedules => &mut app.schedule_scroll,
        screen::RightPane::Proof => &mut app.proof_scroll,
    };
    if up {
        *slot = slot.saturating_sub(step);
    } else {
        *slot = slot.saturating_add(step).min(max);
    }
    true
}

pub(super) fn scroll_lines(app: &mut App, up: bool, step: usize) {
    if app.cards.is_empty() {
        app.scroll = 0;
        return;
    }
    let tail = follow_tail(app);
    let start = if app.follow {
        tail
    } else {
        app.scroll.min(tail)
    };
    if up {
        app.follow = false;
        app.scroll = start.saturating_sub(step);
        return;
    }
    let next = start.saturating_add(step);
    if next >= tail {
        app.follow = true;
        app.scroll = tail;
    } else {
        app.follow = false;
        app.scroll = next;
    }
}

pub(super) fn select_session(app: &mut App, id: String) {
    app.queue_open = false;
    app.queue_highlight = None;
    if app.selected != id {
        app.poll_revision = app.poll_revision.wrapping_add(1);
        app.queue.clear();
        app.queue_items.clear();
        app.cards.clear();
        app.card_event_ids.clear();
        app.artifact_focus = None;
        app.todos.clear();
        app.closeout.clear();
        app.tasks.clear();
        app.schedules.clear();
        app.proof_versions.clear();
        app.proof_selected = None;
        app.proof_status = None;
        app.proof_scroll = 0;
        app.proof_popup = None;
        proof_files::close(app);
        app.open_todo = None;
        app.open_check = None;
        app.task_detail = None;
        app.question_id = None;
        app.phase = None;
        app.action = None;
        app.thinking.clear();
        app.thinking_open = false;
    }
    let rows = servers::sessions(app);
    let selected = if app.servers.is_some() {
        servers::key(&app.server, &id)
    } else {
        id.clone()
    };
    if let Some(key) = screen::session_group(&rows, &selected) {
        app.collapsed.remove(&key);
    }
    app.list_header = None;
    app.open_text = None;
    app.open_image = None;
    app.selected = id;
    if let Some(row) = app.selected_session().cloned() {
        app.yolo = row.yolo;
        app.show_closeout = row.show_closeout;
        app.enhance = row.enhance;
        if !row.model.is_empty() {
            app.model = row.model;
        }
        app.effort = row.effort;
    }
    app.scroll = 0;
    app.follow = true;
    reveal_list(app);
    app.notice = None;
    app.retry_status = None;
    app.question_text.clear();
    app.pastes.retain(|paste| !paste.question);
    app.queue.clear();
    app.context = None;
    app.context_open = false;
}

pub(super) fn next_list_id(sessions: &[SessionRow], closed: &str) -> Option<String> {
    let index = sessions.iter().position(|row| row.id == closed)?;
    sessions
        .get(index.saturating_add(1))
        .or_else(|| index.checked_sub(1).and_then(|prev| sessions.get(prev)))
        .map(|row| row.id.clone())
}

pub(super) fn preview_open(app: &App) -> bool {
    app.open_text.is_some() || app.open_image.is_some()
}

pub(super) fn esc_stops(app: &App) -> bool {
    if app.delete_confirm.is_some() || app.menu.is_some() {
        return false;
    }
    if app.queue_open || app.context_open || preview_open(app) {
        return false;
    }
    if app.picker.is_some() || picker_is_open(app) || app.command_ui.is_some() {
        return false;
    }
    if app.thinking_open {
        return true;
    }
    if app.open_file.is_some()
        || app.overlay_pull
        || app.open_todo.is_some()
        || choosing(app)
        || (app.right_open && app.right_width > TODOS_WIDTH)
        || app.overlay
    {
        return false;
    }
    true
}

pub fn screen_model(app: &App) -> ScreenModel {
    let (bottom, bottom_kind) = bottom_line(app);
    let question = matches!(mode(app), Mode::QuestionText | Mode::Question { .. });
    let input = if question {
        &app.question_text
    } else {
        &app.ask
    };
    let pasted_text = app
        .pastes
        .iter()
        .any(|paste| paste.question == question && paste_matches(input, paste))
        .then(|| input.clone());
    let bottom = if bottom_kind == Bottom::Prompt && bottom == *input {
        display_pasted(input, &app.pastes, question)
    } else {
        bottom
    };
    ScreenModel {
        sessions: servers::sessions(app),
        projects: servers::project_rows(app),
        selected: servers::selected(app),
        cards: app.cards.clone(),
        scroll: display_scroll(app),
        bottom,
        toast: app.notice.clone(),
        bottom_kind,
        pasted_text,
        pending_images: if matches!(mode(app), Mode::Idle | Mode::Working) {
            images::pending(app).to_vec()
        } else {
            Vec::new()
        },
        home: app.home.clone(),
        tick: app.tick,
        yolo: app.yolo,
        enhance: app.enhance,
        context_percent: app.context.as_ref().and_then(|usage| usage.percent),
        file_scroll: app.file_scroll,
        overlay: proof_files::overlay(app)
            .or_else(|| proof::overlay(app))
            .or_else(|| projects::overlay(app))
            .or_else(|| catalog::overlay(app))
            .or_else(|| providers::overlay(app))
            .or_else(|| queue::overlay(app))
            .or_else(|| delete_overlay(app))
            .or(menu_overlay(app))
            .or(context_overlay(app))
            .or_else(|| command_overlay(app))
            .or_else(|| {
                if app.thinking_open {
                    Some(Overlay::Thinking {
                        text: app
                            .action
                            .iter()
                            .chain((!app.thinking.is_empty()).then_some(&app.thinking))
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                    })
                } else {
                    picker_overlay(app).or_else(|| {
                        if let Some(image) = &app.open_image {
                            Some(Overlay::Image {
                                image: image.clone(),
                            })
                        } else if let Some(text) = &app.open_text {
                            Some(Overlay::Text { text: text.clone() })
                        } else if let Some(file) = &app.open_file {
                            Some(Overlay::File {
                                path: file.path.clone(),
                                text: file.text.clone(),
                                truncated: file.truncated,
                            })
                        } else if app.overlay {
                            if app.overlay_pull {
                                app.selected_session()
                                    .and_then(|row| row.pull_url.clone())
                                    .map(|url| Overlay::Pull { url })
                            } else if let Some(overlay) = connections::server_overlay(app) {
                                Some(overlay)
                            } else if let Some(overlay) = profile_overlay(&app.profile_step) {
                                Some(overlay)
                            } else if let Some(overlay) = workspace_overlay(
                                &app.workspace_step,
                                app.server.is_some(),
                                &app.server_names,
                            ) {
                                Some(overlay)
                            } else {
                                overlay_from(&app.cards, &app.question_text)
                            }
                        } else {
                            None
                        }
                    })
                }
            }),
        skill_picker: skill_picker_model(app),
        model: app.model.clone(),
        effort: app.effort.clone(),
        compacting: app
            .selected_session()
            .map(|row| row.compacting)
            .unwrap_or(false),
        todos: app.todos.clone(),
        tasks: app.tasks.clone(),
        schedules: app.schedules.clone(),
        phase: app.phase,
        action: app.action.clone(),
        thinking: app.thinking.clone(),
        retry_status: app.retry_status.clone(),
        queue: app.queue.len(),
        left_open: app.left_open,
        right_open: app.right_open,
        left_width: app.left_width,
        right_width: app.right_width,
        right_panes: app.right_panes.clone(),
        open_todo: app.open_todo.clone(),
        todo_scroll: app.todo_scroll,
        closeout: app.closeout.clone(),
        open_check: app.open_check.clone(),
        closeout_scroll: app.closeout_scroll,
        open_task: app.task_detail.as_ref().map(|detail| screen::OpenTask {
            id: detail.id.clone(),
            argv: detail.argv.clone(),
            state: detail.state.clone(),
            tail: detail.tail.clone(),
        }),
        task_scroll: app.task_scroll,
        schedule_scroll: app.schedule_scroll,
        proof_versions: app.proof_versions.clone(),
        proof_selected: app.proof_selected,
        proof_status: app.proof_status.clone(),
        proof_scroll: app.proof_scroll,
        drag: app.drag,
        drag_origin: app.drag_origin,
        select: app.select,
        collapsed: app.collapsed.clone(),
        list_header: app.list_header.clone(),
        list_scroll: app.list_scroll,
        list_filter: app.list_filter,
    }
}

pub(super) fn layout_model(app: &App) -> ScreenModel {
    let (bottom, bottom_kind) = bottom_line(app);
    let question = matches!(mode(app), Mode::QuestionText | Mode::Question { .. });
    let input = if question {
        &app.question_text
    } else {
        &app.ask
    };
    let pasted_text = app
        .pastes
        .iter()
        .any(|paste| paste.question == question && paste_matches(input, paste))
        .then(|| input.clone());
    let bottom = if bottom_kind == Bottom::Prompt && bottom == *input {
        display_pasted(input, &app.pastes, question)
    } else {
        bottom
    };
    ScreenModel {
        sessions: app.sessions.clone(),
        selected: app.selected.clone(),
        bottom,
        bottom_kind,
        pasted_text,
        pending_images: if matches!(mode(app), Mode::Idle | Mode::Working) {
            images::pending(app).to_vec()
        } else {
            Vec::new()
        },
        todos: app.todos.clone(),
        tasks: app.tasks.clone(),
        schedules: app.schedules.clone(),
        left_open: app.left_open,
        right_open: app.right_open,
        left_width: app.left_width,
        right_width: app.right_width,
        right_panes: app.right_panes.clone(),
        skill_picker: skill_picker_model(app),
        closeout: app.closeout.clone(),
        open_check: app.open_check.clone(),
        closeout_scroll: app.closeout_scroll,
        open_task: app.task_detail.as_ref().map(|detail| screen::OpenTask {
            id: detail.id.clone(),
            argv: detail.argv.clone(),
            state: detail.state.clone(),
            tail: detail.tail.clone(),
        }),
        todo_scroll: app.todo_scroll,
        task_scroll: app.task_scroll,
        schedule_scroll: app.schedule_scroll,
        proof_versions: app.proof_versions.clone(),
        proof_selected: app.proof_selected,
        proof_status: app.proof_status.clone(),
        proof_scroll: app.proof_scroll,
        ..ScreenModel::default()
    }
}

pub(super) fn context_overlay(app: &App) -> Option<Overlay> {
    if !app.context_open {
        return None;
    }
    let usage = app.context.as_ref()?;
    let percent = usage.percent?;
    let window = usage.window?;
    Some(Overlay::Context {
        percent,
        used: usage.used,
        reported_prompt_tokens: usage.reported_prompt_tokens,
        window,
        buckets: usage
            .buckets
            .iter()
            .map(|bucket| screen::ContextLine {
                id: bucket.id.as_str().to_string(),
                tokens: bucket.tokens,
            })
            .collect(),
    })
}

pub(super) fn command_overlay(app: &App) -> Option<Overlay> {
    match &app.command_ui {
        Some(CommandUi::Palette {
            query,
            highlight,
            scroll,
        }) => {
            let rows = filtered_commands(&app.skills, query);
            let (highlight, scroll) =
                fit_palette_window(*highlight, *scroll, rows.len(), palette_visible_rows(app));
            Some(Overlay::Palette {
                rows: rows.into_iter().map(|row| row.line).collect(),
                highlight,
                scroll,
                query: query.clone(),
            })
        }
        Some(CommandUi::Help { scroll }) => Some(Overlay::Help {
            rows: command_lines(&app.skills),
            scroll: *scroll,
        }),
        None => None,
    }
}

pub(super) fn picker_overlay(app: &App) -> Option<Overlay> {
    match &app.picker {
        Some(Picker::Model {
            rows,
            highlight,
            query,
        }) => {
            let filtered: Vec<String> = filtered_model_indices(rows, query)
                .into_iter()
                .map(|index| rows[index].label())
                .collect();
            let highlight = if filtered.is_empty() {
                0
            } else {
                (*highlight).min(filtered.len() - 1)
            };
            Some(Overlay::Model {
                rows: filtered,
                highlight,
            })
        }
        Some(Picker::Effort { rows, highlight }) => Some(Overlay::Effort {
            rows: rows.clone(),
            highlight: *highlight,
        }),
        None => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum QueryRank {
    Prefix,
    Substring,
    Subsequence,
}

pub(super) fn query_rank(haystack: &str, query: &str) -> Option<QueryRank> {
    let haystack = haystack.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();
    if haystack.starts_with(&query) {
        Some(QueryRank::Prefix)
    } else if haystack.contains(&query) {
        Some(QueryRank::Substring)
    } else if is_subsequence(&haystack, &query) {
        Some(QueryRank::Subsequence)
    } else {
        None
    }
}

pub(super) fn is_subsequence(haystack: &str, query: &str) -> bool {
    let mut haystack = haystack.chars();
    query.chars().all(|needle| haystack.any(|ch| ch == needle))
}

pub(super) fn model_query_rank(row: &ModelRow, query: &str) -> Option<QueryRank> {
    if query.is_empty() {
        return Some(QueryRank::Prefix);
    }
    std::iter::once(row.id.as_str())
        .chain(row.aliases.iter().map(String::as_str))
        .filter_map(|haystack| query_rank(haystack, query))
        .min()
}

pub(super) fn filtered_model_indices(rows: &[ModelRow], query: &str) -> Vec<usize> {
    let mut ranked: Vec<(usize, QueryRank)> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| model_query_rank(row, query).map(|rank| (index, rank)))
        .collect();
    if !query.is_empty() {
        ranked.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
    }
    ranked.into_iter().map(|(index, _)| index).collect()
}

pub(super) fn edit_open_model_query(app: &mut App, typed: Option<char>) -> bool {
    let Some(Picker::Model {
        rows,
        highlight,
        query,
    }) = &mut app.picker
    else {
        return false;
    };
    let matches = filtered_model_indices(rows, query);
    let old = matches.get(*highlight).copied();
    match typed {
        Some(c) => query.push(c),
        None => {
            query.pop();
        }
    }
    let matches = filtered_model_indices(rows, query);
    *highlight = old
        .and_then(|index| matches.iter().position(|&i| i == index))
        .unwrap_or(0);
    true
}

pub(super) fn skill_picker_model(app: &App) -> Option<SkillPicker> {
    let rows = picker_matches(app);
    if rows.is_empty() {
        return None;
    }
    Some(SkillPicker {
        selected: app.skill_highlight.min(rows.len() - 1),
        rows: rows
            .into_iter()
            .map(|skill| SkillPickerRow {
                name: skill.name.clone(),
                description: skill.description.lines().next().unwrap_or("").to_string(),
            })
            .collect(),
    })
}

pub(super) fn picker_matches(app: &App) -> Vec<&SkillEntry> {
    if app.overlay
        || app.picker.is_some()
        || app.command_ui.is_some()
        || app.skill_picker_closed
        || !matches!(mode(app), Mode::Idle)
    {
        return Vec::new();
    }
    let Some(token) = skills::picker_token(&app.ask) else {
        return Vec::new();
    };
    if token.eq_ignore_ascii_case("model") || token.eq_ignore_ascii_case("effort") {
        return Vec::new();
    }
    skills::matching(&app.skills, token)
}

pub(super) fn overlay_from(cards: &[Card], prompt: &str) -> Option<Overlay> {
    let waiting = cards.iter().rev().find_map(|card| match card {
        Card::Question { text, choices, .. } if card.is_waiting() => Some(Overlay::Question {
            text: text.clone(),
            choices: choices.clone(),
            prompt: prompt.to_string(),
        }),
        Card::Permission {
            action, diff, argv, ..
        } if card.is_waiting() => Some(Overlay::Permission {
            action: action.clone(),
            diff: diff.clone(),
            argv: argv.clone(),
        }),
        Card::Enhance {
            source,
            text,
            error,
            ..
        } if card.is_waiting() => Some(Overlay::Enhance {
            source: source.clone(),
            text: text.clone(),
            error: error.clone(),
        }),
        _ => None,
    });
    if waiting.is_some() {
        return waiting;
    }
    cards.iter().rev().find_map(|card| match card {
        Card::Proof { text, items } if !text.is_empty() => Some(Overlay::Proof {
            text: text.clone(),
            items: items.clone(),
        }),
        _ => None,
    })
}

pub(super) fn waiting_question_id(cards: &[view::Card]) -> Option<String> {
    cards.iter().rev().find_map(|card| {
        if card.kind != CardKind::Question {
            return None;
        }
        let answered = card
            .body
            .get("answer")
            .map(|value| !value.is_null())
            .unwrap_or(false);
        if answered {
            None
        } else {
            Some(
                card.body
                    .get("eventId")
                    .and_then(Value::as_str)
                    .unwrap_or(card.id.as_str())
                    .to_string(),
            )
        }
    })
}

pub(super) fn enhance_is_esc_target(app: &App) -> bool {
    if app.menu.is_some()
        || app.command_ui.is_some()
        || app.picker.is_some()
        || picker_is_open(app)
        || app.thinking_open
        || app.context_open
        || preview_open(app)
        || app.open_file.is_some()
        || app.overlay_pull
        || app.open_todo.is_some()
        || workspace_open(&app.workspace_step)
        || !app.overlay
    {
        return false;
    }
    matches!(
        overlay_from(&app.cards, &app.question_text),
        Some(Overlay::Enhance { .. })
    )
}

pub(super) fn maybe_open_enhance(app: &mut App) {
    if app
        .selected_session()
        .is_some_and(|row| row.status == Status::Working)
    {
        return;
    }
    if app.queue_open
        || preview_open(app)
        || app.context_open
        || app.thinking_open
        || app.overlay
        || app.open_file.is_some()
        || app.command_ui.is_some()
        || app.picker.is_some()
    {
        return;
    }
    if matches!(
        overlay_from(&app.cards, &app.question_text),
        Some(Overlay::Enhance { .. })
    ) {
        app.overlay_pull = false;
        app.open_todo = None;
        app.open_file = None;
        app.open_text = None;
        app.open_image = None;
        app.overlay = true;
    }
}

pub(super) fn maybe_open_question(app: &mut App) {
    if app.queue_open
        || preview_open(app)
        || app.context_open
        || app.thinking_open
        || app.overlay
        || app.open_file.is_some()
        || app.command_ui.is_some()
    {
        return;
    }
    let Some(id) = app.question_id.as_deref() else {
        return;
    };
    if app.dismissed_question.as_deref() == Some(id) {
        return;
    }
    if matches!(
        overlay_from(&app.cards, &app.question_text),
        Some(Overlay::Question { .. })
    ) {
        app.overlay_pull = false;
        app.open_todo = None;
        app.open_file = None;
        app.open_text = None;
        app.open_image = None;
        app.overlay = true;
    }
}

pub(super) fn apply_flight(app: &mut App, phase: Option<screen::Phase>, thinking: Option<String>) {
    app.phase = phase;
    if let Some(text) = thinking {
        app.thinking = text;
    }
}

pub(super) fn open_context_overlay(app: &mut App) {
    if app
        .context
        .as_ref()
        .and_then(|usage| usage.percent)
        .is_none()
    {
        return;
    }
    app.command_ui = None;
    app.picker = None;
    app.thinking_open = false;
    app.open_file = None;
    app.open_text = None;
    app.open_image = None;
    app.overlay_pull = false;
    app.workspace_step = WorkspaceStep::Off;
    app.profile_step = None;
    app.context_open = true;
    app.overlay = true;
}

pub(super) fn open_thinking_overlay(app: &mut App) {
    app.open_file = None;
    app.open_text = None;
    app.open_image = None;
    app.overlay_pull = false;
    app.workspace_step = WorkspaceStep::Off;
    app.profile_step = None;
    app.picker = None;
    app.thinking_open = true;
    app.overlay = true;
}

pub(super) fn delete_overlay(app: &App) -> Option<Overlay> {
    let confirm = app.delete_confirm.as_ref()?;
    let row = app.sessions.iter().find(|row| row.id == confirm.id)?;
    let path = row.worktree.then(|| row.workspace.display().to_string());
    let warning = confirm.error.clone().or_else(|| {
        confirm
            .status
            .as_ref()
            .filter(|status| !status.changes.is_empty())
            .map(|status| {
                format!(
                    "Warning: {} uncommitted, untracked, or ignored paths.\n{}",
                    status.changes.len(),
                    status
                        .changes
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })
    });
    Some(Overlay::Delete {
        path,
        highlight: confirm.highlight,
        remove_workspace: confirm.remove_workspace,
        confirm_dirty: confirm.confirm_dirty,
        warning,
        loading: confirm.busy,
    })
}

pub(super) fn menu_overlay(app: &App) -> Option<Overlay> {
    app.menu.as_ref().map(|menu| Overlay::Menu {
        id: menu.id.clone(),
        items: menu.items.clone(),
        column: menu.column,
        row: menu.row,
    })
}

pub(super) fn close_overlay(app: &mut App) {
    if app.proof_file_popup.is_some() {
        proof_files::close(app);
        return;
    }
    if app.proof_popup.take().is_some() {
        app.overlay = false;
        return;
    }
    if app.project_popup.is_some() {
        app.notice = None;
        projects::close(app);
        return;
    }
    if app.catalog_popup.is_some() {
        catalog::close(app);
        return;
    }
    if app.provider_popup.is_some() {
        providers::close(app);
        app.overlay = false;
        return;
    }
    if app.queue_open {
        app.queue_open = false;
        return;
    }
    if app.server_popup.is_some() {
        connections::close(app);
        app.overlay = false;
        return;
    }
    if app.server_step.take().is_some() {
        app.overlay = false;
        return;
    }
    if app.open_image.take().is_some() {
        app.overlay = false;
        return;
    }
    if app.delete_confirm.is_some() {
        deletion::close(app);
        return;
    }
    if app.menu.take().is_some() {
        return;
    }
    if app.command_ui.take().is_some() {
        return;
    }
    if app.picker.is_some() {
        app.picker = None;
        return;
    }
    if picker_is_open(app) {
        app.skill_picker_closed = true;
    }
    if app.thinking_open {
        app.thinking_open = false;
        app.overlay = false;
        return;
    }
    if app.context_open {
        app.context_open = false;
        app.overlay = false;
        return;
    }
    if app.open_text.take().is_some() {
        app.overlay = false;
        app.file_scroll = 0;
        return;
    }
    if app.open_file.take().is_some() {
        app.overlay = app.overlay_pull
            || choosing(app)
            || overlay_from(&app.cards, &app.question_text).is_some();
        return;
    }
    if app.overlay
        && !app.overlay_pull
        && app.open_todo.is_none()
        && matches!(
            overlay_from(&app.cards, &app.question_text),
            Some(Overlay::Question { .. })
        )
    {
        app.dismissed_question = app.question_id.clone();
    }
    let collapse_todos =
        app.open_todo.is_some() || (app.right_open && app.right_width > TODOS_WIDTH);
    app.overlay = false;
    app.overlay_pull = false;
    app.workspace_step = WorkspaceStep::Off;
    app.profile_step = None;
    app.open_todo = None;
    app.open_file = None;
    app.open_text = None;
    app.open_image = None;
    app.server_step = None;
    if collapse_todos {
        app.right_open = true;
        app.right_width = TODOS_WIDTH;
    }
}

pub(super) fn bottom_line(app: &App) -> (String, Bottom) {
    if let Some(Picker::Model { query, .. }) = &app.picker {
        return (query.clone(), Bottom::Prompt);
    }
    if let Some(CommandUi::Palette { query, .. }) = &app.command_ui {
        return (query.clone(), Bottom::Prompt);
    }
    match mode(app) {
        Mode::Idle => (app.ask.clone(), Bottom::Prompt),
        Mode::Permission if app.ask.starts_with('/') => (app.ask.clone(), Bottom::Prompt),
        Mode::Permission => ("a once   s session   d deny".to_string(), Bottom::Keys),
        Mode::Question { .. } | Mode::QuestionText => (app.question_text.clone(), Bottom::Prompt),
        Mode::Working | Mode::Enhance { .. } => (app.ask.clone(), Bottom::Prompt),
    }
}

pub(super) fn open_question_choices(app: &App) -> Vec<String> {
    app.cards
        .iter()
        .rev()
        .find_map(|card| match card {
            Card::Question { choices, .. } if card.is_waiting() => Some(
                choices
                    .iter()
                    .map(|choice| choice.label.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn paste_matches(input: &str, paste: &PastedInput) -> bool {
    input.get(paste.start..paste.start.saturating_add(paste.text.len()))
        == Some(paste.text.as_str())
}

fn display_pasted(input: &str, pastes: &[PastedInput], question: bool) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    for paste in pastes {
        if paste.question != question || paste.start < cursor || !paste_matches(input, paste) {
            continue;
        }
        out.push_str(&input[cursor..paste.start]);
        out.push_str(&format!(
            "[Pasted input: {} chars]",
            paste.text.chars().count()
        ));
        cursor = paste.start + paste.text.len();
    }
    out.push_str(&input[cursor..]);
    out
}

pub(super) fn delete_pasted_tail(
    input: &mut String,
    pastes: &mut Vec<PastedInput>,
    question: bool,
) -> bool {
    let Some(index) = pastes.iter().position(|paste| {
        paste.question == question
            && paste.start + paste.text.len() == input.len()
            && paste_matches(input, paste)
    }) else {
        return false;
    };
    input.truncate(pastes.remove(index).start);
    true
}

pub(super) fn advance_notice(app: &mut App, now: Instant) {
    let Some(text) = &app.notice else {
        app.notice_seen = None;
        return;
    };
    if let Some((seen, since)) = &app.notice_seen {
        if seen == text {
            if now.saturating_duration_since(*since) >= Duration::from_secs(5) {
                app.notice = None;
                app.notice_seen = None;
            }
            return;
        }
    }
    app.notice_seen = Some((text.clone(), now));
}
