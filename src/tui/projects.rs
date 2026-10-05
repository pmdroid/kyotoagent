use super::*;

pub(super) enum Popup {
    List {
        rows: Vec<ListedProject>,
        highlight: usize,
        resume: bool,
    },
    Form {
        id: Option<String>,
        name: String,
        path: String,
        field: usize,
        resume: bool,
        error: Option<String>,
    },
}

pub(super) struct Job {
    server: Option<String>,
    resume: bool,
    task: tokio::task::JoinHandle<Result<(), String>>,
}

pub(super) fn close(app: &mut App) {
    app.project_popup = None;
    if let Some(job) = app.project_job.take() {
        job.task.abort();
    }
}

pub(super) fn list(app: &mut App, rows: Vec<ListedProject>) {
    app.project_popup = Some(Popup::List {
        rows,
        highlight: 0,
        resume: false,
    });
}

pub(super) fn add(app: &mut App, resume: bool) {
    release_command_surfaces(app);
    app.project_popup = Some(Popup::Form {
        id: None,
        name: String::new(),
        path: String::new(),
        field: 0,
        resume,
        error: None,
    });
}

pub(super) fn key(app: &App, event: KeyEvent) -> Option<Effect> {
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return match event.code {
            KeyCode::Char('c') => Some(Effect::Exit),
            KeyCode::Char('u') => Some(Effect::DeleteLine),
            _ => None,
        };
    }
    match event.code {
        KeyCode::Esc => Some(Effect::CloseOverlay),
        KeyCode::Enter => Some(Effect::Submit),
        KeyCode::Up => Some(Effect::ScrollUp),
        KeyCode::Down => Some(Effect::ScrollDown),
        KeyCode::Backspace => Some(Effect::Backspace),
        KeyCode::Char(c) if !c.is_control() => {
            if matches!(app.project_popup, Some(Popup::List { .. })) {
                c.to_digit(10)
                    .filter(|digit| *digit > 0)
                    .map(|digit| Effect::Choose(digit as usize - 1))
            } else {
                Some(Effect::Type(c))
            }
        }
        _ => None,
    }
}

pub(super) fn handle(app: &mut App, client: &Client, effect: &Effect) -> bool {
    let Some(popup) = &mut app.project_popup else {
        return false;
    };
    if matches!(effect, Effect::Exit) {
        return false;
    }
    if matches!(effect, Effect::CloseOverlay) {
        app.notice = None;
        close(app);
        return true;
    }
    if app.project_job.is_some() {
        return true;
    }
    match popup {
        Popup::List {
            rows,
            highlight,
            resume,
        } => match effect {
            Effect::ScrollUp => *highlight = highlight.saturating_sub(1),
            Effect::ScrollDown => *highlight = (*highlight + 1).min(rows.len()),
            Effect::Submit | Effect::Choose(_) => {
                let index = if let Effect::Choose(index) = effect {
                    *index
                } else {
                    *highlight
                };
                let resume = *resume;
                if index == 0 {
                    add(app, resume);
                } else if let Some(row) = rows.get(index - 1) {
                    app.project_popup = Some(Popup::Form {
                        id: Some(row.id.clone()),
                        name: row.name.clone(),
                        path: row.path.clone(),
                        field: 0,
                        resume,
                        error: None,
                    });
                }
            }
            _ => {}
        },
        Popup::Form {
            id,
            name,
            path,
            field,
            resume,
            error,
        } => {
            let value = if *field == 0 { &mut *name } else { &mut *path };
            match effect {
                Effect::Type(c) if *field < 2 && value.len() < 4096 => {
                    value.push(*c);
                    *error = None;
                }
                Effect::Paste(text) if *field < 2 && value.len() + text.len() < 4096 => {
                    value.push_str(text.trim());
                    *error = None;
                }
                Effect::Backspace if *field < 2 => {
                    value.pop();
                    *error = None;
                }
                Effect::DeleteLine if *field < 2 => {
                    value.clear();
                    *error = None;
                }
                Effect::ScrollUp => {
                    *field = field.saturating_sub(1);
                    *error = None;
                }
                Effect::Submit if *field < 2 => {
                    if value.trim().is_empty() {
                        *error = Some("Enter a value before continuing.".into());
                    } else {
                        *field += 1;
                    }
                }
                Effect::Submit => {
                    let editing = id.is_some();
                    let id = id.clone().unwrap_or_else(|| {
                        name.to_ascii_lowercase()
                            .chars()
                            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                            .collect::<String>()
                            .split('-')
                            .filter(|part| !part.is_empty())
                            .collect::<Vec<_>>()
                            .join("-")
                    });
                    let body =
                        serde_json::json!({"id": id, "name": name.trim(), "path": path.trim()})
                            .to_string();
                    let endpoint = if editing {
                        format!("/v1/projects/{id}")
                    } else {
                        "/v1/projects".into()
                    };
                    let client = client.clone();
                    app.project_job = Some(Job {
                        server: app.server.clone(),
                        resume: *resume,
                        task: tokio::spawn(async move {
                            let (status, response) = tokio::time::timeout(
                                Duration::from_secs(15),
                                client.request(
                                    if editing { "PUT" } else { "POST" },
                                    &endpoint,
                                    Some(&body),
                                ),
                            )
                            .await
                            .map_err(|_| "Saving project timed out.".to_string())??;
                            if matches!(status, 201 | 204) {
                                Ok(())
                            } else {
                                Err(error_text(&response, status))
                            }
                        }),
                    });
                }
                _ => {}
            }
        }
    }
    true
}

pub(super) async fn advance(app: &mut App, client: &Client) {
    let Some(job) = &app.project_job else { return };
    if job.server != app.server {
        close(app);
        return;
    }
    if !job.task.is_finished() {
        return;
    }
    let job = app.project_job.take().unwrap();
    match job.task.await {
        Ok(Ok(())) => {
            *client.projects.lock().unwrap() = None;
            close(app);
            app.notice = Some("Project saved on this server.".into());
            if job.resume {
                catalog::open(app, client, catalog::Target::Projects);
            } else {
                catalog::open(app, client, catalog::Target::ManageProjects);
            }
        }
        result => {
            if let Some(Popup::Form { error, .. }) = &mut app.project_popup {
                *error = Some(match result {
                    Ok(Err(message)) => message,
                    _ => "Saving project stopped.".into(),
                });
            }
        }
    }
}

pub(super) fn overlay(app: &App) -> Option<Overlay> {
    let popup = app.project_popup.as_ref()?;
    let server = app.server_name();
    if app.project_job.is_some() {
        return Some(Overlay::Text {
            text: format!("Saving project on {server}…"),
        });
    }
    Some(match popup {
        Popup::List {
            rows, highlight, ..
        } => Overlay::Question {
            text: format!("Projects on {server}\nSelect a project to edit."),
            choices: std::iter::once("Add project".to_string())
                .chain(
                    rows.iter()
                        .map(|row| format!("{} · {}", row.name, row.path)),
                )
                .enumerate()
                .map(|(index, label)| Choice {
                    label,
                    marked: index == *highlight,
                })
                .collect(),
            prompt: String::new(),
        },
        Popup::Form {
            name,
            path,
            field,
            error,
            ..
        } => {
            let (label, prompt) = match field {
                0 => ("Project name", name.clone()),
                1 => ("Existing directory on this server", path.clone()),
                _ => ("Enter to save project", String::new()),
            };
            Overlay::Question {
                text: format!("Project on {server}\nName: {name}\nDirectory: {path}\n\n{label}\n{}\nEnter next · ↑ previous · Ctrl-U clear · Esc cancel", error.as_deref().unwrap_or("")),
                choices: Vec::new(), prompt,
            }
        }
    })
}
