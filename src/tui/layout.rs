use super::*;
use crate::config::Layout;

pub(super) struct Job {
    server: Option<String>,
    layout: Layout,
    task: tokio::task::JoinHandle<Result<(), String>>,
}

pub(super) fn current(app: &App) -> Layout {
    Layout {
        left_open: app.left_open,
        left_width: app.left_width,
        right_open: app.right_open,
        right_width: app.right_width,
        right_panes: app.right_panes.clone(),
    }
}

pub(super) fn apply_saved(app: &mut App, layout: Option<Layout>) {
    if app.saved_layout.as_ref() == Some(&layout) || app.layout_job.is_some() {
        return;
    }
    let value = layout.clone().unwrap_or(Layout {
        left_open: true,
        left_width: LIST_WIDTH,
        right_open: false,
        right_width: TODOS_WIDTH,
        right_panes: BTreeSet::new(),
    });
    app.left_open = value.left_open;
    app.left_width = value.left_width;
    app.right_open = value.right_open;
    app.right_width = value.right_width;
    app.right_panes = value.right_panes;
    app.drag = None;
    app.saved_layout = Some(layout);
}

pub(super) fn save(app: &mut App, client: &Client) {
    if app.layout_job.is_some() {
        return;
    }
    let layout = current(app);
    let body = serde_json::to_string(&layout).unwrap();
    let client = client.clone();
    app.poll_revision = app.poll_revision.wrapping_add(1);
    app.notice = Some("Saving layout…".into());
    app.layout_job = Some(Job {
        server: app.server.clone(),
        layout,
        task: tokio::spawn(async move {
            let (status, body) = tokio::time::timeout(
                Duration::from_secs(12),
                client.request("PUT", "/v1/layout", Some(&body)),
            )
            .await
            .map_err(|_| "Saving layout timed out. Try again.".to_string())??;
            if status != 204 {
                return Err(error_text(&body, status));
            }
            Ok(())
        }),
    });
}

pub(super) async fn advance(app: &mut App) {
    if !app
        .layout_job
        .as_ref()
        .is_some_and(|job| job.task.is_finished())
    {
        return;
    }
    let Some(mut job) = app.layout_job.take() else {
        return;
    };
    if job.server != app.server {
        return;
    }
    app.poll_revision = app.poll_revision.wrapping_add(1);
    match (&mut job.task).await {
        Ok(Ok(())) => {
            app.saved_layout = Some(Some(job.layout));
            app.notice = Some("Layout saved for all sessions on this server.".into());
        }
        Ok(Err(error)) => app.notice = Some(error),
        Err(_) => app.notice = Some("Saving layout failed. Try again.".into()),
    }
}
