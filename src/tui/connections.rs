use super::*;
use crate::pairing::Connections;

pub(super) fn open_server_picker(app: &mut App) {
    match Connections::load(&app.home.join(".kyotoagent/config.toml")) {
        Ok(saved) => {
            release_command_surfaces(app);
            app.context_open = false;
            app.open_image = None;
            app.open_todo = None;
            app.server_highlight = 0;
            app.server_names = saved.server_names;
            app.server_step = Some(saved.servers.into_keys().collect());
            app.overlay = true;
        }
        Err(error) => app.notice = Some(error),
    }
}

struct SwitchRequest {
    origin: Option<String>,
    target: Option<String>,
    client: Client,
    context: poll::PollContext,
    workspace: PathBuf,
}

struct PreparedSwitch {
    request: SwitchRequest,
    data: poll::PollData,
}

fn prepare_switch(
    app: &App,
    cached: &BTreeMap<Option<String>, App>,
    target: Option<String>,
) -> Result<SwitchRequest, String> {
    let saved = Connections::load(&app.home.join(".kyotoagent/config.toml"))?;
    let client = match target.as_ref() {
        Some(id) => Client::at_url(saved.servers.get(id).ok_or("server is not saved")?)?,
        None => Client::at(app.home.join(".kyotoagent").join(server::SOCKET_FILE)),
    };
    let workspace = if target.is_some() {
        PathBuf::new()
    } else {
        current_workspace()?
    };
    let mut fresh = App::new(workspace.clone(), app.home.clone(), String::new());
    fresh.server = target.clone();
    let source = if target == app.server {
        app
    } else {
        cached.get(&target).unwrap_or(&fresh)
    };
    Ok(SwitchRequest {
        origin: app.server.clone(),
        target,
        client,
        context: poll::PollContext::new(source),
        workspace,
    })
}

async fn fetch_switch(request: SwitchRequest) -> Result<PreparedSwitch, String> {
    if let Transport::Socket(socket) = &request.client.transport {
        server::local::ensure(socket).await?;
    }
    let data = tokio::time::timeout(
        Duration::from_secs(8),
        poll::fetch(&request.context, &request.client),
    )
    .await
    .map_err(|_| "server connection timed out")??;
    Ok(PreparedSwitch { request, data })
}

fn finish_switch(
    app: &mut App,
    client: &mut Client,
    cached: &mut BTreeMap<Option<String>, App>,
    prepared: PreparedSwitch,
) -> Result<(), String> {
    let PreparedSwitch { request, data } = prepared;
    if app.server != request.origin {
        return Err("server changed while connecting".to_string());
    }
    Connections::select(
        &app.home.join(".kyotoagent/config.toml"),
        request.target.as_deref(),
    )?;
    let names = Connections::load(&app.home.join(".kyotoagent/config.toml"))?.server_names;
    if request.target == app.server {
        if request.context.matches(app) {
            poll::apply_poll(app, data);
        }
        *client = request.client;
        app.server_names = names;
        release_command_surfaces(app);
        app.notice = None;
        return Ok(());
    }
    let mut next = cached
        .remove(&request.target)
        .unwrap_or_else(|| App::new(request.workspace, app.home.clone(), String::new()));
    next.area = app.area;
    next.server = request.target;
    next.server_names = names;
    poll::apply_poll(&mut next, data);
    release_command_surfaces(&mut next);
    release_command_surfaces(app);
    let old = std::mem::replace(app, next);
    cached.insert(old.server.clone(), old);
    *client = request.client;
    app.notice = None;
    Ok(())
}

#[cfg(test)]
pub(super) async fn switch_server(
    app: &mut App,
    client: &mut Client,
    cached: &mut BTreeMap<Option<String>, App>,
    target: Option<String>,
) -> Result<(), String> {
    let request = prepare_switch(app, cached, target)?;
    let prepared = fetch_switch(request).await?;
    finish_switch(app, client, cached, prepared)
}

#[derive(Default)]
pub(super) struct BackgroundSwitch {
    pending: Option<tokio::task::JoinHandle<Result<PreparedSwitch, String>>>,
}

impl Drop for BackgroundSwitch {
    fn drop(&mut self) {
        if let Some(task) = &self.pending {
            task.abort();
        }
    }
}

impl BackgroundSwitch {
    pub(super) fn forget(
        &mut self,
        app: &mut App,
        client: &mut Client,
        cached: &mut BTreeMap<Option<String>, App>,
        id: &str,
    ) {
        if let Some(pending) = self.pending.take() {
            pending.abort();
        }
        cached.remove(&Some(id.to_string()));
        if app.server.as_deref() != Some(id) {
            return;
        }
        let mut local = cached.remove(&None).unwrap_or_else(|| {
            App::new(
                current_workspace().unwrap_or_else(|_| app.home.clone()),
                app.home.clone(),
                String::new(),
            )
        });
        local.area = app.area;
        local.server_names = app.server_names.clone();
        release_command_surfaces(&mut local);
        release_command_surfaces(app);
        *app = local;
        *client = Client::at(app.home.join(".kyotoagent").join(server::SOCKET_FILE));
        self.request(app, cached, None);
    }

    pub(super) fn request(
        &mut self,
        app: &mut App,
        cached: &BTreeMap<Option<String>, App>,
        target: Option<String>,
    ) {
        if let Some(previous) = self.pending.take() {
            previous.abort();
        }
        match prepare_switch(app, cached, target) {
            Ok(request) => {
                app.notice = Some(
                    if request.target.is_none()
                        && !app
                            .home
                            .join(".kyotoagent")
                            .join(server::SOCKET_FILE)
                            .exists()
                    {
                        "Starting local server…".into()
                    } else {
                        format!(
                            "Connecting to {}…",
                            request
                                .target
                                .as_deref()
                                .map_or("Local", |id| app.server_label(id))
                        )
                    },
                );
                self.pending = Some(tokio::spawn(fetch_switch(request)));
            }
            Err(error) => app.notice = Some(error),
        }
    }

    pub(super) async fn advance(
        &mut self,
        app: &mut App,
        client: &mut Client,
        cached: &mut BTreeMap<Option<String>, App>,
    ) -> bool {
        if !self.pending.as_ref().is_some_and(|task| task.is_finished()) {
            return false;
        }
        let Some(task) = self.pending.take() else {
            return false;
        };
        let result = match task.await {
            Ok(Ok(prepared)) => finish_switch(app, client, cached, prepared),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("server connection failed".to_string()),
        };
        match result {
            Ok(()) => true,
            Err(error) => {
                app.notice = Some(error);
                false
            }
        }
    }
}

pub(super) struct Popup {
    page: Page,
    input: String,
    highlight: usize,
    error: Option<String>,
    id: u64,
}

enum Page {
    Import,
    Manage {
        servers: Vec<(String, String)>,
        remove: bool,
    },
    Rename {
        id: String,
    },
    Remove {
        id: String,
    },
    Verified {
        uri: String,
        info: Box<crate::pairing::PairingInfo>,
    },
    Listen,
    Host,
    Share {
        uri: String,
        qr: String,
    },
    Waiting,
}

pub(super) struct Job {
    server: Option<String>,
    id: u64,
    task: tokio::task::JoinHandle<Result<(Page, String), String>>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) fn close(app: &mut App) {
    app.server_popup = None;
    app.server_job = None;
}

fn open(app: &mut App, page: Page, input: String) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    close(app);
    app.server_step = None;
    app.server_popup = Some(Popup {
        page,
        input,
        highlight: 0,
        error: None,
        id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    });
    app.overlay = true;
}

fn start<F>(app: &mut App, task: F)
where
    F: std::future::Future<Output = Result<(Page, String), String>> + Send + 'static,
{
    let Some(popup) = &mut app.server_popup else {
        return;
    };
    popup.error = None;
    app.server_job = Some(Job {
        server: app.server.clone(),
        id: popup.id,
        task: tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(12), task)
                .await
                .map_err(|_| "server operation timed out".to_string())?
        }),
    });
}

pub(super) async fn advance(app: &mut App) {
    if !app
        .server_job
        .as_ref()
        .is_some_and(|job| job.task.is_finished())
    {
        return;
    }
    let Some(mut job) = app.server_job.take() else {
        return;
    };
    let result = (&mut job.task).await;
    let Some(popup) = &mut app.server_popup else {
        return;
    };
    if job.server != app.server || job.id != popup.id {
        return;
    }
    match result {
        Ok(Ok((page, input))) => {
            popup.page = page;
            popup.input = input;
            popup.highlight = 0;
            popup.error = None;
        }
        Ok(Err(error)) => popup.error = Some(error),
        Err(_) => popup.error = Some("server operation failed".to_string()),
    }
}

fn popup_choices(page: &Page) -> Vec<&str> {
    match page {
        Page::Verified { .. } => vec!["Save and connect", "Save"],
        Page::Share { .. } => vec!["Copy pairing link"],
        Page::Manage { servers, .. } => servers.iter().map(|(_, label)| label.as_str()).collect(),
        Page::Remove { .. } => vec!["Cancel", "Remove server"],
        _ => Vec::new(),
    }
}

fn picker_choices(app: &App, names: &[String]) -> Vec<String> {
    let mut rows = vec!["Local".to_string()];
    rows.extend(names.iter().map(|id| app.server_label(id).to_string()));
    rows.extend(["Add server", "Share this server", "Configure HTTPS"].map(str::to_string));
    if !names.is_empty() {
        rows.extend(["Rename server", "Remove server"].map(str::to_string));
    }
    rows.into_iter()
        .enumerate()
        .map(|(index, label)| {
            if index == app.server_highlight {
                format!("› {label}")
            } else {
                label
            }
        })
        .collect()
}

pub(super) fn server_overlay(app: &App) -> Option<Overlay> {
    if let Some(popup) = &app.server_popup {
        let target = app.server_name();
        if let Page::Share { qr, .. } = &popup.page {
            return Some(Overlay::Pairing {
                title: format!(
                    "Share {target} · Code expires in 10 min · Enter copies · Esc closes"
                ),
                qr: qr.clone(),
            });
        }
        let mut text = match &popup.page {
            Page::Import => "Add server\nPaste a pairing link, then press Enter to verify.".to_string(),
            Page::Manage { remove, .. } => format!("{} server\nChoose a saved server.", if *remove { "Remove" } else { "Rename" }),
            Page::Rename { id } => format!("Rename server\n{}\n\nServer name\nEnter save · Ctrl-U clear · Esc cancel", app.server_label(id)),
            Page::Remove { id } => {
                let mut text = format!("Remove server {}?\nThe saved connection and its unsent drafts will be removed.\nPair again to reconnect.", app.server_label(id));
                if app.server.as_ref() == Some(id) {
                    text.push_str("\nThis disconnects the server and switches to Local.");
                }
                text
            }
            Page::Verified { uri, info } => {
                let host = crate::pairing::connection(uri).map(|(base, _)| base).unwrap_or_default();
                format!("Verified {} {}\n{}\nWorkspace: {}\n{} models · {} repositories", info.server.name, info.server.version, host, info.server.workspace, info.models.len(), info.repositories.len())
            }
            Page::Listen => format!("Configure HTTPS on {target}\nListen IP address and port. Enter to enable."),
            Page::Host => format!("Share {target}\nReachable hostname or IP address and HTTPS port. Enter to create link."),
            Page::Share { qr, .. } => format!("Share {target}\n```\n{qr}\n```\nThe link grants access to this server. Enter to copy."),
            Page::Waiting => format!("Checking HTTPS on {target}…"),
        };
        if app.server_job.is_some() {
            text.push_str("\nConnecting…");
        }
        if let Some(error) = &popup.error {
            text.push_str(&format!("\n{error}"));
        }
        let choices = popup_choices(&popup.page)
            .into_iter()
            .enumerate()
            .map(|(index, label)| Choice {
                label: if index == popup.highlight {
                    format!("› {label}")
                } else {
                    label.to_string()
                },
                marked: false,
            })
            .collect();
        return Some(Overlay::Question {
            text,
            choices,
            prompt: if matches!(popup.page, Page::Import) {
                "•".repeat(popup.input.chars().count().min(70))
            } else {
                popup.input.clone()
            },
        });
    }
    let names = app.server_step.as_ref()?;
    Some(Overlay::Question {
        text: format!("Which server?\nConnected to {}", app.server_name()),
        choices: picker_choices(app, names)
            .into_iter()
            .map(|label| Choice {
                label,
                marked: false,
            })
            .collect(),
        prompt: String::new(),
    })
}

pub(super) fn key(app: &App, event: KeyEvent) -> Option<Effect> {
    match event.code {
        KeyCode::Esc => Some(Effect::CloseOverlay),
        KeyCode::Enter if !event.modifiers.contains(KeyModifiers::SHIFT) => Some(Effect::Submit),
        KeyCode::Up => Some(Effect::ScrollUp),
        KeyCode::Down => Some(Effect::ScrollDown),
        KeyCode::Backspace => Some(Effect::Backspace),
        KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Effect::DeleteLine)
        }
        KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => Some(Effect::Exit),
        KeyCode::Char(c)
            if !event
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            let selecting = app.server_step.is_some()
                || app
                    .server_popup
                    .as_ref()
                    .is_some_and(|popup| !popup_choices(&popup.page).is_empty());
            if selecting {
                c.to_digit(10)
                    .filter(|digit| *digit > 0)
                    .map(|digit| Effect::Choose(digit as usize - 1))
            } else if !c.is_control() {
                Some(Effect::Type(c))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(super) fn choose_server(app: &mut App, client: &Client, index: usize) {
    let Some(names) = &app.server_step else {
        return;
    };
    let count = names.len();
    if index <= count {
        let target = if index == 0 {
            None
        } else {
            Some(names[index - 1].clone())
        };
        close_overlay(app);
        app.server_requested = Some(target);
        return;
    }
    match index - count {
        1 => open(app, Page::Import, String::new()),
        2 | 3 => {
            open(app, Page::Waiting, String::new());
            let client = client.clone();
            let server = app.server.clone();
            start(app, async move {
                let value = request_json(&client, "GET", "/v1/https", None).await?;
                match value["addr"].as_str() {
                    Some(addr) => Ok((Page::Host, advertised_host(addr, server.as_deref()))),
                    None => Ok((Page::Listen, "0.0.0.0:8484".to_string())),
                }
            });
        }
        4 | 5 if !names.is_empty() => {
            let servers = names
                .iter()
                .map(|id| (id.clone(), app.server_label(id).to_string()))
                .collect();
            open(
                app,
                Page::Manage {
                    servers,
                    remove: index - count == 5,
                },
                String::new(),
            );
        }
        _ => {}
    }
}

fn advertised_host(addr: &str, server: Option<&str>) -> String {
    let Ok(addr) = addr.parse::<std::net::SocketAddr>() else {
        return addr.to_string();
    };
    if let Some(url) = server.and_then(|id| reqwest::Url::parse(&format!("https://{id}")).ok()) {
        if let Some(host) = url.host() {
            return format!("{host}:{}", addr.port());
        }
    }
    if addr.ip().is_unspecified() {
        format!("localhost:{}", addr.port())
    } else {
        addr.to_string()
    }
}

async fn request_json(
    client: &Client,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let body = body.map(|value| value.to_string());
    let (status, response) = client.request(method, path, body.as_deref()).await?;
    if status != 200 {
        return Err(error_text(&response, status));
    }
    serde_json::from_str(&response).map_err(|_| "invalid server response".to_string())
}

pub(super) fn handle(app: &mut App, client: &Client, effect: &Effect) -> bool {
    if app.server_step.is_some() {
        let last = app.server_step.as_ref().map_or(0, |names| {
            picker_choices(app, names).len().saturating_sub(1)
        });
        match effect {
            Effect::ScrollUp => app.server_highlight = app.server_highlight.saturating_sub(1),
            Effect::ScrollDown => app.server_highlight = (app.server_highlight + 1).min(last),
            Effect::Submit => choose_server(app, client, app.server_highlight),
            Effect::Choose(index) => choose_server(app, client, *index),
            Effect::CloseOverlay => close_overlay(app),
            Effect::Exit => return false,
            _ => {}
        }
        return true;
    }
    let Some(popup) = &mut app.server_popup else {
        return false;
    };
    if matches!(effect, Effect::CloseOverlay) {
        close_overlay(app);
        return true;
    }
    if matches!(effect, Effect::Exit) {
        return false;
    }
    if app.server_job.is_some() {
        return true;
    }
    let editing = matches!(
        popup.page,
        Page::Import | Page::Listen | Page::Host | Page::Rename { .. }
    );
    match effect {
        Effect::Type(c) if editing => popup.input.push(*c),
        Effect::Paste(text) if editing => popup.input.push_str(text.trim()),
        Effect::Backspace if editing => {
            popup.input.pop();
        }
        Effect::DeleteLine if editing => popup.input.clear(),
        Effect::ScrollUp => popup.highlight = popup.highlight.saturating_sub(1),
        Effect::ScrollDown => {
            popup.highlight =
                (popup.highlight + 1).min(popup_choices(&popup.page).len().saturating_sub(1))
        }
        Effect::Submit | Effect::Choose(_) => submit_popup(app, client, effect),
        _ => {}
    }
    true
}

fn submit_popup(app: &mut App, client: &Client, effect: &Effect) {
    let Some(popup) = &mut app.server_popup else {
        return;
    };
    let index = if let Effect::Choose(index) = effect {
        *index
    } else {
        popup.highlight
    };
    let input = popup.input.trim().to_string();
    match &popup.page {
        Page::Manage { servers, remove } => {
            if let Some((id, name)) = servers.get(index) {
                let page = if *remove {
                    Page::Remove { id: id.clone() }
                } else {
                    Page::Rename { id: id.clone() }
                };
                let input = if *remove { String::new() } else { name.clone() };
                open(app, page, input);
            }
        }
        Page::Rename { id } => {
            match Connections::rename(&app.home.join(".kyotoagent/config.toml"), id, &input) {
                Ok(()) => {
                    open_server_picker(app);
                    app.notice = Some("Server renamed.".into());
                }
                Err(error) => popup.error = Some(error),
            }
        }
        Page::Remove { .. } if index == 0 => open_server_picker(app),
        Page::Remove { id } if index == 1 => {
            match Connections::remove(&app.home.join(".kyotoagent/config.toml"), id) {
                Ok(()) => {
                    app.server_removed = Some(id.clone());
                    app.server_names.remove(id);
                    close_overlay(app);
                    app.notice = Some("Server removed.".into());
                }
                Err(error) => popup.error = Some(error),
            }
        }
        Page::Import => start(app, async move {
            let (uri, info) = crate::pairing::verify(&input, "Kyoto Agent TUI").await?;
            Ok((
                Page::Verified {
                    uri,
                    info: Box::new(info),
                },
                String::new(),
            ))
        }),
        Page::Verified { uri, .. } if index < 2 => {
            match Connections::remember(&app.home.join(".kyotoagent/config.toml"), uri, false) {
                Ok(id) => {
                    close_overlay(app);
                    if index == 0 {
                        app.server_requested = Some(Some(id));
                    }
                }
                Err(error) => popup.error = Some(error),
            }
        }
        Page::Listen => {
            let client = client.clone();
            let server = app.server.clone();
            start(app, async move {
                let value = request_json(
                    &client,
                    "POST",
                    "/v1/https",
                    Some(serde_json::json!({"listen": input})),
                )
                .await?;
                let addr = value["addr"].as_str().ok_or("invalid HTTPS address")?;
                Ok((Page::Host, advertised_host(addr, server.as_deref())))
            });
        }
        Page::Host => {
            let client = client.clone();
            start(app, async move {
                let value = request_json(
                    &client,
                    "POST",
                    "/v1/share",
                    Some(serde_json::json!({"host": input})),
                )
                .await?;
                let uri = value["uri"]
                    .as_str()
                    .ok_or("invalid pairing link")?
                    .to_string();
                let qr =
                    qrcode::QrCode::with_error_correction_level(uri.as_bytes(), qrcode::EcLevel::L)
                        .map_err(|error| error.to_string())?
                        .render::<qrcode::render::unicode::Dense1x2>()
                        .quiet_zone(false)
                        .build();
                Ok((Page::Share { uri, qr }, String::new()))
            });
        }
        Page::Share { uri, .. } if index == 0 => {
            let uri = uri.clone();
            commit_copy(app, &uri);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saved_servers_can_be_renamed_and_removed_without_connecting() {
        let home = std::env::temp_dir().join(format!("ka-manage-servers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join(".kyotoagent/config.toml");
        let first = Connections::remember(
            &path,
            "kyotoagent://first.example:7841?token=first-credential",
            true,
        )
        .unwrap();
        let second = Connections::remember(
            &path,
            "kyotoagent://second.example:7841?token=second-credential",
            false,
        )
        .unwrap();
        let mut app = App::new(home.clone(), home.clone(), String::new());
        app.server = Some(first.clone());
        app.ask = "keep this draft".into();
        let client = Client::at(home.join("unavailable.sock"));
        open_server_picker(&mut app);
        choose_server(&mut app, &client, 6);
        assert!(format!("{:?}", server_overlay(&app)).contains("Rename server"));
        apply(&mut app, &client, Effect::Choose(0)).await.unwrap();
        apply(&mut app, &client, Effect::DeleteLine).await.unwrap();
        apply(&mut app, &client, Effect::Paste("Office".into()))
            .await
            .unwrap();
        apply(&mut app, &client, Effect::Submit).await.unwrap();
        let config: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config["server_names"][&first].as_str(), Some("Office"));
        let saved = Connections::load(&path).unwrap();
        assert_eq!(saved.server.as_deref(), Some(first.as_str()));
        assert_eq!(saved.servers.len(), 2);
        open_server_picker(&mut app);
        let screen = format!("{:?}", server_overlay(&app));
        assert!(screen.contains("Connected to Office"));
        assert!(!screen.contains("credential"));
        choose_server(&mut app, &client, 7);
        apply(&mut app, &client, Effect::Choose(1)).await.unwrap();
        assert!(format!("{:?}", server_overlay(&app)).contains("Remove server"));
        apply(&mut app, &client, Effect::Choose(1)).await.unwrap();
        let saved = Connections::load(&path).unwrap();
        assert!(!saved.servers.contains_key(&second));
        assert_eq!(saved.server.as_deref(), Some(first.as_str()));
        assert_eq!(app.server.as_deref(), Some(first.as_str()));
        assert_eq!(app.ask, "keep this draft");
        assert!(app.server_job.is_none());
        assert!(app.server_requested.is_none());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn removing_the_active_server_requires_confirmation_and_disconnects_immediately() {
        let home = std::env::temp_dir().join(format!("ka-remove-active-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join(".kyotoagent/config.toml");
        let uri = "kyotoagent://127.0.0.1:1?token=expired";
        let id = Connections::remember(&path, uri, true).unwrap();
        let mut app = App::new(home.clone(), home.clone(), String::new());
        app.server = Some(id.clone());
        app.ask = "remote draft".into();
        let mut client = Client::at_url(uri).unwrap();
        open_server_picker(&mut app);
        choose_server(&mut app, &client, 6);
        apply(&mut app, &client, Effect::Choose(0)).await.unwrap();
        assert!(format!("{:?}", server_overlay(&app)).contains("unsent draft"));
        let before = std::fs::read_to_string(&path).unwrap();
        apply(&mut app, &client, Effect::Submit).await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(app.ask, "remote draft");
        assert!(app.server_removed.is_none());
        choose_server(&mut app, &client, 6);
        apply(&mut app, &client, Effect::Choose(0)).await.unwrap();
        apply(&mut app, &client, Effect::Choose(1)).await.unwrap();
        let mut local = App::new(home.clone(), home.clone(), String::new());
        local.ask = "local draft".into();
        let mut cached = BTreeMap::from([
            (None, local),
            (
                Some(id.clone()),
                App::new(home.clone(), home.clone(), String::new()),
            ),
        ]);
        let mut switching = BackgroundSwitch::default();
        let removed = app.server_removed.take().unwrap();
        switching.forget(&mut app, &mut client, &mut cached, &removed);
        assert!(app.server.is_none());
        assert!(client.server_id().is_none());
        assert_eq!(app.ask, "local draft");
        assert!(!cached.contains_key(&Some(id)));
        let saved = Connections::load(&path).unwrap();
        assert!(saved.server.is_none());
        assert!(saved.servers.is_empty());
        drop(switching);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn cancelling_or_rejecting_a_rename_preserves_the_connection_and_draft() {
        let home = std::env::temp_dir().join(format!("ka-rename-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join(".kyotoagent/config.toml");
        let id =
            Connections::remember(&path, "kyotoagent://127.0.0.1:1?token=expired", true).unwrap();
        let mut app = App::new(home.clone(), home.clone(), String::new());
        app.server = Some(id);
        app.ask = "keep this draft".into();
        let client = Client::at(home.join("unused.sock"));
        let before = std::fs::read_to_string(&path).unwrap();
        open_server_picker(&mut app);
        choose_server(&mut app, &client, 5);
        apply(&mut app, &client, Effect::Choose(0)).await.unwrap();
        apply(&mut app, &client, Effect::DeleteLine).await.unwrap();
        apply(&mut app, &client, Effect::Submit).await.unwrap();
        assert!(app.server_popup.as_ref().unwrap().error.is_some());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let effect = key(&app, KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)).unwrap();
        assert_eq!(effect, Effect::Type('2'));
        apply(&mut app, &client, effect).await.unwrap();
        apply(&mut app, &client, Effect::CloseOverlay)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(app.ask, "keep this draft");
        std::fs::remove_dir_all(home).unwrap();
    }

    async fn finish(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.server_job.is_some() {
            advance(app).await;
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn importing_requires_verification_and_confirmation_and_preserves_the_draft() {
        let home = std::env::temp_dir().join(format!("ka-popup-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let (_, task, id) = super::super::tests::saved_test_server(&home, "server").await;
        let path = home.join(".kyotoagent/config.toml");
        let uri = Connections::load(&path).unwrap().servers[&id].clone();
        std::fs::remove_file(&path).unwrap();
        let code = crate::pairing::PairingKey::load(&home.join("server"))
            .unwrap()
            .code()
            .unwrap();
        let pairing_uri = format!("kyotoagent://{id}?token={code}");
        let mut app = App::new(home.clone(), home.clone(), String::new());
        app.ask = "keep this draft".to_string();
        let client = Client::at(home.join("unused.sock"));
        open_server_picker(&mut app);
        choose_server(&mut app, &client, 1);
        apply(&mut app, &client, Effect::Paste(pairing_uri.clone()))
            .await
            .unwrap();
        apply(&mut app, &client, Effect::Submit).await.unwrap();
        assert!(!path.exists());
        finish(&mut app).await;
        assert!(matches!(
            app.server_popup.as_ref().unwrap().page,
            Page::Verified { .. }
        ));
        assert!(!path.exists());
        assert_eq!(app.ask, "keep this draft");
        apply(&mut app, &client, Effect::Choose(1)).await.unwrap();
        let saved = Connections::load(&path).unwrap();
        assert_eq!(saved.servers[&id], uri);
        assert_ne!(saved.servers[&id], pairing_uri);
        assert!(saved.server.is_none());
        assert!(app.server_requested.is_none());
        assert!(app.server_popup.is_none());
        assert_eq!(app.ask, "keep this draft");
        task.abort();
        let _ = task.await;
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn failed_or_abandoned_pairing_does_not_save_a_connection() {
        let home = std::env::temp_dir().join(format!("ka-popup-failure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut app = App::new(home.clone(), home.clone(), String::new());
        app.ask = "draft".to_string();
        let client = Client::at(home.join("unused.sock"));
        open(
            &mut app,
            Page::Import,
            "kyotoagent://127.0.0.1:1?token=private".to_string(),
        );
        assert!(!esc_stops(&app));
        assert!(!format!("{:?}", server_overlay(&app)).contains("private"));
        apply(&mut app, &client, Effect::Submit).await.unwrap();
        finish(&mut app).await;
        assert!(app.server_popup.as_ref().unwrap().error.is_some());
        assert!(!home.join(".kyotoagent/config.toml").exists());
        start(&mut app, async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok((Page::Listen, String::new()))
        });
        apply(&mut app, &client, Effect::CloseOverlay)
            .await
            .unwrap();
        apply(&mut app, &client, Effect::Type('!')).await.unwrap();
        assert_eq!(app.ask, "draft!");
        assert!(app.server_job.is_none());
        assert!(app.server_popup.is_none());
    }

    #[tokio::test]
    async fn sharing_uses_the_selected_server_and_copies_only_on_request() {
        let home = std::env::temp_dir().join(format!("ka-popup-share-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let (_, task, id) = super::super::tests::saved_test_server(&home, "server").await;
        let path = home.join(".kyotoagent/config.toml");
        let uri = Connections::load(&path).unwrap().servers[&id].clone();
        let client = Client::at_url(&uri).unwrap();
        let mut app = App::new(home.clone(), home.clone(), String::new());
        app.server = Some(id.clone());
        app.ask = "remote draft".to_string();
        open_server_picker(&mut app);
        choose_server(&mut app, &client, 3);
        finish(&mut app).await;
        assert!(matches!(
            app.server_popup.as_ref().unwrap().page,
            Page::Host
        ));
        assert_eq!(app.server_popup.as_ref().unwrap().input, id);
        let _capture = capture_copy();
        apply(&mut app, &client, Effect::Submit).await.unwrap();
        finish(&mut app).await;
        assert!(matches!(
            app.server_popup.as_ref().unwrap().page,
            Page::Share { .. }
        ));
        assert!(last_copied().is_none());
        let Page::Share { uri: shared, .. } = &app.server_popup.as_ref().unwrap().page else {
            panic!("pairing link");
        };
        let shared = shared.clone();
        assert_ne!(shared, uri);
        let (_, code) = crate::pairing::connection(&shared).unwrap();
        assert!(crate::pairing::PairingKey::load(&home.join("server"))
            .unwrap()
            .accepts_code(&code.unwrap()));
        apply(&mut app, &client, Effect::Submit).await.unwrap();
        assert_eq!(last_copied(), Some(osc52(&shared)));
        assert!(app.notice.is_none());
        assert_eq!(app.ask, "remote draft");
        close_overlay(&mut app);
        task.abort();
        let _ = task.await;
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn a_response_from_another_server_cannot_change_the_popup() {
        let mut app = App::new(PathBuf::new(), PathBuf::new(), String::new());
        open(&mut app, Page::Import, "current input".to_string());
        start(&mut app, async { Ok((Page::Listen, "stale".to_string())) });
        app.server = Some("other".to_string());
        finish(&mut app).await;
        assert!(matches!(
            app.server_popup.as_ref().unwrap().page,
            Page::Import
        ));
        assert_eq!(app.server_popup.as_ref().unwrap().input, "current input");
    }

    #[tokio::test]
    async fn delayed_switches_keep_input_live_and_preserve_the_latest_draft() {
        let home = std::env::temp_dir().join(format!("ka-popup-delayed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".kyotoagent")).unwrap();
        let listener =
            tokio::net::UnixListener::bind(home.join(".kyotoagent/kyotoagent.sock")).unwrap();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let requested = Arc::new(tokio::sync::Notify::new());
        let held = Arc::clone(&gate);
        let signal = Arc::clone(&requested);
        let task = tokio::spawn(async move {
            let router = axum::Router::new()
                .route(
                    "/v1/sessions",
                    axum::routing::get(move || {
                        let held = Arc::clone(&held);
                        let signal = Arc::clone(&signal);
                        async move {
                            signal.notify_one();
                            let _permit = held.acquire().await.unwrap();
                            axum::Json(serde_json::json!([]))
                        }
                    }),
                )
                .route(
                    "/v1/projects",
                    axum::routing::get(|| async { axum::Json(serde_json::json!([])) }),
                );
            axum::serve(listener, router).await.unwrap();
        });
        let mut app = App::new(home.clone(), home.clone(), String::new());
        app.server = Some("old-server".to_string());
        app.ask = "draft".to_string();
        let mut client = Client::at(home.join("unused.sock"));
        let mut cached = BTreeMap::new();
        let mut switching = BackgroundSwitch::default();
        switching.request(&mut app, &cached, None);
        tokio::time::timeout(Duration::from_secs(5), requested.notified())
            .await
            .unwrap();
        apply(&mut app, &client, Effect::Type('!')).await.unwrap();
        assert_eq!(app.ask, "draft!");
        assert_eq!(app.server.as_deref(), Some("old-server"));
        assert!(!switching.advance(&mut app, &mut client, &mut cached).await);
        gate.add_permits(1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !switching.advance(&mut app, &mut client, &mut cached).await {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(app.server.is_none());
        assert_eq!(cached[&Some("old-server".to_string())].ask, "draft!");
        task.abort();
        let _ = task.await;
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn reconnecting_the_current_server_keeps_new_model_and_session_changes() {
        let home = std::env::temp_dir().join(format!("ka-popup-reconnect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let (_, task, id) = super::super::tests::saved_test_server(&home, "server").await;
        std::fs::write(
            home.join("server/config.toml"),
            format!("base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"unavailable-model\"\n[projects.test]\npath = \"{}\"\n", home.join("server").display()),
        ).unwrap();
        let saved = Connections::load(&home.join(".kyotoagent/config.toml")).unwrap();
        let mut client = Client::at_url(&saved.servers[&id]).unwrap();
        let mut app = App::new(home.clone(), home.clone(), String::new());
        app.server = Some(id.clone());
        poll(&mut app, &client).await.unwrap();
        let mut cached = BTreeMap::new();
        let request = prepare_switch(&app, &cached, Some(id.clone())).unwrap();
        let prepared = fetch_switch(request).await.unwrap();
        assert!(post_model(&mut app, &client, "new-model", Some("high"), None).await);
        finish_switch(&mut app, &mut client, &mut cached, prepared).unwrap();
        assert_eq!(app.model, "new-model");
        assert_eq!(app.effort.as_deref(), Some("high"));
        poll(&mut app, &client).await.unwrap();
        assert_eq!(app.model, "new-model");
        let request = prepare_switch(&app, &cached, Some(id)).unwrap();
        let prepared = fetch_switch(request).await.unwrap();
        let body = serde_json::json!({"workspace": home.join("server")}).to_string();
        let (status, response) = client
            .request("POST", "/v1/sessions", Some(&body))
            .await
            .unwrap();
        assert_eq!(status, 201, "{response}");
        let row: Value = serde_json::from_str(&response).unwrap();
        let selected = row["id"].as_str().unwrap().to_string();
        poll(&mut app, &client).await.unwrap();
        apply(&mut app, &client, Effect::SelectSession(selected.clone()))
            .await
            .unwrap();
        finish_switch(&mut app, &mut client, &mut cached, prepared).unwrap();
        assert_eq!(app.selected, selected);
        poll(&mut app, &client).await.unwrap();
        assert_eq!(app.selected, selected);
        task.abort();
        let _ = task.await;
        std::fs::remove_dir_all(home).unwrap();
    }
}
