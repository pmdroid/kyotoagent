use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, ConfigError};
use crate::events::format_millis;

pub const AUTH_FILE: &str = "auth.json";
pub const DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const DEFAULT_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
pub const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
pub const GROK_PROVIDER: &str = "grok";
pub const GROK_BASE_URL: &str = "https://api.x.ai/v1";
pub const GROK_MODEL: &str = "grok-4.6";
pub const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
pub const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";

const DEFAULT_INTERVAL_SECS: u64 = 5;
const SLOW_DOWN_SECS: u64 = 5;
const DEFAULT_EXPIRES_IN: u64 = 3600;
const REFRESH_SKEW_SECS: u64 = 60;
const REQUEST_TIMEOUT_SECS: u64 = 30;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: String,
}

#[derive(Clone, Debug)]
pub struct AuthClient {
    http: reqwest::Client,
    device_url: String,
    token_url: String,
    client_id: String,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenOk {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenErr {
    error: String,
}

#[derive(Debug)]
pub enum AuthError {
    Transport(reqwest::Error),
    Status { status: u16, body: String },
    Decode(serde_json::Error),
    Denied,
    Expired,
    InvalidUri { uri: String },
    Io { path: PathBuf, source: io::Error },
    Output(io::Error),
    Config(ConfigError),
    NeedLogin,
    TimedOut,
    BadAccount,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Transport(source) => {
                write!(f, "the login server could not be reached: {source}")
            }
            AuthError::Status { status, body } => {
                let detail = body.trim();
                if detail.is_empty() {
                    write!(f, "the login server answered {status}")
                } else {
                    write!(f, "the login server answered {status}: {detail}")
                }
            }
            AuthError::Decode(source) => {
                write!(f, "the login server's answer could not be read: {source}")
            }
            AuthError::Denied => write!(f, "authorization was denied"),
            AuthError::Expired => write!(f, "the login code expired"),
            AuthError::InvalidUri { uri } => {
                write!(f, "the verification URL must be https: {uri}")
            }
            AuthError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            AuthError::Output(source) => write!(f, "{source}"),
            AuthError::Config(source) => write!(f, "{source}"),
            AuthError::NeedLogin => write!(f, "Open Providers in Kyoto Agent to sign in."),
            AuthError::TimedOut => write!(f, "device auth timed out after 15 minutes"),
            AuthError::BadAccount => write!(f, "the login server's answer had no account id"),
        }
    }
}

impl std::error::Error for AuthError {}

impl AuthClient {
    pub fn new(client_id: &str) -> AuthClient {
        AuthClient::at(DEVICE_CODE_URL, TOKEN_URL, client_id)
    }

    pub fn at(device_url: &str, token_url: &str, client_id: &str) -> AuthClient {
        AuthClient {
            http: reqwest::Client::new(),
            device_url: device_url.to_string(),
            token_url: token_url.to_string(),
            client_id: client_id.to_string(),
        }
    }

    async fn post_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
    ) -> Result<(u16, String), AuthError> {
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(encode_form(form))
            .send()
            .await
            .map_err(AuthError::Transport)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(AuthError::Transport)?;
        Ok((status, text))
    }

    pub async fn refresh(&self, path: &Path, current: &Tokens) -> Result<Tokens, AuthError> {
        let (status, text) = self
            .post_form(
                &self.token_url,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", self.client_id.as_str()),
                    ("refresh_token", current.refresh_token.as_str()),
                ],
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(AuthError::NeedLogin);
        }
        let body: TokenOk = serde_json::from_str(&text).map_err(AuthError::Decode)?;
        let tokens = tokens_from_ok(body, Some(current))?;
        write_tokens(path, &tokens)?;
        Ok(tokens)
    }
}

pub fn default_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = PathBuf::from(home);
    if home.as_os_str().is_empty() {
        None
    } else {
        Some(home.join(".kyotoagent"))
    }
}

pub fn default_path() -> Option<PathBuf> {
    Some(default_root()?.join(AUTH_FILE))
}

pub fn load(path: &Path) -> Result<Tokens, AuthError> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(AuthError::Decode),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Err(AuthError::NeedLogin),
        Err(source) => Err(AuthError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn logout(path: &Path) -> Result<(), AuthError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AuthError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn write_tokens(path: &Path, tokens: &Tokens) -> Result<(), AuthError> {
    let body = serde_json::to_vec_pretty(tokens).map_err(AuthError::Decode)?;
    write_bytes(path, &body)
}

pub fn is_fresh(tokens: &Tokens) -> bool {
    fresh_stamp(&tokens.expires_at)
}

fn fresh_stamp(expires_at: &str) -> bool {
    let skew = now_millis().saturating_add(REFRESH_SKEW_SECS.saturating_mul(1000));
    expires_at > format_millis(skew as i64).as_str()
}

async fn refresh_lock(path: &Path) -> Result<fs::File, AuthError> {
    let mut name = path.as_os_str().to_os_string();
    name.push(".refresh.lock");
    let path = PathBuf::from(name);
    let error_path = path.clone();
    tokio::task::spawn_blocking(move || {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|source| AuthError::Io {
                path: path.clone(),
                source,
            })?;
        file.lock()
            .map_err(|source| AuthError::Io { path, source })?;
        Ok(file)
    })
    .await
    .map_err(|source| AuthError::Io {
        path: error_path,
        source: io::Error::other(source),
    })?
}

pub async fn access_token(client: &AuthClient, path: &Path) -> Result<String, AuthError> {
    let tokens = load(path)?;
    if is_fresh(&tokens) {
        return Ok(tokens.access_token);
    }
    let _lock = refresh_lock(path).await?;
    let tokens = load(path)?;
    if is_fresh(&tokens) {
        return Ok(tokens.access_token);
    }
    let tokens = client.refresh(path, &tokens).await?;
    Ok(tokens.access_token)
}

pub async fn access_token_after_401(
    client: &AuthClient,
    path: &Path,
    rejected: &str,
) -> Result<String, AuthError> {
    let _lock = refresh_lock(path).await?;
    let tokens = load(path)?;
    if tokens.access_token != rejected && is_fresh(&tokens) {
        return Ok(tokens.access_token);
    }
    let tokens = client.refresh(path, &tokens).await?;
    Ok(tokens.access_token)
}

pub async fn login(
    client: &AuthClient,
    config_path: &Path,
    auth_path: &Path,
    out: &mut impl Write,
) -> Result<(), AuthError> {
    let (status, text) = client
        .post_form(
            &client.device_url,
            &[
                ("client_id", client.client_id.as_str()),
                ("scope", DEFAULT_SCOPE),
                ("referrer", "kyotoagent"),
            ],
        )
        .await?;
    if !(200..300).contains(&status) {
        return Err(AuthError::Status { status, body: text });
    }
    let device: DeviceCodeResponse = serde_json::from_str(&text).map_err(AuthError::Decode)?;
    require_https(&device.verification_uri)?;
    let complete = device
        .verification_uri_complete
        .as_deref()
        .map(str::trim)
        .filter(|uri| !uri.is_empty());
    if let Some(uri) = complete {
        require_https(uri)?;
        writeln!(out, "{uri}").map_err(AuthError::Output)?;
    } else {
        writeln!(out, "{}", device.verification_uri).map_err(AuthError::Output)?;
        writeln!(out, "{}", device.user_code).map_err(AuthError::Output)?;
    }
    let mut interval = device.interval.unwrap_or(DEFAULT_INTERVAL_SECS);
    loop {
        if interval > 0 {
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
        let (status, text) = client
            .post_form(
                &client.token_url,
                &[
                    ("grant_type", DEVICE_GRANT_TYPE),
                    ("client_id", client.client_id.as_str()),
                    ("device_code", device.device_code.as_str()),
                ],
            )
            .await?;
        if (200..300).contains(&status) {
            let body: TokenOk = serde_json::from_str(&text).map_err(AuthError::Decode)?;
            let tokens = tokens_from_ok(body, None)?;
            write_tokens(auth_path, &tokens)?;
            select_grok(config_path)?;
            return Ok(());
        }
        let error = serde_json::from_str::<TokenErr>(&text)
            .ok()
            .map(|body| body.error);
        match error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => {
                interval = interval.saturating_add(SLOW_DOWN_SECS);
            }
            Some("access_denied") => return Err(AuthError::Denied),
            Some("expired_token") => return Err(AuthError::Expired),
            _ => {
                return Err(AuthError::Status { status, body: text });
            }
        }
    }
}

fn select_grok(config_path: &Path) -> Result<(), AuthError> {
    match Config::load(config_path) {
        Ok(config) if config.providers.contains_key(GROK_PROVIDER) => {
            Config::use_provider(config_path, GROK_PROVIDER).map_err(AuthError::Config)?;
        }
        Ok(_) => {
            Config::add_provider(config_path, GROK_PROVIDER, GROK_BASE_URL, GROK_MODEL, None)
                .map_err(AuthError::Config)?;
        }
        Err(ConfigError::Io { ref source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Config::add_provider(config_path, GROK_PROVIDER, GROK_BASE_URL, GROK_MODEL, None)
                .map_err(AuthError::Config)?;
        }
        Err(error) => return Err(AuthError::Config(error)),
    }
    Config::write_missing_title_model(config_path).map_err(AuthError::Config)
}

fn tokens_from_ok(body: TokenOk, previous: Option<&Tokens>) -> Result<Tokens, AuthError> {
    let refresh_token = body
        .refresh_token
        .filter(|token| !token.is_empty())
        .or_else(|| previous.map(|tokens| tokens.refresh_token.clone()))
        .unwrap_or_default();
    if refresh_token.is_empty() {
        return Err(AuthError::NeedLogin);
    }
    let expires_in = body.expires_in.unwrap_or(DEFAULT_EXPIRES_IN);
    let expires_at =
        format_millis(now_millis().saturating_add(expires_in.saturating_mul(1000)) as i64);
    Ok(Tokens {
        access_token: body.access_token,
        refresh_token,
        expires_at,
    })
}

fn require_https(uri: &str) -> Result<(), AuthError> {
    if uri.starts_with("https:") && !uri.chars().any(|c| c.is_ascii_control()) {
        Ok(())
    } else {
        Err(AuthError::InvalidUri {
            uri: uri.to_string(),
        })
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn encode_form(form: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (key, value)) in form.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&encode_form_component(key));
        out.push('=');
        out.push_str(&encode_form_component(value));
    }
    out
}

fn encode_form_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub const CODEX_AUTH_FILE: &str = "codex-auth.json";
pub const OPENCODE_AUTH_FILE: &str = "opencode-auth.json";
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_ISSUER: &str = "https://auth.openai.com";
pub const CODEX_ORIGINATOR: &str = "codex_cli_rs";
pub const CODEX_CLIENT_VERSION: &str = "0.160.0";
pub const CODEX_AUTH_CLAIM: &str = "https://api.openai.com/auth";

const CODEX_POLL_LIMIT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodexTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub account_id: String,
    pub expires_at: String,
}

#[derive(Clone, Debug)]
pub struct CodexAuth {
    http: reqwest::Client,
    issuer: String,
    client_id: String,
}

#[derive(Debug, Deserialize)]
struct UserCodeBody {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(default)]
    interval: Option<IntervalValue>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum IntervalValue {
    Num(u64),
    Text(String),
}

#[derive(Debug, Deserialize)]
struct DeviceGrant {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Debug, Deserialize)]
struct CodexTokenBody {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

impl CodexAuth {
    pub fn at(issuer: &str) -> CodexAuth {
        CodexAuth {
            http: reqwest::Client::new(),
            issuer: issuer.trim_end_matches('/').to_string(),
            client_id: CODEX_CLIENT_ID.to_string(),
        }
    }

    pub async fn refresh(
        &self,
        path: &Path,
        current: &CodexTokens,
    ) -> Result<CodexTokens, AuthError> {
        let url = format!("{}/oauth/token", self.issuer);
        let payload = serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": &self.client_id,
            "refresh_token": &current.refresh_token,
        });
        let (status, text) = self.post_json(&url, &payload).await?;
        if !(200..300).contains(&status) {
            return Err(AuthError::NeedLogin);
        }
        let body: CodexTokenBody = serde_json::from_str(&text).map_err(AuthError::Decode)?;
        let tokens = codex_tokens_from(body, Some(current))?;
        write_codex_tokens(path, &tokens)?;
        Ok(tokens)
    }

    async fn post_json(
        &self,
        url: &str,
        body: &impl Serialize,
    ) -> Result<(u16, String), AuthError> {
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .json(body)
            .send()
            .await
            .map_err(AuthError::Transport)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(AuthError::Transport)?;
        Ok((status, text))
    }

    async fn post_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
    ) -> Result<(u16, String), AuthError> {
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(encode_form(form))
            .send()
            .await
            .map_err(AuthError::Transport)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(AuthError::Transport)?;
        Ok((status, text))
    }
}

pub fn default_codex_path() -> Option<PathBuf> {
    Some(default_root()?.join(CODEX_AUTH_FILE))
}

pub fn default_opencode_path() -> Option<PathBuf> {
    Some(default_root()?.join(OPENCODE_AUTH_FILE))
}

pub fn load_opencode_key(path: &Path) -> Result<String, AuthError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(AuthError::NeedLogin);
        }
        Err(source) => {
            return Err(AuthError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let value: Value = serde_json::from_str(&text).map_err(AuthError::Decode)?;
    let key = value
        .get("api_key")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if key.is_empty() {
        Err(AuthError::NeedLogin)
    } else {
        Ok(key.to_string())
    }
}

pub fn write_opencode_key(path: &Path, key: &str) -> Result<(), AuthError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(AuthError::NeedLogin);
    }
    let body = serde_json::to_vec_pretty(&serde_json::json!({ "api_key": key }))
        .map_err(AuthError::Decode)?;
    write_bytes(path, &body)
}

pub fn load_codex(path: &Path) -> Result<CodexTokens, AuthError> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(AuthError::Decode),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Err(AuthError::NeedLogin),
        Err(source) => Err(AuthError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn write_codex_tokens(path: &Path, tokens: &CodexTokens) -> Result<(), AuthError> {
    let body = serde_json::to_vec_pretty(tokens).map_err(AuthError::Decode)?;
    write_bytes(path, &body)
}

fn write_bytes(path: &Path, body: &[u8]) -> Result<(), AuthError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| AuthError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    static NEXT_WRITE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_WRITE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{}.{}.tmp", std::process::id(), nonce, sequence));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|source| AuthError::Io {
            path: tmp.clone(),
            source,
        })?;
    let result = (|| {
        file.write_all(body)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&tmp);
        return Err(AuthError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

pub fn provider_key_path(root: &Path, provider: Option<&str>) -> PathBuf {
    root.join(match provider {
        Some(id) => format!("api-key-{id}.json"),
        None => "api-key.json".to_string(),
    })
}

pub fn stored_provider_key(
    config: &Config,
    root: Option<&Path>,
) -> Result<Option<String>, AuthError> {
    if let Some(key) = config.api_key() {
        return Ok(Some(key));
    }
    let root = root
        .map(Path::to_path_buf)
        .or_else(|| Config::default_path().and_then(|path| path.parent().map(Path::to_path_buf)));
    let Some(root) = root else {
        return Ok(None);
    };
    let path = provider_key_path(&root, config.provider.as_deref());
    match load_opencode_key(&path) {
        Ok(key) => Ok(Some(key)),
        Err(AuthError::NeedLogin) if !path.exists() => Ok(None),
        Err(error) => Err(error),
    }
}

pub async fn codex_access(client: &CodexAuth, path: &Path) -> Result<String, AuthError> {
    let tokens = load_codex(path)?;
    if fresh_stamp(&tokens.expires_at) {
        return Ok(tokens.access_token);
    }
    let _lock = refresh_lock(path).await?;
    let tokens = load_codex(path)?;
    if fresh_stamp(&tokens.expires_at) {
        return Ok(tokens.access_token);
    }
    let tokens = client.refresh(path, &tokens).await?;
    Ok(tokens.access_token)
}

pub async fn codex_access_after_401(
    client: &CodexAuth,
    path: &Path,
    rejected: &str,
) -> Result<String, AuthError> {
    let _lock = refresh_lock(path).await?;
    let tokens = load_codex(path)?;
    if tokens.access_token != rejected && fresh_stamp(&tokens.expires_at) {
        return Ok(tokens.access_token);
    }
    let tokens = client.refresh(path, &tokens).await?;
    Ok(tokens.access_token)
}

pub async fn login_codex(
    client: &CodexAuth,
    config_path: &Path,
    auth_path: &Path,
    out: &mut impl Write,
) -> Result<(), AuthError> {
    let usercode_url = format!("{}/api/accounts/deviceauth/usercode", client.issuer);
    let (status, text) = client
        .post_json(
            &usercode_url,
            &serde_json::json!({ "client_id": &client.client_id }),
        )
        .await?;
    if !(200..300).contains(&status) {
        return Err(AuthError::Status { status, body: text });
    }
    let started: UserCodeBody = serde_json::from_str(&text).map_err(AuthError::Decode)?;
    writeln!(out, "{}/codex/device", client.issuer).map_err(AuthError::Output)?;
    writeln!(out, "{}", started.user_code).map_err(AuthError::Output)?;
    let interval = interval_of(started.interval.as_ref());
    let poll_url = format!("{}/api/accounts/deviceauth/token", client.issuer);
    let started_at = Instant::now();
    let grant = loop {
        if started_at.elapsed() >= CODEX_POLL_LIMIT {
            return Err(AuthError::TimedOut);
        }
        let (status, text) = client
            .post_json(
                &poll_url,
                &serde_json::json!({
                    "device_auth_id": &started.device_auth_id,
                    "user_code": &started.user_code,
                }),
            )
            .await?;
        if (200..300).contains(&status) {
            let grant = serde_json::from_str::<DeviceGrant>(&text).map_err(AuthError::Decode)?;
            break grant;
        }
        if status == 403 || status == 404 {
            let pause = Duration::from_secs(interval);
            match started_at.elapsed().checked_add(pause) {
                Some(waited) if waited < CODEX_POLL_LIMIT => {
                    if interval > 0 {
                        tokio::time::sleep(pause).await;
                    }
                }
                _ => return Err(AuthError::TimedOut),
            }
            continue;
        }
        return Err(AuthError::Status { status, body: text });
    };
    let token_url = format!("{}/oauth/token", client.issuer);
    let redirect = format!("{}/deviceauth/callback", client.issuer);
    let (status, text) = client
        .post_form(
            &token_url,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", client.client_id.as_str()),
                ("code", grant.authorization_code.as_str()),
                ("redirect_uri", redirect.as_str()),
                ("code_verifier", grant.code_verifier.as_str()),
            ],
        )
        .await?;
    if !(200..300).contains(&status) {
        return Err(AuthError::Status { status, body: text });
    }
    let body: CodexTokenBody = serde_json::from_str(&text).map_err(AuthError::Decode)?;
    let tokens = codex_tokens_from(body, None)?;
    write_codex_tokens(auth_path, &tokens)?;
    select_codex(config_path)?;
    Ok(())
}

fn select_codex(config_path: &Path) -> Result<(), AuthError> {
    Config::write_codex_table(config_path).map_err(AuthError::Config)?;
    Config::write_missing_title_model(config_path).map_err(AuthError::Config)
}

fn interval_of(value: Option<&IntervalValue>) -> u64 {
    let seconds = match value {
        Some(IntervalValue::Num(seconds)) => *seconds,
        Some(IntervalValue::Text(text)) => text.trim().parse().unwrap_or(DEFAULT_INTERVAL_SECS),
        None => DEFAULT_INTERVAL_SECS,
    };
    seconds.min(60)
}

fn codex_tokens_from(
    body: CodexTokenBody,
    previous: Option<&CodexTokens>,
) -> Result<CodexTokens, AuthError> {
    let refresh_token = body
        .refresh_token
        .filter(|token| !token.is_empty())
        .or_else(|| previous.map(|tokens| tokens.refresh_token.clone()))
        .filter(|token| !token.is_empty())
        .ok_or(AuthError::NeedLogin)?;
    let replaced = matches!(body.id_token.as_deref(), Some(token) if !token.is_empty());
    let id_token = body
        .id_token
        .filter(|token| !token.is_empty())
        .or_else(|| previous.map(|tokens| tokens.id_token.clone()))
        .filter(|token| !token.is_empty())
        .ok_or(AuthError::BadAccount)?;
    let account_id = match account_id_of(&id_token) {
        Ok(id) => id,
        Err(error) => match previous {
            Some(tokens) if !replaced => tokens.account_id.clone(),
            _ => return Err(error),
        },
    };
    if body.access_token.is_empty() {
        return Err(AuthError::NeedLogin);
    }
    let expires_at = codex_expiry(body.expires_in, &id_token);
    Ok(CodexTokens {
        access_token: body.access_token,
        refresh_token,
        id_token,
        account_id,
        expires_at,
    })
}

fn codex_expiry(expires_in: Option<u64>, id_token: &str) -> String {
    if let Some(seconds) = expires_in {
        return format_millis(now_millis().saturating_add(seconds.saturating_mul(1000)) as i64);
    }
    if let Some(exp) = jwt_exp(id_token) {
        return format_millis(exp.saturating_mul(1000) as i64);
    }
    format_millis(now_millis().saturating_add(DEFAULT_EXPIRES_IN.saturating_mul(1000)) as i64)
}

fn account_id_of(id_token: &str) -> Result<String, AuthError> {
    let payload = jwt_payload(id_token)?;
    let from_claim = payload
        .get(CODEX_AUTH_CLAIM)
        .and_then(|value| value.get("chatgpt_account_id"))
        .and_then(Value::as_str);
    let from_top = payload.get("chatgpt_account_id").and_then(Value::as_str);
    from_claim
        .or(from_top)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or(AuthError::BadAccount)
}

fn jwt_payload(token: &str) -> Result<Value, AuthError> {
    let part = token.split('.').nth(1).ok_or(AuthError::BadAccount)?;
    let bytes = b64url_decode(part)?;
    serde_json::from_slice(&bytes).map_err(|_| AuthError::BadAccount)
}

fn jwt_exp(token: &str) -> Option<u64> {
    let payload = jwt_payload(token).ok()?;
    match payload.get("exp")? {
        Value::Number(number) => number.as_u64().or_else(|| {
            number
                .as_i64()
                .filter(|value| *value >= 0)
                .map(|value| value as u64)
        }),
        _ => None,
    }
}

fn b64url_decode(input: &str) -> Result<Vec<u8>, AuthError> {
    fn val(byte: u8) -> Result<u8, AuthError> {
        match byte {
            b'A'..=b'Z' => Ok(byte - b'A'),
            b'a'..=b'z' => Ok(byte - b'a' + 26),
            b'0'..=b'9' => Ok(byte - b'0' + 52),
            b'+' | b'-' => Ok(62),
            b'/' | b'_' => Ok(63),
            _ => Err(AuthError::BadAccount),
        }
    }
    let cleaned: Vec<u8> = input.bytes().filter(|byte| *byte != b'=').collect();
    if cleaned.is_empty() {
        return Err(AuthError::BadAccount);
    }
    let mut out = Vec::new();
    let mut index = 0;
    while index < cleaned.len() {
        let remaining = cleaned.len() - index;
        if remaining == 1 {
            return Err(AuthError::BadAccount);
        }
        let first = val(cleaned[index])?;
        let second = val(cleaned[index + 1])?;
        out.push((first << 2) | (second >> 4));
        if remaining == 2 {
            break;
        }
        let third = val(cleaned[index + 2])?;
        out.push((second << 4) | (third >> 2));
        if remaining == 3 {
            break;
        }
        let fourth = val(cleaned[index + 3])?;
        out.push((third << 6) | fourth);
        index += 4;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write as IoWrite};
    use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    #[derive(Clone, Debug)]
    struct Received {
        method: String,
        path: String,
        body: String,
    }

    impl Received {
        fn form(&self) -> Vec<(String, String)> {
            let mut pairs = Vec::new();
            for part in self.body.split('&') {
                if part.is_empty() {
                    continue;
                }
                let mut pieces = part.splitn(2, '=');
                let key = pieces.next().unwrap_or("");
                let value = pieces.next().unwrap_or("");
                pairs.push((decode(key), decode(value)));
            }
            pairs
        }

        fn form_value(&self, name: &str) -> Option<String> {
            self.form()
                .into_iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        }
    }

    fn decode(value: &str) -> String {
        let plus = value.replace('+', " ");
        let bytes = plus.into_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = &plus_slice(&bytes, i + 1);
                if let Ok(n) = u8::from_str_radix(hex, 16) {
                    out.push(n);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn plus_slice(bytes: &[u8], at: usize) -> String {
        String::from_utf8_lossy(&bytes[at..at + 2]).into_owned()
    }

    #[derive(Clone)]
    enum Canned {
        Json(u16, String),
    }

    struct FakeServer {
        addr: SocketAddr,
        received: Arc<Mutex<Vec<Received>>>,
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl FakeServer {
        fn start(device: Vec<Canned>, token: Vec<Canned>) -> FakeServer {
            let listener = TcpListener::bind("127.0.0.1:0").expect("the fake server binds");
            listener
                .set_nonblocking(true)
                .expect("the listener does not block");
            let addr = listener
                .local_addr()
                .expect("the fake server has an address");
            let received = Arc::new(Mutex::new(Vec::new()));
            let device = Arc::new(Mutex::new(device));
            let token = Arc::new(Mutex::new(token));
            let stop = Arc::new(AtomicBool::new(false));
            let handle = {
                let received = Arc::clone(&received);
                let device = Arc::clone(&device);
                let token = Arc::clone(&token);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                let _ = stream.set_nonblocking(false);
                                serve_one(stream, &received, &device, &token);
                            }
                            Err(ref source) if source.kind() == io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            Err(_) => break,
                        }
                    }
                })
            };
            FakeServer {
                addr,
                received,
                stop,
                handle: Some(handle),
            }
        }

        fn device_url(&self) -> String {
            format!("http://{}/oauth2/device/code", self.addr)
        }

        fn token_url(&self) -> String {
            format!("http://{}/oauth2/token", self.addr)
        }

        fn received(&self) -> Vec<Received> {
            self.received
                .lock()
                .expect("the log is not poisoned")
                .clone()
        }

        fn token_requests(&self) -> Vec<Received> {
            self.received()
                .into_iter()
                .filter(|request| request.path == "/oauth2/token")
                .collect()
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn serve_one(
        mut stream: TcpStream,
        received: &Arc<Mutex<Vec<Received>>>,
        device: &Arc<Mutex<Vec<Canned>>>,
        token: &Arc<Mutex<Vec<Canned>>>,
    ) {
        let Some(request) = read_request(&mut stream) else {
            return;
        };
        let queue = if request.path == "/oauth2/device/code" {
            device
        } else {
            token
        };
        received
            .lock()
            .expect("the log is not poisoned")
            .push(request);
        let reply = {
            let mut queue = queue.lock().expect("the queue is not poisoned");
            if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue
                    .first()
                    .cloned()
                    .unwrap_or(Canned::Json(500, String::new()))
            }
        };
        match reply {
            Canned::Json(status, body) => respond(&mut stream, status, &body),
        }
    }

    fn read_request(stream: &mut TcpStream) -> Option<Received> {
        let mut raw = Vec::new();
        let mut chunk = [0_u8; 1024];
        let head_end = loop {
            if let Some(at) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                break at;
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(n) => raw.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
        let mut lines = head.lines();
        let start = lines.next()?;
        let mut parts = start.split_whitespace();
        let method = parts.next()?.to_string();
        let path = parts.next()?.to_string();
        let mut length = 0usize;
        for line in lines {
            if let Some((key, value)) = line.split_once(':') {
                if key.trim().eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = raw[head_end + 4..].to_vec();
        while body.len() < length {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
            }
        }
        Some(Received {
            method,
            path,
            body: String::from_utf8_lossy(&body).to_string(),
        })
    }

    fn respond(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            _ => "Error",
        };
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Both);
    }

    fn device_ok() -> Canned {
        Canned::Json(
            200,
            serde_json::json!({
                "device_code": "device-1",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://example.test/activate",
                "verification_uri_complete": "https://example.test/activate?user_code=WDJB-MJHT",
                "expires_in": 600,
                "interval": 0
            })
            .to_string(),
        )
    }

    fn device_split() -> Canned {
        Canned::Json(
            200,
            serde_json::json!({
                "device_code": "device-1",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://example.test/activate",
                "expires_in": 600,
                "interval": 0
            })
            .to_string(),
        )
    }

    fn pending() -> Canned {
        Canned::Json(
            400,
            serde_json::json!({ "error": "authorization_pending" }).to_string(),
        )
    }

    fn denied() -> Canned {
        Canned::Json(
            400,
            serde_json::json!({ "error": "access_denied" }).to_string(),
        )
    }

    fn expired() -> Canned {
        Canned::Json(
            400,
            serde_json::json!({ "error": "expired_token" }).to_string(),
        )
    }

    fn token_ok(access: &str, refresh: &str) -> Canned {
        Canned::Json(
            200,
            serde_json::json!({
                "access_token": access,
                "refresh_token": refresh,
                "expires_in": 3600
            })
            .to_string(),
        )
    }

    fn client_for(server: &FakeServer) -> AuthClient {
        AuthClient::at(&server.device_url(), &server.token_url(), DEFAULT_CLIENT_ID)
    }

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kyotoagent-auth-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the temp dir exists");
        dir
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path)
            .expect("the file exists")
            .permissions()
            .mode()
            & 0o777
    }

    #[tokio::test]
    async fn login_prints_the_url_writes_the_file_and_selects_grok() {
        let server = FakeServer::start(vec![device_ok()], vec![token_ok("access-1", "refresh-1")]);
        let root = temp_root("login-ok");
        let config_path = root.join("config.toml");
        let auth_path = root.join(AUTH_FILE);
        let mut out = Vec::new();
        login(&client_for(&server), &config_path, &auth_path, &mut out)
            .await
            .expect("login succeeds");
        let printed = String::from_utf8(out).expect("stdout is text");
        assert!(
            printed.contains("https://example.test/activate?user_code=WDJB-MJHT"),
            "{printed}"
        );
        assert!(auth_path.exists(), "auth.json is written");
        assert_eq!(mode_of(&auth_path), 0o600);
        let tokens = load(&auth_path).expect("the file loads");
        assert_eq!(tokens.access_token, "access-1");
        assert_eq!(tokens.refresh_token, "refresh-1");
        let config = Config::load(&config_path).expect("the config loads");
        assert_eq!(config.provider.as_deref(), Some("grok"));
        assert_eq!(config.base_url, GROK_BASE_URL);
        assert_eq!(config.model, GROK_MODEL);
        assert_eq!(config.api_key_env, None);
        let written = fs::read_to_string(&config_path).expect("the config reads");
        assert!(
            written.contains("title_model = \"google/gemini-3.8-flash\""),
            "{written}"
        );
        let device = &server.received()[0];
        assert_eq!(device.method, "POST");
        assert_eq!(device.path, "/oauth2/device/code");
        assert_eq!(
            device.form_value("client_id").as_deref(),
            Some(DEFAULT_CLIENT_ID)
        );
        assert_eq!(device.form_value("scope").as_deref(), Some(DEFAULT_SCOPE));
        assert_eq!(device.form_value("referrer").as_deref(), Some("kyotoagent"));
        let token = &server.token_requests()[0];
        assert_eq!(
            token.form_value("grant_type").as_deref(),
            Some(DEVICE_GRANT_TYPE)
        );
        assert_eq!(token.form_value("device_code").as_deref(), Some("device-1"));
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn login_prints_the_uri_and_code_when_the_complete_uri_is_absent() {
        let server = FakeServer::start(
            vec![device_split()],
            vec![token_ok("access-1", "refresh-1")],
        );
        let root = temp_root("login-split");
        let mut out = Vec::new();
        login(
            &client_for(&server),
            &root.join("config.toml"),
            &root.join(AUTH_FILE),
            &mut out,
        )
        .await
        .expect("login succeeds");
        let printed = String::from_utf8(out).expect("stdout is text");
        assert!(
            printed.contains("https://example.test/activate"),
            "{printed}"
        );
        assert!(printed.contains("WDJB-MJHT"), "{printed}");
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn pending_then_success_still_writes_the_file() {
        let server = FakeServer::start(
            vec![device_ok()],
            vec![pending(), token_ok("access-2", "refresh-2")],
        );
        let root = temp_root("pending");
        let auth_path = root.join(AUTH_FILE);
        let mut out = Vec::new();
        login(
            &client_for(&server),
            &root.join("config.toml"),
            &auth_path,
            &mut out,
        )
        .await
        .expect("pending then success logs in");
        let tokens = load(&auth_path).expect("the file loads");
        assert_eq!(tokens.access_token, "access-2");
        assert_eq!(server.token_requests().len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn access_denied_writes_no_file() {
        let server = FakeServer::start(vec![device_ok()], vec![denied()]);
        let root = temp_root("denied");
        let auth_path = root.join(AUTH_FILE);
        let mut out = Vec::new();
        let error = login(
            &client_for(&server),
            &root.join("config.toml"),
            &auth_path,
            &mut out,
        )
        .await
        .expect_err("denied is a failed login");
        assert!(matches!(error, AuthError::Denied), "{error:?}");
        assert!(!auth_path.exists(), "denied writes no file");
        assert!(
            !root.join("config.toml").exists(),
            "denied writes no config"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn expired_token_writes_no_file() {
        let server = FakeServer::start(vec![device_ok()], vec![expired()]);
        let root = temp_root("expired");
        let auth_path = root.join(AUTH_FILE);
        let mut out = Vec::new();
        let error = login(
            &client_for(&server),
            &root.join("config.toml"),
            &auth_path,
            &mut out,
        )
        .await
        .expect_err("expired is a failed login");
        assert!(matches!(error, AuthError::Expired), "{error:?}");
        assert!(!auth_path.exists(), "expired writes no file");
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_http_verification_uri_is_refused_and_writes_nothing() {
        let device = Canned::Json(
            200,
            serde_json::json!({
                "device_code": "device-1",
                "user_code": "WDJB-MJHT",
                "verification_uri": "http://example.test/activate",
                "interval": 0
            })
            .to_string(),
        );
        let server = FakeServer::start(vec![device], vec![token_ok("access-1", "refresh-1")]);
        let root = temp_root("http-uri");
        let auth_path = root.join(AUTH_FILE);
        let mut out = Vec::new();
        let error = login(
            &client_for(&server),
            &root.join("config.toml"),
            &auth_path,
            &mut out,
        )
        .await
        .expect_err("http is refused");
        assert!(matches!(error, AuthError::InvalidUri { .. }), "{error:?}");
        assert!(!auth_path.exists());
        assert!(server.token_requests().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_credential_writes_are_complete_and_private() {
        let root = temp_root("concurrent-writes");
        let path = root.join(AUTH_FILE);
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let mut writers = Vec::new();
            for writer in 0..8 {
                let path = &path;
                let barrier = &barrier;
                writers.push(scope.spawn(move || {
                    let mut failures = Vec::new();
                    for round in 0..16 {
                        barrier.wait();
                        let result = write_tokens(
                            path,
                            &Tokens {
                                access_token: format!("access-{writer}-{round}"),
                                refresh_token: format!("refresh-{writer}-{round}"),
                                expires_at: "2035-01-01T00:00:00.000Z".into(),
                            },
                        );
                        barrier.wait();
                        if let Err(error) = result {
                            failures.push(error.to_string());
                        }
                    }
                    assert!(failures.is_empty(), "{failures:?}");
                }));
            }
            for writer in writers {
                writer.join().unwrap();
            }
        });
        let stored = load(&path).unwrap();
        assert_eq!(
            stored.access_token.trim_start_matches("access-"),
            stored.refresh_token.trim_start_matches("refresh-")
        );
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    async fn concurrent_refresh(codex: bool, rejected: bool) {
        let root = temp_root(&format!("concurrent-refresh-{codex}-{rejected}"));
        let path = root.join(if codex { CODEX_AUTH_FILE } else { AUTH_FILE });
        let expiry = if rejected {
            "2099-01-01T00:00:00.000Z"
        } else {
            "2020-01-01T00:00:00.000Z"
        };
        if codex {
            write_codex_tokens(
                &path,
                &CodexTokens {
                    access_token: "old-access".into(),
                    refresh_token: "old-refresh".into(),
                    id_token: "old-id".into(),
                    account_id: "account-1".into(),
                    expires_at: expiry.into(),
                },
            )
            .unwrap();
        } else {
            write_tokens(
                &path,
                &Tokens {
                    access_token: "old-access".into(),
                    refresh_token: "old-refresh".into(),
                    expires_at: expiry.into(),
                },
            )
            .unwrap();
        }
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let received = Arc::clone(&requests);
        let app = axum::Router::new().route(
            "/oauth/token",
            axum::routing::post(move || {
                let received = Arc::clone(&received);
                async move {
                    let number = received.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    let status = if number == 0 {
                        axum::http::StatusCode::OK
                    } else {
                        axum::http::StatusCode::UNAUTHORIZED
                    };
                    (
                        status,
                        axum::Json(serde_json::json!({
                            "access_token": "fresh-access",
                            "refresh_token": "next-refresh",
                            "expires_in": 3600
                        })),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (first, second) = if codex {
            let client = CodexAuth::at(&base);
            if rejected {
                tokio::join!(
                    codex_access_after_401(&client, &path, "old-access"),
                    codex_access_after_401(&client, &path, "old-access")
                )
            } else {
                tokio::join!(codex_access(&client, &path), codex_access(&client, &path))
            }
        } else {
            let client = AuthClient::at(&base, &format!("{base}/oauth/token"), "client");
            if rejected {
                tokio::join!(
                    access_token_after_401(&client, &path, "old-access"),
                    access_token_after_401(&client, &path, "old-access")
                )
            } else {
                tokio::join!(access_token(&client, &path), access_token(&client, &path))
            }
        };
        server.abort();
        assert_eq!(first.unwrap(), "fresh-access");
        assert_eq!(second.unwrap(), "fresh-access");
        if rejected {
            let late = if codex {
                codex_access_after_401(&CodexAuth::at(&base), &path, "old-access").await
            } else {
                access_token_after_401(
                    &AuthClient::at(&base, &format!("{base}/oauth/token"), "client"),
                    &path,
                    "old-access",
                )
                .await
            };
            assert_eq!(late.unwrap(), "fresh-access");
        }
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(
            mode_of(&path.with_file_name(format!(
                "{}.refresh.lock",
                path.file_name().unwrap().to_str().unwrap()
            ))),
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn concurrent_expired_grok_refreshes_share_the_rotated_token() {
        concurrent_refresh(false, false).await;
    }

    #[tokio::test]
    async fn concurrent_rejected_grok_refreshes_share_the_rotated_token() {
        concurrent_refresh(false, true).await;
    }

    #[tokio::test]
    async fn concurrent_expired_codex_refreshes_share_the_rotated_token() {
        concurrent_refresh(true, false).await;
    }

    #[tokio::test]
    async fn concurrent_rejected_codex_refreshes_share_the_rotated_token() {
        concurrent_refresh(true, true).await;
    }

    #[tokio::test]
    async fn a_rotated_refresh_token_is_what_the_next_refresh_sends() {
        let server = FakeServer::start(
            vec![],
            vec![
                token_ok("access-2", "refresh-2"),
                token_ok("access-3", "refresh-3"),
            ],
        );
        let root = temp_root("refresh");
        let auth_path = root.join(AUTH_FILE);
        write_tokens(
            &auth_path,
            &Tokens {
                access_token: "access-1".into(),
                refresh_token: "refresh-1".into(),
                expires_at: "2020-01-01T00:00:00.000Z".into(),
            },
        )
        .expect("the file writes");
        let client = client_for(&server);
        let first = access_token(&client, &auth_path)
            .await
            .expect("the first refresh succeeds");
        assert_eq!(first, "access-2");
        let stored = load(&auth_path).expect("the file loads");
        assert_eq!(stored.refresh_token, "refresh-2");
        assert_eq!(mode_of(&auth_path), 0o600);
        write_tokens(
            &auth_path,
            &Tokens {
                access_token: stored.access_token,
                refresh_token: stored.refresh_token,
                expires_at: "2020-01-01T00:00:00.000Z".into(),
            },
        )
        .expect("the expiry is forced");
        let second = access_token(&client, &auth_path)
            .await
            .expect("the second refresh succeeds");
        assert_eq!(second, "access-3");
        let requests = server.token_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].form_value("grant_type").as_deref(),
            Some("refresh_token")
        );
        assert_eq!(
            requests[0].form_value("refresh_token").as_deref(),
            Some("refresh-1")
        );
        assert_eq!(
            requests[1].form_value("refresh_token").as_deref(),
            Some("refresh-2")
        );
        assert_eq!(
            load(&auth_path).expect("the file loads").refresh_token,
            "refresh-3"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_failed_refresh_leaves_the_file_and_says_to_log_in() {
        let server = FakeServer::start(
            vec![],
            vec![Canned::Json(
                400,
                serde_json::json!({ "error": "invalid_grant" }).to_string(),
            )],
        );
        let root = temp_root("refresh-fail");
        let auth_path = root.join(AUTH_FILE);
        let before = Tokens {
            access_token: "access-1".into(),
            refresh_token: "refresh-1".into(),
            expires_at: "2020-01-01T00:00:00.000Z".into(),
        };
        write_tokens(&auth_path, &before).expect("the file writes");
        let error = access_token(&client_for(&server), &auth_path)
            .await
            .expect_err("a failed refresh is not a token");
        assert!(matches!(error, AuthError::NeedLogin), "{error:?}");
        assert_eq!(
            error.to_string(),
            "Open Providers in Kyoto Agent to sign in."
        );
        let stored = load(&auth_path).expect("the file stays");
        assert_eq!(stored, before);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn logout_removes_auth_json_and_leaves_config() {
        let root = temp_root("logout");
        let auth_path = root.join(AUTH_FILE);
        let config_path = root.join("config.toml");
        write_tokens(
            &auth_path,
            &Tokens {
                access_token: "access-1".into(),
                refresh_token: "refresh-1".into(),
                expires_at: "2030-01-01T00:00:00.000Z".into(),
            },
        )
        .expect("the file writes");
        fs::write(&config_path, "provider = \"grok\"\n").expect("the config writes");
        logout(&auth_path).expect("logout succeeds");
        assert!(!auth_path.exists());
        assert!(config_path.exists());
        logout(&auth_path).expect("a second logout succeeds");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_auth_json_is_need_login() {
        let path = temp_root("missing").join(AUTH_FILE);
        let error = load(&path).expect_err("missing is need login");
        assert!(matches!(error, AuthError::NeedLogin));
        assert_eq!(
            error.to_string(),
            "Open Providers in Kyoto Agent to sign in."
        );
        let _ = fs::remove_dir_all(path.parent().expect("a parent"));
    }

    #[test]
    fn login_keeps_an_existing_grok_table() {
        let root = temp_root("keep-grok");
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            "provider = \"office\"\n\n[providers.office]\nbase_url = \"http://127.0.0.1:8/v1\"\nmodel = \"office\"\n\n[providers.grok]\nbase_url = \"http://127.0.0.1:9/v1\"\nmodel = \"kept\"\n",
        )
        .expect("the file writes");
        select_grok(&config_path).expect("grok is selected");
        let config = Config::load(&config_path).expect("the file loads");
        assert_eq!(config.provider.as_deref(), Some("grok"));
        assert_eq!(config.base_url, "http://127.0.0.1:9/v1");
        assert_eq!(config.model, "kept");
        assert_eq!(config.title_model(), Some(GROK_MODEL));
        let written = fs::read_to_string(&config_path).expect("the file reads");
        assert!(
            written.contains("title_model = \"google/gemini-3.8-flash\""),
            "{written}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn login_keeps_an_explicit_title_model() {
        let root = temp_root("keep-title");
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            "provider = \"grok\"\ntitle_model = \"title-fast\"\n\n[providers.grok]\nbase_url = \"http://127.0.0.1:9/v1\"\nmodel = \"kept\"\n",
        )
        .expect("the file writes");
        select_grok(&config_path).expect("grok is selected");
        let config = Config::load(&config_path).expect("the file loads");
        assert_eq!(config.title_model(), Some("title-fast"));
        let written = fs::read_to_string(&config_path).expect("the file reads");
        assert!(
            written.contains("title_model = \"title-fast\""),
            "{written}"
        );
        assert!(!written.contains("google/gemini-3.8-flash"), "{written}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn login_keeps_an_empty_title_model() {
        let root = temp_root("empty-title");
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            "provider = \"grok\"\ntitle_model = \"\"\n\n[providers.grok]\nbase_url = \"http://127.0.0.1:9/v1\"\nmodel = \"kept\"\n",
        )
        .expect("the file writes");
        select_grok(&config_path).expect("grok is selected");
        let config = Config::load(&config_path).expect("the file loads");
        assert_eq!(config.title_model(), None);
        let written = fs::read_to_string(&config_path).expect("the file reads");
        assert!(written.contains("title_model = \"\""), "{written}");
        assert!(!written.contains("google/gemini-3.8-flash"), "{written}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn default_path_is_inside_kyotoagent() {
        if let Some(path) = default_path() {
            assert!(path.ends_with(".kyotoagent/auth.json"), "{path:?}");
            let text = path.to_string_lossy();
            assert!(
                !text.contains("/.grok/"),
                "the session file is not the Grok CLI's: {path:?}"
            );
        }
    }

    struct CodexServer {
        addr: SocketAddr,
        received: Arc<Mutex<Vec<Received>>>,
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl CodexServer {
        fn start(usercode: Vec<Canned>, poll: Vec<Canned>, exchange: Vec<Canned>) -> CodexServer {
            let listener = TcpListener::bind("127.0.0.1:0").expect("the fake server binds");
            listener
                .set_nonblocking(true)
                .expect("the listener does not block the thread");
            let addr = listener
                .local_addr()
                .expect("the fake server has an address");
            let received = Arc::new(Mutex::new(Vec::new()));
            let queues = Arc::new(Mutex::new(CodexQueues {
                usercode,
                poll,
                exchange,
            }));
            let stop = Arc::new(AtomicBool::new(false));
            let handle = {
                let received = Arc::clone(&received);
                let queues = Arc::clone(&queues);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                let _ = stream.set_nonblocking(false);
                                serve_codex(stream, &received, &queues);
                            }
                            Err(ref source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            Err(_) => break,
                        }
                    }
                })
            };
            CodexServer {
                addr,
                received,
                stop,
                handle: Some(handle),
            }
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn received(&self) -> Vec<Received> {
            self.received
                .lock()
                .expect("the log is not poisoned")
                .clone()
        }
    }

    impl Drop for CodexServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    struct CodexQueues {
        usercode: Vec<Canned>,
        poll: Vec<Canned>,
        exchange: Vec<Canned>,
    }

    fn serve_codex(
        mut stream: TcpStream,
        received: &Arc<Mutex<Vec<Received>>>,
        queues: &Arc<Mutex<CodexQueues>>,
    ) {
        let Some(request) = read_request(&mut stream) else {
            return;
        };
        let slot = if request.path.ends_with("/usercode") {
            0
        } else if request.path.contains("deviceauth") {
            1
        } else {
            2
        };
        received
            .lock()
            .expect("the log is not poisoned")
            .push(request);
        let reply = {
            let mut queues = queues.lock().expect("the queue is not poisoned");
            let queue = match slot {
                0 => &mut queues.usercode,
                1 => &mut queues.poll,
                _ => &mut queues.exchange,
            };
            if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue
                    .first()
                    .cloned()
                    .unwrap_or(Canned::Json(500, String::new()))
            }
        };
        match reply {
            Canned::Json(status, body) => respond(&mut stream, status, &body),
        }
    }

    fn codex_usercode() -> Canned {
        Canned::Json(
            200,
            serde_json::json!({
                "device_auth_id": "dev-1",
                "user_code": "ABCD-EFGH",
                "interval": "0"
            })
            .to_string(),
        )
    }

    fn codex_pending() -> Canned {
        Canned::Json(403, String::new())
    }

    fn codex_grant() -> Canned {
        Canned::Json(
            200,
            serde_json::json!({
                "authorization_code": "authcode-1",
                "code_verifier": "verifier-1",
                "code_challenge": "challenge-1"
            })
            .to_string(),
        )
    }

    fn b64url(raw: &[u8]) -> String {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        let mut index = 0;
        while index < raw.len() {
            let first = raw[index];
            let second = if index + 1 < raw.len() {
                raw[index + 1]
            } else {
                0
            };
            let third = if index + 2 < raw.len() {
                raw[index + 2]
            } else {
                0
            };
            let left = raw.len() - index;
            out.push(TABLE[(first >> 2) as usize] as char);
            out.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
            if left == 1 {
                break;
            }
            out.push(TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
            if left == 2 {
                break;
            }
            out.push(TABLE[(third & 0x3f) as usize] as char);
            index += 3;
        }
        out
    }

    fn test_jwt(account: &str) -> String {
        let header = b64url(br#"{"alg":"none"}"#);
        let payload = serde_json::json!({
            CODEX_AUTH_CLAIM: { "chatgpt_account_id": account }
        });
        format!("{header}.{}.sig", b64url(payload.to_string().as_bytes()))
    }

    fn codex_exchange(access: &str, refresh: &str, id_token: &str) -> Canned {
        Canned::Json(
            200,
            serde_json::json!({
                "access_token": access,
                "refresh_token": refresh,
                "id_token": id_token,
                "expires_in": 3600
            })
            .to_string(),
        )
    }

    fn client_for_codex(server: &CodexServer) -> CodexAuth {
        CodexAuth::at(&server.base_url())
    }

    #[tokio::test]
    async fn login_codex_writes_the_file_selects_codex_and_leaves_the_cli_file() {
        let jwt = test_jwt("acc-1");
        let server = CodexServer::start(
            vec![codex_usercode()],
            vec![codex_pending(), codex_grant()],
            vec![codex_exchange("access-1", "refresh-1", &jwt)],
        );
        let root = temp_root("codex-login");
        let decoy_dir = root.join(".codex");
        fs::create_dir_all(&decoy_dir).expect("the decoy dir exists");
        let decoy = decoy_dir.join("auth.json");
        let canary = "{\"tokens\":{\"access_token\":\"canary-token-value\"}}";
        fs::write(&decoy, canary).expect("the decoy writes");
        let before = fs::read(&decoy).expect("the decoy reads");
        let config_path = root.join("config.toml");
        let auth_path = root.join(CODEX_AUTH_FILE);
        let mut out = Vec::new();
        login_codex(
            &client_for_codex(&server),
            &config_path,
            &auth_path,
            &mut out,
        )
        .await
        .expect("login succeeds");
        let printed = String::from_utf8(out).expect("stdout is text");
        assert!(printed.contains(&format!("{}/codex/device", server.base_url())));
        assert!(printed.contains("ABCD-EFGH"));
        assert!(!printed.contains("access-1"));
        assert!(!printed.contains("refresh-1"));
        assert!(!printed.contains("verifier-1"));
        assert!(!printed.contains("canary-token-value"));
        assert!(!printed.contains(&jwt));
        assert_eq!(fs::read(&decoy).expect("the decoy still reads"), before);
        assert_eq!(mode_of(&auth_path), 0o600);
        let tokens = load_codex(&auth_path).expect("the file loads");
        assert_eq!(tokens.access_token, "access-1");
        assert_eq!(tokens.refresh_token, "refresh-1");
        assert_eq!(tokens.account_id, "acc-1");
        assert_eq!(tokens.id_token, jwt);
        let stored = fs::read_to_string(&auth_path).expect("the auth file reads");
        assert!(!stored.contains("canary-token-value"));
        let config = Config::load(&config_path).expect("the config loads");
        assert_eq!(config.provider.as_deref(), Some("codex"));
        assert_eq!(config.model, "gpt-6.1-sol");
        assert!(config.is_codex());
        assert!(config.api_key_env.is_none());
        let written = fs::read_to_string(&config_path).expect("the config reads");
        assert!(written.contains("kind = \"codex\""));
        assert!(written.contains("model = \"gpt-6.1-sol\""));
        assert!(!written.contains("base_url"));
        assert!(!written.contains("api_key_env"));
        assert!(!written.contains("access-1"));
        assert!(!written.contains("refresh-1"));
        assert!(!written.contains("canary-token-value"));
        assert!(!written.contains(&jwt));
        let seen = server.received();
        assert_eq!(seen[0].path, "/api/accounts/deviceauth/usercode");
        assert_eq!(seen[0].method, "POST");
        let usercode: serde_json::Value = serde_json::from_str(&seen[0].body).expect("json");
        assert_eq!(usercode["client_id"], CODEX_CLIENT_ID);
        assert_eq!(seen[1].path, "/api/accounts/deviceauth/token");
        assert_eq!(seen[2].path, "/api/accounts/deviceauth/token");
        let poll: serde_json::Value = serde_json::from_str(&seen[2].body).expect("json");
        assert_eq!(poll["device_auth_id"], "dev-1");
        assert_eq!(poll["user_code"], "ABCD-EFGH");
        assert_eq!(seen[3].path, "/oauth/token");
        assert_eq!(
            seen[3].form_value("grant_type").as_deref(),
            Some("authorization_code")
        );
        assert_eq!(seen[3].form_value("code").as_deref(), Some("authcode-1"));
        assert_eq!(
            seen[3].form_value("code_verifier").as_deref(),
            Some("verifier-1")
        );
        assert_eq!(
            seen[3].form_value("client_id").as_deref(),
            Some(CODEX_CLIENT_ID)
        );
        assert_eq!(
            seen[3].form_value("redirect_uri").as_deref(),
            Some(format!("{}/deviceauth/callback", server.base_url()).as_str())
        );
        assert!(seen[3].form_value("code_challenge").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn login_codex_without_an_account_id_writes_nothing() {
        let header = b64url(br#"{"alg":"none"}"#);
        let payload = b64url(br#"{"sub":"user"}"#);
        let jwt = format!("{header}.{payload}.sig");
        let server = CodexServer::start(
            vec![codex_usercode()],
            vec![codex_grant()],
            vec![codex_exchange("access-1", "refresh-1", &jwt)],
        );
        let root = temp_root("codex-no-account");
        let auth_path = root.join(CODEX_AUTH_FILE);
        let mut out = Vec::new();
        let error = login_codex(
            &client_for_codex(&server),
            &root.join("config.toml"),
            &auth_path,
            &mut out,
        )
        .await
        .expect_err("missing account id fails");
        assert!(matches!(error, AuthError::BadAccount), "{error:?}");
        assert!(!auth_path.exists());
        let printed = String::from_utf8(out).expect("stdout is text");
        assert!(!printed.contains("access-1"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn select_codex_keeps_an_existing_model_and_base() {
        let root = temp_root("codex-keep");
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            "provider = \"office\"\n\n[providers.codex]\nmodel = \"kept\"\nbase_url = \"local\"\n",
        )
        .expect("the file writes");
        select_codex(&config_path).expect("codex is selected");
        let config = Config::load(&config_path).expect("the file loads");
        assert_eq!(config.provider.as_deref(), Some("codex"));
        assert_eq!(config.model, "kept");
        assert_eq!(config.base_url, "local");
        assert!(config.is_codex());
        assert!(config.api_key_env.is_none());
        let written = fs::read_to_string(&config_path).expect("the file reads");
        assert!(written.contains("kind = \"codex\""));
        assert!(!written.contains("api_key_env"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_codex_token_inside_the_skew_is_not_fresh() {
        let soon = crate::events::format_millis((now_millis() + 30_000) as i64);
        let later = crate::events::format_millis((now_millis() + 120_000) as i64);
        assert!(!fresh_stamp(&soon));
        assert!(fresh_stamp(&later));
    }
    #[test]
    fn saved_provider_keys_are_isolated_and_environment_keys_take_precedence() {
        let root = temp_root("provider-keys");
        fs::create_dir_all(&root).unwrap();
        let variable = format!("KYOTO_PROVIDER_KEY_TEST_{}", std::process::id());
        let mut config = Config::from_toml(&format!("provider = \"office\"\n[providers.office]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"test\"\napi_key_env = \"{variable}\"\n")).unwrap();
        let path = provider_key_path(&root, Some("office"));
        write_opencode_key(&path, "saved-secret").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            stored_provider_key(&config, Some(&root))
                .unwrap()
                .as_deref(),
            Some("saved-secret")
        );
        std::env::set_var(&variable, "environment-secret");
        assert_eq!(
            stored_provider_key(&config, Some(&root))
                .unwrap()
                .as_deref(),
            Some("environment-secret")
        );
        std::env::remove_var(&variable);
        config.provider = Some("other".into());
        assert_eq!(stored_provider_key(&config, Some(&root)).unwrap(), None);
        let _ = fs::remove_dir_all(root);
    }
}
