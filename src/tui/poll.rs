use super::*;

#[derive(Clone)]
pub(super) struct PollContext {
    server: Option<String>,
    revision: u64,
    selected: String,
    workspace: PathBuf,
    start_yolo: bool,
    task: Option<String>,
    known_tasks: Vec<String>,
    proof_visible: bool,
}

impl PollContext {
    pub(super) fn new(app: &App) -> Self {
        Self {
            server: app.server.clone(),
            revision: app.poll_revision,
            selected: app.selected.clone(),
            workspace: app.workspace.clone(),
            start_yolo: app.start_yolo,
            task: app.task_detail.as_ref().map(|task| task.id.clone()),
            known_tasks: app.tasks.iter().map(|task| task.id.clone()).collect(),
            proof_visible: app.right_panes.contains(&RightPane::Proof) || app.proof_popup.is_some(),
        }
    }
    pub(super) fn matches(&self, app: &App) -> bool {
        self.revision == app.poll_revision
            && self.server == app.server
            && self.selected == app.selected
            && self.workspace == app.workspace
    }
}

pub(super) struct PollData {
    sessions: Vec<SessionRow>,
    projects: Vec<ListedProject>,
    layout: Option<crate::config::Layout>,
    info: Option<crate::pairing::ServerInfo>,
    view: Option<view::View>,
    task: Option<TaskDetail>,
    yolo_started: bool,
    proof: Option<Result<Vec<crate::proof::ProofVersion>, String>>,
}

pub(super) async fn fetch(context: &PollContext, client: &Client) -> Result<PollData, String> {
    let (_, info, sessions, projects, layout) = tokio::join!(
        client.request("POST", "/v1/tui/heartbeat", None),
        async {
            if context.server.is_some() {
                client.server_info().await.map(Some)
            } else {
                Ok(None)
            }
        },
        client.request("GET", "/v1/sessions", None),
        listed_projects(client),
        async {
            let (status, body) = client.request("GET", "/v1/layout", None).await?;
            match status {
                200 => serde_json::from_str::<Option<crate::config::Layout>>(&body)
                    .map_err(|error| error.to_string()),
                404 => Ok(None),
                _ => Err(error_text(&body, status)),
            }
        },
    );
    let info = info?;
    let workspace = info
        .as_ref()
        .map(|info| Path::new(&info.workspace))
        .unwrap_or(&context.workspace);
    let (status, body) = sessions?;
    if status != 200 {
        return Err(error_text(&body, status));
    }
    let rows: Vec<ListRow> = serde_json::from_str(&body).map_err(|source| source.to_string())?;
    let names: BTreeMap<_, _> = projects.iter().map(|row| (&row.id, &row.name)).collect();
    let mut sessions: Vec<SessionRow> = rows
        .into_iter()
        .map(|row| {
            let mut row = into_row(row);
            if let Some(id) = row.project.clone() {
                if let Some(name) = names.get(&id) {
                    row.project_name = Some((*name).clone());
                }
            }
            row
        })
        .collect();
    let selected = live_selection(
        &sessions,
        &context.selected,
        workspace,
        context.server.is_some(),
    );
    let mut yolo_started = false;
    if context.start_yolo && !selected.is_empty() {
        if !sessions.iter().any(|row| row.id == selected && row.yolo) {
            let body = serde_json::json!({"yolo":true}).to_string();
            let (status, response) = client
                .request(
                    "POST",
                    &format!("/v1/sessions/{selected}/yolo"),
                    Some(&body),
                )
                .await?;
            if status != 200 && status != 204 {
                return Err(error_text(&response, status));
            }
        }
        if let Some(row) = sessions.iter_mut().find(|row| row.id == selected) {
            row.yolo = true;
        }
        yolo_started = true;
    }
    let view = if selected.is_empty() {
        None
    } else {
        let (status, body) = client
            .request("GET", &format!("/v1/sessions/{selected}/view"), None)
            .await?;
        if status == 200 {
            Some(serde_json::from_str::<view::View>(&body).map_err(|error| error.to_string())?)
        } else if context.server.is_some() {
            return Err(error_text(&body, status));
        } else {
            None
        }
    };
    let proof = if context.proof_visible && !selected.is_empty() {
        Some(proof::fetch(client, &selected).await)
    } else {
        None
    };
    let task_id = view
        .as_ref()
        .and_then(|view| {
            view.tasks
                .iter()
                .rev()
                .find(|task| !context.known_tasks.contains(&task.id))
                .map(|task| task.id.clone())
        })
        .or_else(|| context.task.clone());
    let task = if let Some(id) = task_id {
        fetch_task_for(client, &selected, &id).await.ok()
    } else {
        None
    };
    Ok(PollData {
        layout: layout?,
        sessions,
        projects,
        info,
        view,
        task,
        yolo_started,
        proof,
    })
}

pub(super) fn apply_poll(app: &mut App, data: PollData) {
    layout::apply_saved(app, data.layout);
    let question_overlay = app.overlay
        && app.question_id.is_some()
        && !choosing(app)
        && matches!(screen_model(app).overlay, Some(Overlay::Question { .. }));
    app.poll_revision = app.poll_revision.wrapping_add(1);
    if let Some(info) = data.info {
        app.workspace = PathBuf::from(&info.workspace);
        app.remote_defaults = Some(info);
    }
    app.project_rows = data.projects;
    app.sessions = data.sessions;
    if data.yolo_started {
        app.start_yolo = false;
        if let Some(row) = app.sessions.iter_mut().find(|row| row.id == app.selected) {
            row.yolo = true;
        }
    }
    if app
        .menu
        .as_ref()
        .is_some_and(|menu| app.sessions.iter().all(|row| row.id != menu.id))
    {
        app.menu = None;
    }
    if !app.sessions.iter().any(|row| row.id == app.selected) {
        let id = live_selection(&app.sessions, "", &app.workspace, app.server.is_some());
        if id != app.selected {
            select_session(app, id);
        }
    }
    if let Some(proof) = data.proof {
        proof::reconcile(app, proof);
    }
    remember_sessions(app);
    if let Some(row) = app.selected_session() {
        let yolo = row.yolo;
        let show_closeout = row.show_closeout;
        let enhance = row.enhance;
        let model = row.model.clone();
        let effort = row.effort.clone();
        app.yolo = yolo;
        app.show_closeout = show_closeout;
        app.enhance = enhance;
        if !model.is_empty() {
            app.model = model;
        }
        app.effort = effort;
    }
    if app.selected.is_empty() {
        if let Some(info) = &app.remote_defaults {
            app.model = info.model.clone();
            app.effort = info.effort.clone();
            app.yolo = info.yolo;
            app.enhance = info.enhance;
            app.show_closeout = info.show_closeout;
        }
        app.cards.clear();
        app.todos.clear();
        app.open_todo = None;
        app.closeout.clear();
        app.open_check = None;
        app.closeout_scroll = 0;
        app.todo_scroll = 0;
        app.tasks.clear();
        app.task_detail = None;
        app.task_scroll = 0;
        app.schedules.clear();
        app.schedule_scroll = 0;
        app.phase = None;
        app.retry_status = None;
        app.action = None;
        app.thinking.clear();
        app.thinking_open = false;
        app.context = None;
        app.context_open = false;
        app.queue.clear();
        app.queue_items.clear();
        queue::reconcile(app);
        app.scroll = 0;
        app.follow = true;
        app.question_id = None;
        return;
    }
    if let Some(view) = data.view {
        let visual_open = app
            .open_image
            .as_ref()
            .is_some_and(|image| visual_question(app, image).is_some());
        let next_question = waiting_question_id(&view.cards);
        if next_question != app.question_id && visual_open {
            app.open_image = None;
            app.question_text.clear();
            app.question_cursor = None;
        }
        let before = arrived_lists(app);
        let work_cards: Vec<_> = app
            .cards
            .iter()
            .filter(|card| !matches!(card, Card::Btw { .. }))
            .cloned()
            .collect();
        app.cards.clear();
        app.card_event_ids.clear();
        for card in &view.cards {
            if let Some(mut screen) = to_screen_card(card) {
                if let Card::Artifact { file, focused, .. } = &mut screen {
                    *focused = app.artifact_focus.as_ref() == Some(&file.id);
                }
                app.cards.push(screen);
                app.card_event_ids.push(text_field(&card.body, "eventId"));
            }
        }
        if app
            .cards
            .iter()
            .any(|card| matches!(card, Card::Enhance { .. }))
        {
            app.enhance_source = None;
        }
        app.todos = view.todos;
        if app
            .open_todo
            .as_ref()
            .is_some_and(|id| app.todos.iter().all(|item| item.id != *id))
        {
            app.open_todo = None;
        }
        app.closeout = view.closeout.iter().map(to_closeout_check).collect();
        if app
            .open_check
            .as_ref()
            .is_some_and(|id| app.closeout.iter().all(|item| item.id != *id))
        {
            app.open_check = None;
        }
        app.tasks = view
            .tasks
            .into_iter()
            .map(|item| screen::TaskLine {
                id: item.id,
                argv: item.argv.join(" "),
                state: item.state.label().to_string(),
            })
            .collect();
        app.schedules = view
            .schedules
            .into_iter()
            .map(|item| screen::ScheduleLine {
                id: item.id,
                note: item.note,
                remaining_min: item.remaining_min,
            })
            .collect();
        prune_panes(app);
        let fresh = open_arrived(app, &before);
        if let Some(detail) = data.task {
            if fresh.as_deref() == Some(detail.id.as_str())
                || app
                    .task_detail
                    .as_ref()
                    .is_some_and(|current| current.id == detail.id)
            {
                app.task_detail = Some(detail);
            }
        }
        app.retry_status = view.retry_status;
        app.action = view.action;
        apply_flight(app, view.phase, view.thinking);
        app.queue = view.queue;
        app.queue_items = view.queue_items;
        queue::reconcile(app);
        app.question_id = waiting_question_id(&view.cards);
        if question_overlay && app.question_id.is_none() {
            app.overlay = false;
        }
        app.skills = view.skills;
        app.context = view.context;
        if app
            .context
            .as_ref()
            .and_then(|usage| usage.percent)
            .is_none()
        {
            app.context_open = false;
        }
        if app.cards.is_empty() {
            app.scroll = 0;
        } else if app.follow
            && work_cards.iter().ne(app
                .cards
                .iter()
                .filter(|card| !matches!(card, Card::Btw { .. })))
        {
            app.scroll = follow_tail(app);
        } else {
            let tail = follow_tail(app);
            if app.scroll >= tail {
                app.follow = true;
                app.scroll = tail;
            }
        }
    }
    if app.context_open
        || app.overlay
        || app.open_file.is_some()
        || app.thinking_open
        || preview_open(app)
    {
        let keep = if app.context_open
            || app.thinking_open
            || app.open_file.is_some()
            || preview_open(app)
        {
            true
        } else if app.overlay_pull {
            app.selected_session()
                .and_then(|row| row.pull_url.as_ref())
                .is_some()
        } else if choosing(app) {
            true
        } else {
            overlay_from(&app.cards, &app.question_text).is_some()
        };
        if !keep {
            app.overlay = false;
            app.overlay_pull = false;
            app.workspace_step = WorkspaceStep::Off;
            app.profile_step = None;
            app.open_file = None;
            app.open_text = None;
            app.thinking_open = false;
        }
    }
    maybe_open_question(app);
    maybe_open_enhance(app);
}

pub async fn poll(app: &mut App, client: &Client) -> Result<(), String> {
    app.poll_revision = app.poll_revision.wrapping_add(1);
    let context = PollContext::new(app);
    let request = context.clone();
    let client = client.clone();
    let data = tokio::spawn(async move { fetch(&request, &client).await })
        .await
        .map_err(|_| "Session refresh stopped.".to_string())??;
    if context.matches(app) {
        apply_poll(app, data);
    }
    Ok(())
}

struct PendingPoll {
    generation: u64,
    context: PollContext,
    task: tokio::task::JoinHandle<Result<PollData, String>>,
}

pub(super) struct BackgroundPoll {
    generation: u64,
    pending: Option<PendingPoll>,
    started: Instant,
}

impl BackgroundPoll {
    pub(super) fn new() -> Self {
        Self {
            generation: 0,
            pending: None,
            started: Instant::now(),
        }
    }

    pub(super) fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(pending) = self.pending.take() {
            pending.task.abort();
        }
    }

    pub(super) async fn advance(&mut self, app: &mut App, client: &Client) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.context.matches(app))
        {
            self.invalidate();
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.task.is_finished())
        {
            let PendingPoll {
                generation,
                context,
                task,
            } = self.pending.take().unwrap();
            if let Ok(result) = task.await {
                if generation == self.generation && context.matches(app) {
                    match result {
                        Ok(data) => apply_poll(app, data),
                        Err(error) => app.notice = Some(error),
                    }
                }
            }
        }
        if self.pending.is_none() && self.started.elapsed() >= Duration::from_millis(250) {
            let context = PollContext::new(app);
            let request = context.clone();
            let client = client.clone();
            let task = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(8), fetch(&request, &client))
                    .await
                    .map_err(|_| "Server connection timed out.".to_string())?
            });
            self.pending = Some(PendingPoll {
                generation: self.generation,
                context,
                task,
            });
            self.started = Instant::now();
        }
    }
}

impl Drop for BackgroundPoll {
    fn drop(&mut self) {
        self.invalidate();
    }
}

pub(super) fn remember_sessions(app: &mut App) {
    let previous = app.known_status.take();
    let mut next = BTreeMap::new();
    for row in &app.sessions {
        if let Some(known) = &previous {
            if !row.archived && row.id != app.selected {
                if let Some(phrase) =
                    notice_for(known.get(&row.id).copied(), row.status, row.waiting)
                {
                    ring_session(&row.id, phrase);
                }
            }
        }
        next.insert(row.id.clone(), (row.status, row.waiting));
    }
    app.known_status = Some(next);
}

fn live_selection(
    sessions: &[SessionRow],
    selected: &str,
    workspace: &Path,
    remote: bool,
) -> String {
    if sessions
        .iter()
        .any(|row| row.id == selected && !row.archived)
    {
        return selected.to_string();
    }
    if let Some(id) = newest_for_server(sessions, workspace, remote) {
        return id;
    }
    sessions
        .iter()
        .find(|row| !row.archived)
        .map(|row| row.id.clone())
        .unwrap_or_default()
}

pub(super) fn notice_for(
    previous: Option<(Status, Option<Wait>)>,
    status: Status,
    waiting: Option<Wait>,
) -> Option<&'static str> {
    match (previous.map(|(status, _)| status), status) {
        (None, Status::Waiting) | (Some(Status::Working | Status::Idle), Status::Waiting) => {
            Some(match waiting {
                Some(Wait::Question) => "needs a question",
                Some(Wait::Enhance) => "needs a prompt",
                _ => "needs a permission",
            })
        }
        (Some(Status::Working | Status::Waiting), Status::Idle) => Some("finished"),
        _ => None,
    }
}

pub(super) fn ring_session(id: &str, phrase: &str) {
    let bytes = format!(
        "\u{7}\u{1b}]9;{}: {} {phrase}\u{1b}\\",
        screen::PRODUCT,
        screen::short_id(id)
    )
    .into_bytes();
    let mut slot = NOTICES.lock().expect("notices");
    if slot.stubbed {
        slot.bytes.extend_from_slice(&bytes);
        return;
    }
    drop(slot);
    let mut out = io::stdout();
    if !out.is_terminal() {
        return;
    }
    let _ = out.write_all(&bytes);
    let _ = out.flush();
}

pub(super) fn picker_is_open(app: &App) -> bool {
    !picker_matches(app).is_empty()
}

pub(super) fn after_ask_edit(app: &mut App) {
    if skills::picker_token(&app.ask).is_none() {
        app.skill_picker_closed = false;
        app.skill_highlight = 0;
    }
    let count = picker_matches(app).len();
    if count == 0 {
        app.skill_highlight = 0;
    } else {
        app.skill_highlight = app.skill_highlight.min(count - 1);
    }
}

pub(super) fn fill_skill_picker(app: &mut App) -> bool {
    if app.overlay {
        return false;
    }
    let rows = picker_matches(app);
    if rows.is_empty() {
        return false;
    }
    let index = app.skill_highlight.min(rows.len() - 1);
    let name = rows[index].name.clone();
    let token = skills::picker_token(&app.ask).unwrap_or("");
    if token.eq_ignore_ascii_case(&name) {
        return false;
    }
    let end = token.len() + 1;
    let replacement = if end == app.ask.len() {
        skills::fill_slash(&name)
    } else {
        format!("/{name}")
    };
    editor::replace(app, 0..end, &replacement);
    after_ask_edit(app);
    true
}

pub(super) async fn post_show_closeout(
    app: &mut App,
    client: &Client,
    show: bool,
) -> Result<(), String> {
    if app.selected.is_empty() {
        return Ok(());
    }
    let body = serde_json::json!({ "show": show }).to_string();
    let (status, response) = client
        .request(
            "POST",
            &format!("/v1/sessions/{}/closeout", app.selected),
            Some(&body),
        )
        .await?;
    if status == 204 || status == 200 {
        app.poll_revision = app.poll_revision.wrapping_add(1);
        app.show_closeout = show;
        if let Some(row) = app.sessions.iter_mut().find(|row| row.id == app.selected) {
            row.show_closeout = show;
        }
    } else {
        app.notice = Some(error_text(&response, status));
    }
    Ok(())
}

pub(super) async fn post_enhance(
    app: &mut App,
    client: &Client,
    enhance: bool,
) -> Result<(), String> {
    if app.selected.is_empty() {
        return Ok(());
    }
    let body = serde_json::json!({ "enhance": enhance }).to_string();
    let (status, response) = client
        .request(
            "POST",
            &format!("/v1/sessions/{}/enhance", app.selected),
            Some(&body),
        )
        .await?;
    if status == 204 || status == 200 {
        app.poll_revision = app.poll_revision.wrapping_add(1);
        app.enhance = enhance;
        if let Some(row) = app.sessions.iter_mut().find(|row| row.id == app.selected) {
            row.enhance = enhance;
        }
    } else {
        app.notice = Some(error_text(&response, status));
    }
    Ok(())
}

pub(super) async fn post_yolo(app: &mut App, client: &Client, yolo: bool) -> Result<(), String> {
    if app.selected.is_empty() {
        return Ok(());
    }
    let body = serde_json::json!({ "yolo": yolo }).to_string();
    let (status, response) = client
        .request(
            "POST",
            &format!("/v1/sessions/{}/yolo", app.selected),
            Some(&body),
        )
        .await?;
    if status == 204 || status == 200 {
        app.poll_revision = app.poll_revision.wrapping_add(1);
        app.yolo = yolo;
        if let Some(row) = app.sessions.iter_mut().find(|row| row.id == app.selected) {
            row.yolo = yolo;
        }
    } else {
        app.notice = Some(error_text(&response, status));
    }
    Ok(())
}

pub(super) fn focus_todo(app: &mut App, id: String) {
    app.thinking_open = false;
    app.overlay_pull = false;
    app.open_file = None;
    app.open_text = None;
    app.workspace_step = WorkspaceStep::Off;
    app.profile_step = None;
    app.open_todo = if app.open_todo.as_deref() == Some(id.as_str()) {
        None
    } else {
        Some(id)
    };
    app.overlay = false;
    app.right_panes.insert(RightPane::Todos);
    show_column(app);
}

pub(super) fn show_column(app: &mut App) {
    app.right_open = true;
    if app.right_width < 2 {
        app.right_width = TODOS_WIDTH;
    }
    app.drag = None;
}

pub(super) fn toggle_pane(app: &mut App, pane: RightPane) {
    if app.right_panes.remove(&pane) {
        if app.right_panes.is_empty() {
            app.right_open = false;
        }
        return;
    }
    app.right_panes.insert(pane);
    show_column(app);
}

pub(super) struct ArrivedLists {
    task_ids: Vec<String>,
}

pub(super) fn arrived_lists(app: &App) -> ArrivedLists {
    ArrivedLists {
        task_ids: app.tasks.iter().map(|task| task.id.clone()).collect(),
    }
}

pub(super) fn open_arrived(app: &mut App, before: &ArrivedLists) -> Option<String> {
    app.tasks
        .iter()
        .rfind(|task| !before.task_ids.contains(&task.id))
        .map(|task| task.id.clone())
}

pub(super) fn prune_panes(app: &mut App) {
    if app.todos.is_empty() {
        app.open_todo = None;
        app.todo_scroll = 0;
    }
    if app.closeout.is_empty() {
        app.open_check = None;
        app.closeout_scroll = 0;
    }
    if app.tasks.is_empty() {
        app.task_detail = None;
        app.task_scroll = 0;
    } else if app
        .task_detail
        .as_ref()
        .is_some_and(|detail| app.tasks.iter().all(|task| task.id != detail.id))
    {
        app.task_detail = None;
    }
    if app.schedules.is_empty() {
        app.schedule_scroll = 0;
    }
}

#[cfg(test)]
mod background_tests {
    use super::*;

    fn app_and_data() -> (App, PollData) {
        let model = crate::mock::working();
        let mut app = App::new(
            PathBuf::from("/w"),
            PathBuf::from("/home/u"),
            model.selected.clone(),
        );
        app.sessions = model.sessions.clone();
        app.server = Some("first".into());
        let view = serde_json::from_str(include_str!("../../ios/Fixtures/view.json")).unwrap();
        let data = PollData {
            layout: None,
            sessions: model.sessions,
            projects: Vec::new(),
            info: None,
            view: Some(view),
            task: None,
            yolo_started: false,
            proof: None,
        };
        (app, data)
    }

    fn question_data(answered: bool) -> PollData {
        let (_, mut data) = app_and_data();
        for row in &mut data.sessions {
            row.status = if answered {
                Status::Idle
            } else {
                Status::Waiting
            };
            row.waiting = (!answered).then_some(Wait::Question);
        }
        let view = data.view.as_mut().unwrap();
        view.status = if answered {
            Status::Idle
        } else {
            Status::Waiting
        };
        view.tasks.clear();
        view.todos.clear();
        view.closeout.clear();
        view.schedules.clear();
        view.phase = None;
        view.thinking = None;
        let choices = if answered {
            serde_json::json!([])
        } else {
            serde_json::json!(["continue", "stop"])
        };
        let mut cards = serde_json::json!([
            {"id":"ask","kind":"ask","at":"2026-10-03T00:00:00Z","body":{"text":"Name the binary."}},
            {"id":"question","kind":"question","at":"2026-10-03T00:00:00Z","body":{"eventId":"q1","text":"Which way?","choices":choices}}
        ]);
        if answered {
            cards[1]["body"]["answer"] = serde_json::json!("hello");
            cards.as_array_mut().unwrap().extend([
                serde_json::json!({"id":"answer","kind":"answer","at":"2026-10-03T00:00:00Z","body":{"text":"hello"}}),
                serde_json::json!({"id":"result","kind":"result","at":"2026-10-03T00:00:00Z","body":{"text":"Done."}}),
                serde_json::json!({"id":"proof","kind":"proof","at":"2026-10-03T00:00:00Z","body":{"text":"cargo test passed."}}),
            ]);
        }
        view.cards = serde_json::from_value(cards).unwrap();
        data
    }

    #[test]
    fn an_answered_question_closes_after_an_unanswered_refresh_instead_of_becoming_proof() {
        let (mut app, _) = app_and_data();
        app.ask = "kept draft".into();
        apply_poll(&mut app, question_data(false));
        assert!(matches!(
            screen_model(&app).overlay,
            Some(Overlay::Question { .. })
        ));
        app.overlay = false;
        app.question_text.clear();
        app.question_cursor = None;
        apply_poll(&mut app, question_data(false));
        assert!(matches!(
            screen_model(&app).overlay,
            Some(Overlay::Question { .. })
        ));
        apply_poll(&mut app, question_data(true));
        let model = screen_model(&app);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(76, 24)).unwrap();
        terminal
            .draw(|frame| screen::render(&model, frame.area(), frame))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            rendered.contains("hello"),
            "answer disappeared behind a replacement overlay: {rendered}"
        );
        assert!(model.overlay.is_none());
        assert_eq!(app.ask, "kept draft");
    }

    #[test]
    fn an_explicit_proof_overlay_stays_open_after_refresh() {
        let (mut app, _) = app_and_data();
        apply_poll(&mut app, question_data(true));
        app.overlay = true;
        assert!(matches!(
            screen_model(&app).overlay,
            Some(Overlay::Proof { .. })
        ));
        apply_poll(&mut app, question_data(true));
        assert!(matches!(
            screen_model(&app).overlay,
            Some(Overlay::Proof { .. })
        ));
    }

    #[test]
    fn a_server_picker_stays_open_when_a_background_question_is_answered() {
        let (mut app, _) = app_and_data();
        apply_poll(&mut app, question_data(false));
        connections::open_server_picker(&mut app);
        let picker = screen_model(&app).overlay;
        assert!(picker.is_some());
        apply_poll(&mut app, question_data(true));
        assert_eq!(screen_model(&app).overlay, picker);
    }

    #[tokio::test]
    async fn provider_popup_survives_refresh_and_escape_preserves_the_working_turn() {
        let (mut app, mut data) = app_and_data();
        let client = Client::at(PathBuf::from("/unused"));
        app.ask = "kept draft".into();
        app.provider_popup = Some(providers::Popup::Loading);
        app.overlay = true;
        data.view.as_mut().unwrap().cards.clear();
        apply_poll(&mut app, data);
        assert!(app.overlay);
        assert!(!esc_stops(&app));
        app.overlay = false;
        assert!(!esc_stops(&app));
        apply(&mut app, &client, Effect::CloseOverlay)
            .await
            .unwrap();
        assert!(app.provider_popup.is_none());
        assert_eq!(app.selected_session().unwrap().status, Status::Working);
        assert_eq!(app.ask, "kept draft");
    }

    #[tokio::test]
    async fn previews_survive_refresh_and_escape_without_changing_underlying_work() {
        for image in [false, true] {
            let (mut app, _) = app_and_data();
            let client = Client::at(PathBuf::from("/unused"));
            let effect = if image {
                Effect::OpenImage(
                    crate::attachment::ImageAttachment::from_bytes("dog.png", crate::splash::PNG)
                        .unwrap(),
                )
            } else {
                Effect::OpenText("complete text".repeat(200))
            };
            apply(&mut app, &client, effect).await.unwrap();
            for _ in 0..3 {
                let (_, mut next) = app_and_data();
                next.view.as_mut().unwrap().cards.clear();
                apply_poll(&mut app, next);
                assert!(app.overlay);
                assert!(if image {
                    app.open_image.is_some()
                } else {
                    app.open_text.is_some()
                });
            }
            assert!(!esc_stops(&app));
            app.overlay = false;
            assert!(!esc_stops(&app));
            app.overlay = true;
            let event = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(keystroke(&app, event), Some(Effect::CloseOverlay));
            apply(&mut app, &client, Effect::CloseOverlay)
                .await
                .unwrap();
            assert!(app.open_image.is_none() && app.open_text.is_none());
            assert_eq!(app.selected_session().unwrap().status, Status::Working);
            assert!(esc_stops(&app));
        }
    }

    #[tokio::test]
    async fn a_delayed_refresh_applies_data_without_consuming_input_or_overlays() {
        let (mut app, data) = app_and_data();
        let client = Client::at(PathBuf::from("/unused"));
        let (send, receive) = tokio::sync::oneshot::channel();
        let mut refresh = BackgroundPoll::new();
        let context = PollContext::new(&app);
        let task = tokio::spawn(async move {
            receive.await.unwrap();
            Ok(data)
        });
        refresh.pending = Some(PendingPoll {
            generation: refresh.generation,
            context,
            task,
        });
        app.tick = 17;
        apply(&mut app, &client, Effect::Type('x')).await.unwrap();
        app.picker = Some(Picker::Effort {
            rows: EFFORTS.iter().map(|row| (*row).to_string()).collect(),
            highlight: 0,
        });
        app.overlay = true;
        refresh.advance(&mut app, &client).await;
        assert_eq!(app.ask, "x");
        send.send(()).unwrap();
        while !refresh.pending.as_ref().unwrap().task.is_finished() {
            tokio::task::yield_now().await;
        }
        refresh.advance(&mut app, &client).await;
        assert_eq!(app.ask, "x");
        assert_eq!(app.tick, 17);
        assert!(app.picker.is_some());
        assert!(app.overlay);
        assert!(!app.cards.is_empty());
    }

    #[tokio::test]
    async fn a_completed_old_refresh_cannot_overwrite_a_mutation_after_its_refresh_fails() {
        let (mut app, data) = app_and_data();
        let client = Client::at(PathBuf::from("/unused"));
        let (send, receive) = tokio::sync::oneshot::channel();
        let mut refresh = BackgroundPoll::new();
        let context = PollContext::new(&app);
        let task = tokio::spawn(async move {
            receive.await.unwrap();
            Ok(data)
        });
        refresh.pending = Some(PendingPoll {
            generation: refresh.generation,
            context,
            task,
        });
        app.yolo = true;
        app.model = "new-model".into();
        assert!(poll(&mut app, &client).await.is_err());
        send.send(()).unwrap();
        while !refresh.pending.as_ref().unwrap().task.is_finished() {
            tokio::task::yield_now().await;
        }
        refresh.advance(&mut app, &client).await;
        assert!(app.yolo);
        assert_eq!(app.model, "new-model");
        assert!(app.cards.is_empty());
    }

    #[tokio::test]
    async fn a_refresh_from_an_old_server_or_selection_is_discarded_even_after_switching_back() {
        for change_server in [false, true] {
            let (mut app, data) = app_and_data();
            let client = Client::at(PathBuf::from("/unused"));
            let (send, receive) = tokio::sync::oneshot::channel();
            let mut refresh = BackgroundPoll::new();
            let context = PollContext::new(&app);
            let task = tokio::spawn(async move {
                receive.await.unwrap();
                Ok(data)
            });
            refresh.pending = Some(PendingPoll {
                generation: refresh.generation,
                context,
                task,
            });
            let selected = app.selected.clone();
            if change_server {
                app.server = Some("second".into());
            } else {
                app.selected = "other".into();
            }
            refresh.advance(&mut app, &client).await;
            assert!(refresh.pending.is_none());
            assert_eq!(refresh.generation, 1);
            app.server = Some("first".into());
            app.selected = selected;
            let _ = send.send(());
            tokio::task::yield_now().await;
            refresh.advance(&mut app, &client).await;
            assert!(app.cards.is_empty());
        }
    }
}
