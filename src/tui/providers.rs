use super::*;
use crate::server::login::{Progress, ProviderStatus};

pub(super) enum Popup {
    Loading,
    List {
        rows: Vec<ProviderStatus>,
        highlight: usize,
    },
    Key {
        provider: ProviderStatus,
        value: String,
    },
    Form {
        values: [String; 4],
        field: usize,
        error: Option<String>,
    },
    Device(Progress),
    Saving,
    Error(String),
}

pub(super) struct Job {
    server: Option<String>,
    task: tokio::task::JoinHandle<Result<Response, String>>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum Response {
    List(Vec<ProviderStatus>),
    Device(Progress),
    Saved,
    Created,
}

pub(super) fn close(app: &mut App) {
    app.provider_popup = None;
    app.provider_job = None;
}

pub(super) fn open(app: &mut App, client: &Client) {
    release_command_surfaces(app);
    app.context_open = false;
    app.open_image = None;
    app.open_todo = None;
    app.provider_popup = Some(Popup::Loading);
    app.overlay = true;
    request(app, client, "GET", "/v1/providers".into(), None);
}

fn request(
    app: &mut App,
    client: &Client,
    method: &'static str,
    path: String,
    body: Option<String>,
) {
    let client = client.clone();
    app.provider_job = Some(Job {
        server: app.server.clone(),
        task: tokio::spawn(async move {
            let (status, response) = tokio::time::timeout(
                Duration::from_secs(8),
                client.request(method, &path, body.as_deref()),
            )
            .await
            .map_err(|_| "provider request timed out")??;
            if status == 201 && path == "/v1/providers" {
                return Ok(Response::Created);
            }
            if status == 204 {
                return Ok(Response::Saved);
            }
            if !matches!(status, 200 | 202) {
                return Err(error_text(&response, status));
            }
            if path == "/v1/providers" {
                serde_json::from_str(&response)
                    .map(Response::List)
                    .map_err(|_| "invalid provider list".into())
            } else {
                serde_json::from_str(&response)
                    .map(Response::Device)
                    .map_err(|_| "invalid login progress".into())
            }
        }),
    });
}

pub(super) async fn advance(app: &mut App, client: &Client) {
    if app
        .provider_job
        .as_ref()
        .is_some_and(|job| job.server != app.server)
    {
        close(app);
        app.overlay = false;
        return;
    }
    if app
        .provider_job
        .as_ref()
        .is_some_and(|job| job.task.is_finished())
    {
        let mut job = app.provider_job.take().unwrap();
        let result = (&mut job.task)
            .await
            .unwrap_or_else(|_| Err("provider request stopped".into()));
        if app.provider_popup.is_none() {
            return;
        }
        if let Err(message) = &result {
            if let Some(Popup::Form { error, .. }) = &mut app.provider_popup {
                *error = Some(message.clone());
                return;
            }
        }
        app.provider_popup = Some(match result {
            Ok(Response::List(rows)) => Popup::List { rows, highlight: 0 },
            Ok(Response::Device(progress)) => {
                if progress.status == "complete" {
                    app.poll_revision = app.poll_revision.wrapping_add(1);
                    app.notice = Some(format!(
                        "Signed in to {} on {}",
                        progress.provider,
                        app.server_name()
                    ));
                }
                Popup::Device(progress)
            }
            Ok(response @ (Response::Saved | Response::Created)) => {
                app.poll_revision = app.poll_revision.wrapping_add(1);
                app.notice = Some(format!(
                    "{} saved on {}",
                    if matches!(response, Response::Created) {
                        "Provider"
                    } else {
                        "API key"
                    },
                    app.server_name()
                ));
                Popup::Loading
            }
            Err(error) => Popup::Error(error),
        });
        if matches!(app.provider_popup, Some(Popup::Loading)) {
            request(app, client, "GET", "/v1/providers".into(), None);
        }
        app.provider_next_poll = Instant::now() + Duration::from_millis(500);
    }
    if app.provider_job.is_none() && Instant::now() >= app.provider_next_poll {
        if let Some(Popup::Device(progress)) = &app.provider_popup {
            if matches!(progress.status.as_str(), "starting" | "pending") {
                request(
                    app,
                    client,
                    "GET",
                    format!("/v1/login/{}", progress.id),
                    None,
                );
            }
        }
    }
}

pub(super) fn key(app: &App, event: KeyEvent) -> Option<Effect> {
    if event.kind != KeyEventKind::Press {
        return None;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return match event.code {
            KeyCode::Char('c') => Some(Effect::Exit),
            KeyCode::Char('x') => Some(Effect::Cancel),
            KeyCode::Char('u') => Some(Effect::DeleteLine),
            _ => None,
        };
    }
    match event.code {
        KeyCode::Esc => Some(Effect::CloseOverlay),
        KeyCode::Enter if !event.modifiers.contains(KeyModifiers::SHIFT) => Some(Effect::Submit),
        KeyCode::Up | KeyCode::BackTab => Some(Effect::ScrollUp),
        KeyCode::Down | KeyCode::Tab => Some(Effect::ScrollDown),
        KeyCode::Backspace => Some(Effect::Backspace),
        KeyCode::Char('o') if matches!(app.provider_popup, Some(Popup::Device(_))) => {
            if let Some(Popup::Device(progress)) = &app.provider_popup {
                progress.verification_url.clone().map(Effect::OpenLink)
            } else {
                None
            }
        }
        KeyCode::Char(c) if !c.is_control() => {
            if matches!(app.provider_popup, Some(Popup::List { .. })) {
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

fn list_window(app: &App, highlight: usize) -> (usize, usize) {
    let pane = screen::split_of(&layout_model(app), app.area).session;
    let visible = usize::from(pane.height.saturating_sub(10)).max(1);
    (highlight.saturating_sub(visible.saturating_sub(1)), visible)
}

fn input(popup: &mut Popup) -> Option<&mut String> {
    match popup {
        Popup::Key { value, .. } => Some(value),
        Popup::Form { values, field, .. } => values.get_mut(*field),
        _ => None,
    }
}

pub(super) fn handle(app: &mut App, client: &Client, effect: &Effect) -> bool {
    let start = match &app.provider_popup {
        Some(Popup::List { highlight, .. }) => list_window(app, *highlight).0,
        _ => 0,
    };
    let Some(popup) = &mut app.provider_popup else {
        return false;
    };
    if matches!(popup, Popup::Form { .. })
        && app.provider_job.is_some()
        && !matches!(effect, Effect::Exit | Effect::CloseOverlay)
    {
        return true;
    }
    match effect {
        Effect::Exit | Effect::OpenLink(_) => return false,
        Effect::CloseOverlay => {
            close(app);
            app.overlay = false;
        }
        Effect::ScrollUp | Effect::ScrollDown => {
            if let Popup::List { rows, highlight } = popup {
                *highlight = if matches!(effect, Effect::ScrollUp) {
                    highlight.saturating_sub(1)
                } else {
                    (*highlight + 1).min(rows.len())
                };
            } else if let Popup::Form { field, .. } = popup {
                *field = if matches!(effect, Effect::ScrollUp) {
                    field.saturating_sub(1)
                } else {
                    (*field + 1).min(4)
                };
            }
        }
        Effect::Type(c) => {
            if let Some(value) = input(popup) {
                if !c.is_control() && value.len() + c.len_utf8() <= 16384 {
                    value.push(*c);
                }
            }
        }
        Effect::Paste(text) => {
            if let Some(value) = input(popup) {
                if value.len() + text.trim().len() <= 16384
                    && !text.trim().chars().any(char::is_control)
                {
                    value.push_str(text.trim());
                }
            }
        }
        Effect::Backspace => {
            if let Some(value) = input(popup) {
                value.pop();
            }
        }
        Effect::DeleteLine => {
            if let Some(value) = input(popup) {
                value.clear();
            }
        }
        Effect::Submit | Effect::Choose(_) => match popup {
            Popup::List { rows, highlight } => {
                let index = if let Effect::Choose(index) = effect {
                    *index + start
                } else {
                    *highlight
                };
                if index == 0 {
                    app.provider_popup = Some(Popup::Form {
                        values: Default::default(),
                        field: 0,
                        error: None,
                    });
                } else if let Some(provider) = rows.get(index - 1).cloned() {
                    if let Some(progress) = provider.login.clone().filter(|progress| {
                        matches!(progress.status.as_str(), "starting" | "pending")
                    }) {
                        app.provider_popup = Some(Popup::Device(progress));
                        app.provider_next_poll = Instant::now();
                    } else if provider.device_login {
                        let path = format!("/v1/login/{}", provider.id);
                        app.provider_popup = Some(Popup::Loading);
                        request(app, client, "POST", path, None);
                    } else if provider.api_key {
                        app.provider_popup = Some(Popup::Key {
                            provider,
                            value: String::new(),
                        });
                    }
                }
            }
            Popup::Key { provider, value } if !value.trim().is_empty() => {
                let path = format!("/v1/providers/{}/key", provider.id);
                let body = serde_json::json!({"api_key":std::mem::take(value)}).to_string();
                app.provider_popup = Some(Popup::Saving);
                request(app, client, "POST", path, Some(body));
            }
            Popup::Form { field, .. } if *field < 4 => *field += 1,
            Popup::Form { values, error, .. } => {
                *error = None;
                let body = serde_json::json!({
                    "id": values[0],
                    "base_url": values[1],
                    "model": values[2],
                    "api_key": values[3],
                })
                .to_string();
                request(app, client, "POST", "/v1/providers".into(), Some(body));
            }
            Popup::Error(_) => open(app, client),
            _ => {}
        },
        Effect::Cancel => {
            if let Popup::Device(progress) = popup {
                if matches!(progress.status.as_str(), "starting" | "pending") {
                    let path = format!("/v1/login/{}", progress.id);
                    app.provider_job = None;
                    app.provider_popup = Some(Popup::Saving);
                    request(app, client, "DELETE", path, None);
                }
            }
        }
        _ => {}
    }
    true
}

pub(super) fn overlay(app: &App) -> Option<Overlay> {
    let popup = app.provider_popup.as_ref()?;
    let server = app.server_name();
    let title = format!("Providers on {server}");
    if matches!(popup, Popup::Form { .. }) && app.provider_job.is_some() {
        return Some(Overlay::Text {
            text: format!("{title}\n\nSaving… • Esc close"),
        });
    }
    let (start, visible) = match popup {
        Popup::List { highlight, .. } => list_window(app, *highlight),
        _ => (0, 0),
    };
    Some(match popup {
        Popup::List { rows, highlight } => Overlay::Question {
            text: format!("{title}\n\nChoose a provider • ↑/↓ Enter • Esc close"),
            choices: std::iter::once("Add OpenAI-compatible provider".to_string())
                .chain(rows.iter().map(|provider| {
                    format!(
                        "{}  {}{}",
                        provider.id,
                        if provider.device_login {
                            "device login"
                        } else {
                            "API key"
                        },
                        if provider.authenticated {
                            " • signed in"
                        } else {
                            ""
                        }
                    )
                }))
                .enumerate()
                .skip(start)
                .take(visible)
                .map(|(index, label)| Choice {
                    label: format!("{}{label}", if index == *highlight { "▸ " } else { "" },),
                    marked: index == *highlight,
                })
                .collect(),
            prompt: String::new(),
        },
        Popup::Form {
            values,
            field,
            error,
        } => {
            let labels = [
                "Provider name (lowercase letters, digits, dashes)",
                "Base URL (for example, http://127.0.0.1:3456/v1)",
                "Model ID",
                "API key (optional)",
                "Enter to save provider",
            ];
            let masked = "•".repeat(values[3].chars().count().min(60));
            let prompt = match field {
                3 => masked.clone(),
                _ => values.get(*field).cloned().unwrap_or_default(),
            };
            Overlay::Question {
                text: format!(
                    "OpenAI-compatible provider on {server}\n\nName: {}  \nBase URL: {}  \nModel: {}  \nAPI key: {masked}\n\n{}\n\n{}\n\nEnter {} · ↑/↓ fields · Ctrl-U clear · Esc cancel",
                    values[0], values[1], values[2], labels[*field], error.as_deref().unwrap_or(""),
                    if *field == 4 { "save" } else { "next" }
                ),
                choices: Vec::new(),
                prompt,
            }
        }
        Popup::Key { provider, value } => Overlay::Question {
            text: format!(
                "{} on {server}\n\nEnter an API key • Enter save • Esc close{}",
                provider.id,
                if provider.environment_key {
                    "\nThe server's configured environment key takes precedence."
                } else {
                    ""
                }
            ),
            choices: Vec::new(),
            prompt: "•".repeat(value.chars().count().min(60)),
        },
        Popup::Device(progress) => Overlay::Text {
            text: format!(
                "{} on {server}\n\n{}\n{}\n{}\n{}",
                progress.provider,
                progress.status,
                progress.verification_url.as_deref().unwrap_or(""),
                progress.user_code.as_deref().unwrap_or(""),
                progress.error.as_deref().unwrap_or(
                    if matches!(progress.status.as_str(), "starting" | "pending") {
                        "o browser • Ctrl-X cancel • Esc close"
                    } else {
                        "Esc close"
                    }
                )
            ),
        },
        Popup::Loading => Overlay::Text {
            text: format!(
                "{title}\n\n{} Fetching providers… • Esc close",
                screen::FLUX_SPINNER[app.tick % screen::FLUX_SPINNER.len()]
            ),
        },
        Popup::Saving => Overlay::Text {
            text: format!("{title}\n\nSaving… • Esc close"),
        },
        Popup::Error(error) => Overlay::Text {
            text: format!("{title}\n\n{error}\nEnter retry • Esc close"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new(
            PathBuf::from("/workspace"),
            PathBuf::from("/client-home"),
            String::new(),
        )
    }

    fn provider() -> ProviderStatus {
        ProviderStatus {
            id: "office".into(),
            device_login: false,
            api_key: true,
            configured: true,
            authenticated: false,
            environment_key: false,
            login: None,
        }
    }

    #[tokio::test]
    async fn key_input_and_popup_close_preserve_chat_draft_and_clear_credentials() {
        let mut app = app();
        app.ask = "draft".into();
        app.provider_popup = Some(Popup::Key {
            provider: provider(),
            value: String::new(),
        });
        app.overlay = true;
        let client = Client::at(PathBuf::from("/nonexistent-socket"));
        apply(&mut app, &client, Effect::Paste("private-key".into()))
            .await
            .unwrap();
        assert!(!format!("{:?}", screen_model(&app)).contains("private-key"));
        assert_eq!(app.ask, "draft");
        assert_eq!(
            keystroke(&app, KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            None
        );
        apply(&mut app, &client, Effect::CloseOverlay)
            .await
            .unwrap();
        assert!(app.provider_popup.is_none());
        assert!(app.provider_job.is_none());
        assert_eq!(app.ask, "draft");
    }

    #[tokio::test]
    async fn delayed_provider_job_does_not_block_popup_input_or_apply_after_server_switch() {
        let mut app = app();
        let client = Client::at(PathBuf::from("/nonexistent-socket"));
        app.ask = "draft".into();
        app.provider_popup = Some(Popup::Key {
            provider: provider(),
            value: String::new(),
        });
        app.provider_job = Some(Job {
            server: None,
            task: tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(Response::List(Vec::new()))
            }),
        });
        let started = Instant::now();
        apply(&mut app, &client, Effect::Type('x')).await.unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(matches!(&app.provider_popup, Some(Popup::Key { value, .. }) if value == "x"));
        app.server = Some("other".into());
        advance(&mut app, &client).await;
        assert!(app.provider_popup.is_none());
        assert!(app.provider_job.is_none());
        assert_eq!(app.ask, "draft");
    }
    #[tokio::test]
    async fn numbered_selection_uses_visible_provider_rows_after_scrolling() {
        let mut app = app();
        let client = Client::at(PathBuf::from("/nonexistent-socket"));
        let rows = (0..30)
            .map(|index| {
                let mut provider = provider();
                provider.id = format!("office{index}");
                provider
            })
            .collect();
        app.provider_popup = Some(Popup::List {
            rows,
            highlight: 30,
        });
        let Some(Overlay::Question { choices, .. }) = overlay(&app) else {
            panic!("provider list is visible");
        };
        assert!(choices.last().unwrap().label.contains("office29"));
        let visible_id = choices[0]
            .label
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();
        let effect =
            keystroke(&app, KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)).unwrap();
        apply(&mut app, &client, effect).await.unwrap();
        assert!(
            matches!(&app.provider_popup, Some(Popup::Key { provider, .. }) if provider.id == visible_id)
        );
    }
}
