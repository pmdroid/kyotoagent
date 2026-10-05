use super::*;

pub(super) struct Removal {
    server: Option<String>,
    session: String,
    id: String,
    task: tokio::task::JoinHandle<Result<(u16, String), String>>,
}

pub(super) fn open(app: &mut App) {
    release_command_surfaces(app);
    app.context_open = false;
    app.open_image = None;
    if app.queue_items.is_empty() {
        app.notice = Some("No removable queued messages.".into());
        return;
    }
    app.queue_open = true;
    reconcile(app);
}

pub(super) fn reconcile(app: &mut App) {
    if app.queue_items.is_empty() {
        app.queue_open = false;
        app.queue_highlight = None;
    } else if !app
        .queue_items
        .iter()
        .any(|item| Some(&item.id) == app.queue_highlight.as_ref())
    {
        app.queue_highlight = Some(app.queue_items[0].id.clone());
    }
}

pub(super) fn nudge(app: &mut App, up: bool) {
    let index = app
        .queue_items
        .iter()
        .position(|item| Some(&item.id) == app.queue_highlight.as_ref())
        .unwrap_or(0);
    let next = if up {
        index.saturating_sub(1)
    } else {
        (index + 1).min(app.queue_items.len().saturating_sub(1))
    };
    app.queue_highlight = app.queue_items.get(next).map(|item| item.id.clone());
}

pub(super) fn overlay(app: &App) -> Option<Overlay> {
    if !app.queue_open {
        return None;
    }
    let rows = app
        .queue_items
        .iter()
        .map(|item| {
            let text = item.text.split_whitespace().collect::<Vec<_>>().join(" ");
            let text = if text.is_empty() {
                "Image message".to_string()
            } else {
                text
            };
            if item.image_count > 0 {
                format!(
                    "{text}  ({} {})",
                    item.image_count,
                    if item.image_count == 1 {
                        "image"
                    } else {
                        "images"
                    }
                )
            } else {
                text
            }
        })
        .collect();
    let highlight = app
        .queue_items
        .iter()
        .position(|item| Some(&item.id) == app.queue_highlight.as_ref())
        .unwrap_or(0);
    Some(Overlay::Queue {
        rows,
        highlight,
        removing: app.queue_removal.is_some(),
    })
}

pub(super) fn remove(app: &mut App, client: &Client) {
    if app.queue_removal.is_some() {
        return;
    }
    let Some(id) = app.queue_highlight.clone() else {
        return;
    };
    if !app.queue_items.iter().any(|item| item.id == id) {
        return;
    }
    let path = format!("/v1/sessions/{}/queue/{id}", app.selected);
    let client = client.clone();
    app.queue_removal = Some(Removal {
        server: app.server.clone(),
        session: app.selected.clone(),
        id,
        task: tokio::spawn(async move { client.request("DELETE", &path, None).await }),
    });
}

pub(super) async fn advance(app: &mut App) {
    if !app
        .queue_removal
        .as_ref()
        .is_some_and(|removal| removal.task.is_finished())
    {
        return;
    }
    let removal = app.queue_removal.take().unwrap();
    let result = removal.task.await;
    if app.server != removal.server || app.selected != removal.session {
        return;
    }
    app.poll_revision = app.poll_revision.wrapping_add(1);
    match result {
        Ok(Ok((204, _))) => {
            if let Some(index) = app
                .queue_items
                .iter()
                .position(|item| item.id == removal.id)
            {
                app.queue_items.remove(index);
                if index < app.queue.len() {
                    app.queue.remove(index);
                }
            }
            reconcile(app);
        }
        Ok(Ok((status, body))) => app.notice = Some(error_text(&body, status)),
        Ok(Err(error)) => app.notice = Some(error),
        Err(error) => app.notice = Some(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_delayed_removal_keeps_input_responsive_and_ignores_another_session() {
        let mut app = App::new(
            PathBuf::from("/w"),
            PathBuf::from("/home/u"),
            "old-session".into(),
        );
        app.ask = "draft".into();
        let (release, wait) = tokio::sync::oneshot::channel();
        app.queue_removal = Some(Removal {
            server: None,
            session: "old-session".into(),
            id: "same-id".into(),
            task: tokio::spawn(async move {
                wait.await.unwrap();
                Ok((204, String::new()))
            }),
        });
        advance(&mut app).await;
        assert!(app.queue_removal.is_some());
        let client = Client::at(PathBuf::from("/missing"));
        apply(&mut app, &client, Effect::Type('x')).await.unwrap();
        assert_eq!(app.ask, "draftx");
        app.selected = "new-session".into();
        app.queue = vec!["another queue".into()];
        app.queue_items = vec![view::QueuedMessage {
            id: "same-id".into(),
            text: "another queue".into(),
            image_count: 0,
            enhance: false,
        }];
        release.send(()).unwrap();
        while !app.queue_removal.as_ref().unwrap().task.is_finished() {
            tokio::task::yield_now().await;
        }
        advance(&mut app).await;
        assert_eq!(app.queue, ["another queue"]);
        assert_eq!(app.ask, "draftx");
        assert!(app.queue_removal.is_none());
    }
}
