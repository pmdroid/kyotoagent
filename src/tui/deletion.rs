use super::*;

pub(super) struct Job {
    server: Option<String>,
    id: String,
    preview: bool,
    task: tokio::task::JoinHandle<Result<Response, String>>,
}

enum Response {
    Status(server::WorkspaceStatus),
    Deleted,
}

pub(super) fn close(app: &mut App) {
    app.delete_confirm = None;
    if let Some(job) = app.delete_job.take() {
        job.task.abort();
    }
}

pub(super) fn fetch_status(app: &mut App, client: &Client, id: &str) {
    request(
        app,
        client,
        id,
        "GET",
        format!("/v1/sessions/{id}/workspace"),
    );
}

fn request(app: &mut App, client: &Client, id: &str, method: &'static str, path: String) {
    if let Some(job) = app.delete_job.take() {
        job.task.abort();
    }
    if let Some(confirm) = &mut app.delete_confirm {
        confirm.busy = true;
    }
    let client = client.clone();
    app.delete_job = Some(Job {
        server: app.server.clone(),
        id: id.into(),
        preview: method == "GET",
        task: tokio::spawn(async move {
            let (status, body) =
                tokio::time::timeout(Duration::from_secs(15), client.request(method, &path, None))
                    .await
                    .map_err(|_| "Session operation timed out. Retry or cancel.".to_string())??;
            if status == 204 {
                return Ok(Response::Deleted);
            }
            if status != 200 {
                return Err(error_text(&body, status));
            }
            serde_json::from_str(&body)
                .map(Response::Status)
                .map_err(|_| "Invalid workspace status.".into())
        }),
    });
}

pub(super) fn toggle_workspace(app: &mut App) {
    let Some(confirm) = &mut app.delete_confirm else {
        return;
    };
    if confirm.busy || !confirm.status.as_ref().is_some_and(|status| status.managed) {
        return;
    }
    confirm.remove_workspace = !confirm.remove_workspace;
    confirm.confirm_dirty = false;
}

pub(super) fn decide(app: &mut App, client: &Client) {
    let Some(confirm) = &app.delete_confirm else {
        return;
    };
    if confirm.busy {
        return;
    }
    let worktree = app
        .sessions
        .iter()
        .any(|row| row.id == confirm.id && row.worktree);
    let delete = usize::from(worktree);
    if worktree && confirm.highlight == 0 {
        toggle_workspace(app);
        return;
    }
    if confirm.highlight != delete {
        close(app);
        return;
    }
    if confirm.remove_workspace
        && !confirm.confirm_dirty
        && confirm
            .status
            .as_ref()
            .is_some_and(|status| !status.changes.is_empty())
    {
        let confirm = app.delete_confirm.as_mut().unwrap();
        confirm.confirm_dirty = true;
        confirm.highlight = delete + 1;
        return;
    }
    let id = confirm.id.clone();
    let path = format!(
        "/v1/sessions/{id}?delete_workspace={}&confirm_dirty={}",
        confirm.remove_workspace, confirm.confirm_dirty
    );
    request(app, client, &id, "DELETE", path);
}

pub(super) async fn advance(app: &mut App, client: &Client) {
    let Some(job) = &app.delete_job else { return };
    if job.server != app.server
        || app
            .delete_confirm
            .as_ref()
            .is_none_or(|confirm| confirm.id != job.id)
    {
        close(app);
        return;
    }
    if !job.task.is_finished() {
        return;
    }
    let job = app.delete_job.take().unwrap();
    if let Some(confirm) = &mut app.delete_confirm {
        confirm.busy = false;
    }
    match job.task.await {
        Ok(Ok(Response::Status(status))) => {
            if let Some(confirm) = &mut app.delete_confirm {
                confirm.status = Some(status);
            }
        }
        Ok(Ok(Response::Deleted)) => {
            close(app);
            if app.selected == job.id {
                select_session(
                    app,
                    next_list_id(&app.sessions, &job.id).unwrap_or_default(),
                );
            }
            if let Err(error) = poll(app, client).await {
                app.notice = Some(error);
            }
        }
        result => {
            let message = match result {
                Ok(Err(message)) => message,
                _ => "Session operation stopped.".into(),
            };
            if let Some(confirm) = &mut app.delete_confirm {
                confirm.error = Some(message);
                if confirm.remove_workspace && !job.preview {
                    confirm.confirm_dirty = false;
                    fetch_status(app, client, &job.id);
                }
            }
        }
    }
}
