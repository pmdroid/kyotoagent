use super::*;

pub(super) enum Popup {
    Remove {
        row: ListedProject,
        highlight: usize,
        resume: bool,
        error: Option<String>,
    },
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
    removed: bool,
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
            if matches!(
                app.project_popup,
                Some(Popup::List { .. } | Popup::Remove { .. } | Popup::Form { field: 2.., .. })
            ) {
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
    if app.project_popup.is_none() {
        return false;
    }
    if matches!(effect, Effect::Exit) {
        return false;
    }
    if app.project_job.is_some() {
        return true;
    }
    let effect = if let (Some(Popup::Form { field, .. }), Effect::Choose(index)) =
        (&mut app.project_popup, effect)
    {
        if *field >= 2 {
            *field = index + 2;
            &Effect::Submit
        } else {
            effect
        }
    } else {
        effect
    };
    let popup = app.project_popup.as_mut().unwrap();
    if matches!(effect, Effect::CloseOverlay) {
        app.notice = None;
        close(app);
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
        Popup::Remove {
            row,
            resume,
            error,
            highlight,
        } => match effect {
            Effect::ScrollUp => *highlight = highlight.saturating_sub(1),
            Effect::ScrollDown => *highlight = (*highlight + 1).min(1),
            Effect::Choose(1) => close(app),
            Effect::Submit if *highlight == 1 => close(app),
            Effect::Submit | Effect::Choose(0) => {
                let endpoint = format!("/v1/projects/{}", row.id);
                let client = client.clone();
                *error = None;
                app.project_job = Some(Job {
                    server: app.server.clone(),
                    resume: *resume,
                    removed: true,
                    task: tokio::spawn(async move {
                        let (status, body) = tokio::time::timeout(
                            Duration::from_secs(15),
                            client.request("DELETE", &endpoint, None),
                        )
                        .await
                        .map_err(|_| {
                            "Removing project timed out. Retry or cancel.".to_string()
                        })??;
                        if status == 204 {
                            Ok(())
                        } else {
                            Err(error_text(&body, status))
                        }
                    }),
                });
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
                Effect::ScrollDown => *field = (*field + 1).min(if id.is_some() { 4 } else { 2 }),
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
                Effect::Submit if *field == 3 && id.is_some() => {
                    app.project_popup = Some(Popup::Remove {
                        highlight: 1,
                        row: ListedProject {
                            id: id.clone().unwrap(),
                            name: name.clone(),
                            path: path.clone(),
                        },
                        resume: *resume,
                        error: None,
                    });
                }
                Effect::Submit if *field == 4 => close(app),
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
                        removed: false,
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
            app.notice = Some(
                if job.removed {
                    "Project registration removed. Sessions and files preserved."
                } else {
                    "Project saved on this server."
                }
                .into(),
            );
            if job.resume {
                catalog::open(app, client, catalog::Target::Projects);
            } else {
                catalog::open(app, client, catalog::Target::ManageProjects);
            }
        }
        result => {
            if let Some(Popup::Form { error, .. } | Popup::Remove { error, .. }) =
                &mut app.project_popup
            {
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
            text: format!(
                "{} project on {server}…",
                if app.project_job.as_ref().is_some_and(|job| job.removed) {
                    "Removing"
                } else {
                    "Saving"
                }
            ),
        });
    }
    Some(match popup {
        Popup::Remove { row, error, highlight, .. } => Overlay::Question {
            text: format!("Remove {} from {server}?\nOnly the project registration will be forgotten. Sessions and files are preserved.\n{}", row.name, error.as_deref().unwrap_or("")),
            choices: ["Remove project registration", "Cancel"].into_iter().enumerate().map(|(index, label)| Choice { label: format!("{}{}", if index == *highlight { "> " } else { "  " }, label), marked: index == *highlight }).collect(), prompt: String::new(),
        },
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
            id,
            ..
        } => {
            let (label, prompt) = match field {
                0 => (if id.is_some() { "Project name · Continue to save or remove project" } else { "Project name" }, name.clone()),
                1 => ("Existing directory on this server", path.clone()),
                _ => ("Choose an action · ↑/↓ then Enter", String::new()),
            };
            Overlay::Question {
                text: format!("Project on {server}\nName: {name}\nDirectory: {path}\n\n{label}\n{}\nEnter next · ↑ previous · Ctrl-U clear · Esc cancel", error.as_deref().unwrap_or("")),
                choices: if *field >= 2 {
                    let mut labels = vec!["Save project"];
                    if id.is_some() { labels.extend(["Remove project", "Cancel"]); }
                    labels.into_iter().enumerate().map(|(index, label)| Choice { label: format!("{}{}", if index + 2 == *field { "> " } else { "  " }, label), marked: index + 2 == *field }).collect()
                } else { Vec::new() }, prompt,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_a_project_requires_a_separate_confirmation_and_can_be_cancelled() {
        let mut app = App::new(PathBuf::from("/work"), PathBuf::from("/tmp"), String::new());
        let client = Client::at(PathBuf::from("/missing-project-test.sock"));
        list(
            &mut app,
            vec![ListedProject {
                id: "notes".into(),
                name: "Notes".into(),
                path: "/notes".into(),
            }],
        );
        handle(&mut app, &client, &Effect::Choose(1));
        for _ in 0..2 {
            handle(&mut app, &client, &Effect::Submit);
        }
        assert!(matches!(
            app.project_popup,
            Some(Popup::Form { field: 2, .. })
        ));
        handle(&mut app, &client, &Effect::Choose(1));
        assert!(matches!(app.project_popup, Some(Popup::Remove { .. })));
        assert!(app.project_job.is_none());
        assert!(
            matches!(overlay(&app), Some(Overlay::Question { text, .. }) if text.contains("Sessions and files are preserved"))
        );
        handle(&mut app, &client, &Effect::Choose(1));
        assert!(app.project_popup.is_none());
        assert!(app.project_job.is_none());
    }
}

#[cfg(test)]
mod removal_request_tests {
    use super::*;

    #[tokio::test]
    async fn confirmed_removal_sends_delete_and_preserves_sessions() {
        let removed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = removed.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = axum::Router::new()
            .route(
                "/v1/projects/notes",
                axum::routing::delete(move || async move {
                    observed.store(true, std::sync::atomic::Ordering::SeqCst);
                    axum::http::StatusCode::NO_CONTENT
                }),
            )
            .route(
                "/v1/projects",
                axum::routing::get(|| async { axum::Json(Vec::<serde_json::Value>::new()) }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = Client {
            transport: Transport::Url {
                base: format!("http://{address}"),
                http: reqwest::Client::new(),
            },
            projects: Arc::new(Mutex::new(None)),
        };
        let mut app = App::new(PathBuf::from("/work"), PathBuf::from("/tmp"), String::new());
        app.sessions = crate::mock::idle().sessions;
        let sessions = app.sessions.clone();
        app.project_popup = Some(Popup::Remove {
            row: ListedProject {
                id: "notes".into(),
                name: "Notes".into(),
                path: "/notes".into(),
            },
            highlight: 1,
            resume: false,
            error: None,
        });
        handle(&mut app, &client, &Effect::Choose(0));
        tokio::time::timeout(Duration::from_secs(5), async {
            while app.project_job.is_some() {
                tokio::task::yield_now().await;
                advance(&mut app, &client).await;
            }
        })
        .await
        .unwrap();
        assert!(removed.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(app.sessions, sessions);
        assert!(app
            .notice
            .as_ref()
            .unwrap()
            .contains("registration removed"));
        catalog::close(&mut app);
        server.abort();
        let _ = server.await;
    }
}
