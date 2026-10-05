use super::*;
use crate::auth::{self, AuthClient, AuthError, CodexAuth};
use std::collections::BTreeMap;
use std::io;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub id: String,
    pub provider: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub(super) struct Logins {
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
    grok: AuthClient,
    codex: CodexAuth,
}

struct Job {
    progress: Arc<Mutex<Progress>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    cancellation: tokio::sync::Mutex<()>,
}

impl Drop for Job {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().unwrap().take() {
            task.abort();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub id: String,
    pub device_login: bool,
    pub api_key: bool,
    pub configured: bool,
    pub authenticated: bool,
    pub environment_key: bool,
    pub login: Option<Progress>,
}

pub(super) async fn providers(State(state): State<AppState>) -> Json<Vec<ProviderStatus>> {
    let config = state.runner.current_config();
    let mut ids: BTreeMap<String, Option<&crate::config::Provider>> = config
        .providers
        .iter()
        .map(|(id, provider)| (id.clone(), Some(provider)))
        .collect();
    ids.entry("grok".into()).or_default();
    ids.entry("codex".into()).or_default();
    if config.provider.is_none() {
        ids.insert("default".into(), None);
    }
    let jobs = state.logins.jobs.lock().unwrap();
    Json(
        ids.into_iter()
            .map(|(id, provider)| {
                let codex = id == "codex"
                    || provider.is_some_and(|provider| {
                        provider.kind.as_deref() == Some(crate::config::CODEX_KIND)
                    });
                let device_login = id == "grok" || codex;
                let env = provider
                    .and_then(|provider| provider.api_key_env.as_deref())
                    .or_else(|| {
                        (id == "default")
                            .then_some(config.api_key_env.as_deref())
                            .flatten()
                    });
                let configured =
                    provider.is_some() || (id == "default" && config.provider.is_none());
                let key = auth::provider_key_path(
                    &state.root,
                    (id != "default" || provider.is_some()).then_some(id.as_str()),
                );
                let environment_key = env
                    .and_then(|name| std::env::var(name).ok())
                    .is_some_and(|value| !value.trim().is_empty());
                let authenticated = env
                    .and_then(|name| std::env::var(name).ok())
                    .is_some_and(|value| !value.trim().is_empty())
                    || auth::load_opencode_key(&key).is_ok()
                    || (id == "grok" && auth::load(&state.root.join(auth::AUTH_FILE)).is_ok())
                    || (codex && auth::load_codex(&state.root.join(auth::CODEX_AUTH_FILE)).is_ok())
                    || (provider.is_some_and(|provider| {
                        provider.kind.as_deref() == Some(crate::config::OPENCODE_KIND)
                    }) && auth::load_opencode_key(&state.root.join(auth::OPENCODE_AUTH_FILE))
                        .is_ok());
                let login = jobs.values().rev().find_map(|job| {
                    let progress = job.progress.lock().unwrap();
                    ((progress.provider == id || (codex && progress.provider == "codex"))
                        && matches!(progress.status.as_str(), "starting" | "pending"))
                    .then(|| progress.clone())
                });
                ProviderStatus {
                    id,
                    device_login,
                    api_key: configured
                        && !device_login
                        && !provider.is_some_and(|provider| {
                            provider.kind.as_deref() == Some(crate::config::CODEX_KIND)
                        }),
                    configured,
                    authenticated,
                    environment_key,
                    login,
                }
            })
            .collect(),
    )
}

#[derive(Deserialize)]
pub(super) struct ProviderInput {
    id: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
}

pub(super) async fn create_provider(
    State(state): State<AppState>,
    Json(input): Json<ProviderInput>,
) -> Result<StatusCode, ApiError> {
    let id = input.id.trim();
    if !Config::is_provider_id(id) || id.len() > 64 {
        return Err(ApiError::bad_request(
            "Use up to 64 lowercase letters, digits, and dashes for the provider name.",
        ));
    }
    if matches!(id, "default" | "grok" | "codex" | "opencode") {
        return Err(ApiError::bad_request("Choose a different provider name."));
    }
    let base_url = input.base_url.trim();
    if base_url.len() > 2048
        || base_url.chars().any(char::is_control)
        || !reqwest::Url::parse(base_url).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        })
    {
        return Err(ApiError::bad_request(
            "Enter an HTTP or HTTPS base URL without credentials, query, or fragment.",
        ));
    }
    let model = input.model.trim();
    if model.is_empty() || model.len() > 1024 || model.chars().any(char::is_control) {
        return Err(ApiError::bad_request("Enter a valid model ID."));
    }
    let key = input.api_key.as_deref().unwrap_or("").trim();
    if key.len() > 16384 || key.chars().any(char::is_control) {
        return Err(ApiError::bad_request("Enter a valid API key."));
    }
    let _editing = state
        .config_edit
        .lock()
        .map_err(|_| ApiError::server("Configuration is busy."))?;
    if state.runner.current_config().providers.contains_key(id) {
        return Err(ApiError::conflict("Provider name already exists."));
    }
    let config_path = state.root.join(crate::config::CONFIG_FILE);
    Config::edit(&config_path, |doc| {
        let mut provider = toml_edit::InlineTable::new();
        provider.insert("base_url", base_url.into());
        provider.insert("model", model.into());
        doc["providers"][id] = toml_edit::value(provider);
    })
    .map_err(|_| ApiError::server("Server configuration could not be updated."))?;
    if !key.is_empty()
        && auth::write_opencode_key(&auth::provider_key_path(&state.root, Some(id)), key).is_err()
    {
        Config::edit(&config_path, |doc| {
            if let Some(providers) = doc["providers"].as_table_like_mut() {
                providers.remove(id);
            }
        })
        .map_err(|_| ApiError::server("Provider saved, but its API key could not be written."))?;
        return Err(ApiError::server("Server credentials could not be written."));
    }
    Ok(StatusCode::CREATED)
}

#[derive(Deserialize)]
pub(super) struct KeyInput {
    api_key: String,
}

pub(super) async fn save_key(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(input): Json<KeyInput>,
) -> Result<StatusCode, ApiError> {
    let config = state.runner.current_config();
    let provider = config.providers.get(&id);
    let top_level = id == "default" && provider.is_none() && config.provider.is_none();
    if !Config::is_provider_id(&id)
        || !(provider.is_some() || top_level)
        || matches!(id.as_str(), "grok" | "codex")
        || provider
            .is_some_and(|provider| provider.kind.as_deref() == Some(crate::config::CODEX_KIND))
    {
        return Err(ApiError::bad_request("provider does not support API keys"));
    }
    let key = input.api_key.trim();
    if key.is_empty() || key.len() > 16384 || key.chars().any(char::is_control) {
        return Err(ApiError::bad_request("enter a valid API key"));
    }
    auth::write_opencode_key(
        &auth::provider_key_path(&state.root, (!top_level).then_some(id.as_str())),
        key,
    )
    .map_err(|_| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: "server credentials could not be written".into(),
    })?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn cancel(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Progress>, ApiError> {
    let job = state
        .logins
        .jobs
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            message: "no such login".into(),
        })?;
    let _cancellation = job.cancellation.lock().await;
    let task = job.task.lock().unwrap().take();
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
    }
    let mut progress = job.progress.lock().unwrap();
    if matches!(progress.status.as_str(), "starting" | "pending") {
        progress.status = "cancelled".into();
        progress.verification_url = None;
        progress.user_code = None;
    }
    Ok(Json(progress.clone()))
}

impl Logins {
    pub(super) fn new(grok: AuthClient, codex: CodexAuth) -> Self {
        Self {
            jobs: Mutex::new(BTreeMap::new()),
            grok,
            codex,
        }
    }
}

struct Challenge {
    progress: Arc<Mutex<Progress>>,
    bytes: Vec<u8>,
}

impl Write for Challenge {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len() + bytes.len() > 8192 {
            return Err(io::Error::other("device challenge is too long"));
        }
        self.bytes.extend_from_slice(bytes);
        if self.bytes.last() == Some(&b'\n') {
            let text = String::from_utf8_lossy(&self.bytes);
            let mut lines = text.lines();
            let mut progress = self.progress.lock().unwrap();
            progress.verification_url = lines.next().map(str::to_string);
            progress.user_code = lines.next().map(str::to_string);
            progress.status = "pending".into();
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn safe_error(error: AuthError) -> String {
    match error {
        AuthError::Status { status, .. } => format!("provider answered HTTP {status}"),
        AuthError::Denied => "device login denied".into(),
        AuthError::Expired | AuthError::TimedOut => "device code expired".into(),
        AuthError::Transport(_) => "provider could not be reached".into(),
        AuthError::Config(_) => "server configuration could not be updated".into(),
        AuthError::Io { .. } => "server credentials could not be written".into(),
        AuthError::InvalidUri { .. } => "provider returned an invalid verification URL".into(),
        AuthError::BadAccount => "provider returned no valid account".into(),
        AuthError::NeedLogin => "provider returned incomplete credentials".into(),
        _ => "device login failed".into(),
    }
}

pub(super) async fn start(
    State(state): State<AppState>,
    AxumPath(provider): AxumPath<String>,
) -> Result<(StatusCode, Json<Progress>), ApiError> {
    let provider = if state
        .runner
        .current_config()
        .providers
        .get(&provider)
        .is_some_and(|provider| provider.kind.as_deref() == Some(crate::config::CODEX_KIND))
    {
        "codex".into()
    } else {
        provider
    };
    if !matches!(provider.as_str(), "grok" | "codex") {
        return Err(ApiError::bad_request("unknown login provider"));
    }
    let progress = Progress {
        id: generate_id(),
        provider,
        status: "starting".into(),
        verification_url: None,
        user_code: None,
        error: None,
    };
    let job = Arc::new(Job {
        progress: Arc::new(Mutex::new(progress.clone())),
        task: Mutex::new(None),
        cancellation: tokio::sync::Mutex::new(()),
    });
    let mut task_slot = job.task.lock().unwrap();
    {
        let mut jobs = state.logins.jobs.lock().unwrap();
        if jobs.values().any(|job| {
            matches!(
                job.progress.lock().unwrap().status.as_str(),
                "starting" | "pending"
            )
        }) {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                message: "a provider login is already running".into(),
            });
        }
        if jobs.len() >= 8 {
            jobs.clear();
        }
        jobs.insert(progress.id.clone(), Arc::clone(&job));
    }
    let task_progress = Arc::clone(&job.progress);
    let root = state.root.clone();
    let codex = state.logins.codex.clone();
    let grok = state.logins.grok.clone();
    let task = tokio::spawn(async move {
        let config = root.join(crate::config::CONFIG_FILE);
        let mut out = Challenge {
            progress: Arc::clone(&task_progress),
            bytes: Vec::new(),
        };
        let provider = task_progress.lock().unwrap().provider.clone();
        let login = async {
            match provider.as_str() {
                "codex" => {
                    auth::login_codex(&codex, &config, &root.join(auth::CODEX_AUTH_FILE), &mut out)
                        .await
                }
                _ => auth::login(&grok, &config, &root.join(auth::AUTH_FILE), &mut out).await,
            }?;
            fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).map_err(|source| {
                AuthError::Io {
                    path: config.clone(),
                    source,
                }
            })
        };
        let result = tokio::time::timeout(Duration::from_secs(900), login).await;
        let mut progress = task_progress.lock().unwrap();
        match result {
            Ok(Ok(())) => progress.status = "complete".into(),
            Ok(Err(error)) => {
                progress.status = "failed".into();
                progress.error = Some(safe_error(error));
            }
            Err(_) => {
                progress.status = "failed".into();
                progress.error = Some("device code expired".into());
            }
        }
    });
    *task_slot = Some(task);
    Ok((StatusCode::ACCEPTED, Json(progress)))
}

pub(super) async fn status(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Progress>, ApiError> {
    let jobs = state.logins.jobs.lock().unwrap();
    let job = jobs.get(&id).ok_or_else(|| ApiError {
        status: StatusCode::NOT_FOUND,
        message: "no such login".into(),
    })?;
    let progress = job.progress.lock().unwrap().clone();
    Ok(Json(progress))
}
