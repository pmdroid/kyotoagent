use super::*;

#[derive(Clone)]
pub(super) enum Target {
    Models,
    Effort(String),
    Projects,
    ManageProjects,
    Profiles(Option<PendingSession>),
}

pub(super) struct Popup {
    target: Target,
    error: Option<String>,
}

pub(super) struct Job {
    server: Option<String>,
    selected: String,
    task: tokio::task::JoinHandle<Result<Response, String>>,
}

enum Response {
    Models(crate::chat::ModelCatalog),
    Projects(Vec<ListedProject>, Option<crate::pairing::ServerInfo>),
    Profiles(Vec<String>),
}

pub(super) fn close(app: &mut App) {
    app.catalog_popup = None;
    if let Some(job) = app.catalog_job.take() {
        job.task.abort();
    }
}

pub(super) fn open(app: &mut App, client: &Client, target: Target) {
    close(app);
    let client = client.clone();
    let remote = app.server.is_some();
    let request = target.clone();
    app.catalog_popup = Some(Popup {
        target,
        error: None,
    });
    app.catalog_job = Some(Job {
        server: app.server.clone(),
        selected: app.selected.clone(),
        task: tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(15), async move {
                let path = match request {
                    Target::Models | Target::Effort(_) => "/v1/models?diagnostics=true",
                    Target::Projects | Target::ManageProjects => "/v1/projects",
                    Target::Profiles(_) => "/v1/profiles",
                };
                let (status, body) = client.request("GET", path, None).await?;
                if status != 200 {
                    return Err(error_text(&body, status));
                }
                match request {
                    Target::Models | Target::Effort(_) => serde_json::from_str(&body)
                        .or_else(|_| {
                            serde_json::from_str(&body).map(|models| crate::chat::ModelCatalog {
                                models,
                                errors: BTreeMap::new(),
                            })
                        })
                        .map(Response::Models)
                        .map_err(|_| "Invalid model list from server.".into()),
                    Target::Profiles(_) => serde_json::from_str(&body)
                        .map(Response::Profiles)
                        .map_err(|_| "Invalid profile list from server.".into()),
                    Target::Projects | Target::ManageProjects => {
                        let rows = serde_json::from_str(&body)
                            .map_err(|_| "Invalid project list from server.")?;
                        let info = if remote {
                            Some(client.server_info().await?)
                        } else {
                            None
                        };
                        Ok(Response::Projects(rows, info))
                    }
                }
            })
            .await
            .map_err(|_| "Request timed out. Press Enter to retry.".to_string())?
        }),
    });
}

pub async fn advance(app: &mut App) {
    let Some(job) = &app.catalog_job else { return };
    if job.server != app.server || job.selected != app.selected {
        close(app);
        return;
    }
    if !job.task.is_finished() {
        return;
    }
    let job = app.catalog_job.take().unwrap();
    let Some(popup) = app.catalog_popup.take() else {
        return;
    };
    let result = match job.task.await {
        Ok(Ok(Response::Models(catalog))) => {
            let errors = catalog
                .errors
                .iter()
                .map(|(provider, error)| format!("{provider}: {error}"))
                .collect::<Vec<_>>()
                .join("\n");
            if !errors.is_empty() {
                app.notice = Some(errors.clone());
            }
            if catalog.models.is_empty() && !errors.is_empty() {
                Err(errors)
            } else {
                match &popup.target {
                    Target::Effort(model) => {
                        let efforts = catalog
                            .models
                            .into_iter()
                            .find(|row| row.matches(model) && row.provider.is_none())
                            .map(|row| row.reasoning_efforts)
                            .unwrap_or_default();
                        show_effort_picker(app, efforts);
                        Ok(())
                    }
                    _ => show_model_picker(app, catalog.models),
                }
            }
        }
        Ok(Ok(Response::Projects(rows, info))) => {
            if let Some(info) = info {
                app.workspace = PathBuf::from(&info.workspace);
                app.remote_defaults = Some(info);
            }
            if matches!(popup.target, Target::ManageProjects) {
                projects::list(app, rows);
            } else if rows.is_empty() && app.server.is_none() {
                open_workspace_prompt(app);
            } else {
                open_project_prompt(app, rows);
            }
            Ok(())
        }
        Ok(Ok(Response::Profiles(names))) => {
            if let Target::Profiles(pending) = &popup.target {
                show_profile_prompt(app, names, pending.clone());
            }
            Ok(())
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Err("Request stopped. Press Enter to retry.".into()),
    };
    if let Err(error) = result {
        app.notice = Some(error.clone());
        app.catalog_popup = Some(Popup {
            target: popup.target,
            error: Some(error),
        });
    }
}

pub(super) fn key(event: KeyEvent) -> Option<Effect> {
    match event.code {
        KeyCode::Esc => Some(Effect::CloseOverlay),
        KeyCode::Enter => Some(Effect::Submit),
        KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => Some(Effect::Exit),
        _ => None,
    }
}

pub(super) fn handle(app: &mut App, client: &Client, effect: &Effect) -> bool {
    let Some(popup) = &app.catalog_popup else {
        return false;
    };
    match effect {
        Effect::CloseOverlay => close(app),
        Effect::Submit if popup.error.is_some() => {
            let target = popup.target.clone();
            app.notice = None;
            open(app, client, target);
        }
        Effect::Exit | Effect::OpenPalette | Effect::OpenModel | Effect::NewSession => {
            close(app);
            return false;
        }
        _ => {}
    }
    true
}

pub(super) fn overlay(app: &App) -> Option<Overlay> {
    let popup = app.catalog_popup.as_ref()?;
    let text = if let Some(error) = &popup.error {
        format!("{error}\n\nEnter retry · Esc close")
    } else {
        let label = match popup.target {
            Target::Models => "models",
            Target::Effort(_) => "model options",
            Target::Projects | Target::ManageProjects => "projects",
            Target::Profiles(_) => "profiles",
        };
        format!(
            "{} Fetching {label}…\n\nEsc cancel",
            screen::FLUX_SPINNER[app.tick % screen::FLUX_SPINNER.len()]
        )
    };
    Some(Overlay::Text { text })
}
