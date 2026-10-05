//! The server: HTTP on a unix socket, and optionally HTTPS with HTTP/2.
//!
//! `kyotoagent serve` binds `~/.kyotoagent/kyotoagent.sock` with mode 0600 and serves the
//! sessions routes. An optional `listen` address also serves the same routes
//! over rustls with ALPN `h2` and `http/1.1`. Starting as
//! root is refused, and a second serve exits rather than replace a live one.
//!
//! On startup every session under `~/.kyotoagent/sessions` is reloaded, so an
//! unanswered permission is still waiting and its answer still lands.
//!
//! The routes are the whole API:
//!
//! - `POST /v1/sessions` creates a session for an existing directory.
//! - `GET /v1/sessions` lists the sessions, a waiting row saying what it waits
//!   on.
//! - `GET /v1/sessions/:id/view` is the quiet projection: status, cards,
//!   revision.
//! - `GET /v1/sessions/:id/events` is the raw log.
//! - `POST /v1/sessions/:id/messages` starts a turn: 202 and a turn id, or 409
//!   when the session is busy.
//! - `POST /v1/sessions/:id/answers` answers the open card: 409 when that card
//!   is already settled.
//! - `POST /v1/sessions/:id/cancel` stops the turn.
//! - `GET /v1/profiles` lists the profile names in config.
//! - `POST /v1/sessions/:id/profile` sets or clears the live session profile.
//! - `DELETE /v1/sessions/:id` stops the turn, removes a worktree child, and
//!   deletes the session directory.
//! - `DELETE /v1/sessions/:id/worktree` removes the git worktree and keeps the
//!   session.

use std::fs;
use std::future::IntoFuture;
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::serve::Listener;
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, UnixListener};
use tokio_rustls::TlsAcceptor;

mod https;
pub mod local;
pub mod login;
mod proof;

use crate::config::Config;
use crate::events::{now, open_enhance, Decision, EventKind};
use crate::permit::Answer;
use crate::screen::Status;
use crate::session::{AllowList, Session, SessionError, SessionMeta, EVENTS_FILE};
use crate::tools::{ToolError, Tools};
use crate::turn::{Runner, TurnError};

/// The socket file inside the `~/.kyotoagent` root.
pub const SOCKET_FILE: &str = "kyotoagent.sock";
/// The sessions directory inside the `~/.kyotoagent` root.
pub const SESSIONS_DIR: &str = "sessions";
pub const DEFAULT_LISTEN_CERT: &str = "certs/server.crt";
pub const DEFAULT_LISTEN_KEY: &str = "certs/server.key";

/// The `~/.kyotoagent` root, when there is a home directory to put it in.
pub fn default_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = PathBuf::from(home);
    if home.as_os_str().is_empty() {
        None
    } else {
        Some(home.join(".kyotoagent"))
    }
}

/// The socket path: the root and [`SOCKET_FILE`].
pub fn default_socket() -> Option<PathBuf> {
    Some(default_root()?.join(SOCKET_FILE))
}

/// A counter mixed into every session id, so two sessions created in the same
/// nanosecond are still two different ids.
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new session id: eight hex characters, mixed from the clock, the process,
/// and the counter.
fn generate_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.subsec_nanos() as u64)
        .unwrap_or(0);
    let count = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let bits = nanos.wrapping_mul(0x9e37_79b9)
        ^ count.wrapping_mul(0x85eb_ca6b)
        ^ (std::process::id() as u64);
    format!("{:08x}", bits)
}

/// A session id that is not taken yet.
fn new_session_id(root: &Path) -> String {
    loop {
        let id = generate_id();
        if !session_dir(root, &id).exists() {
            return id;
        }
    }
}

/// The directory of one session.
fn session_dir(root: &Path, id: &str) -> PathBuf {
    root.join(SESSIONS_DIR).join(id)
}

/// Every session directory under the root, in name order. A directory without
/// a `meta.json` is not a session and is skipped.
fn session_dirs(root: &Path) -> Vec<PathBuf> {
    let sessions = root.join(SESSIONS_DIR);
    let Ok(entries) = fs::read_dir(&sessions) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// What a waiting session is waiting on: the newest card of either kind that
/// has no answer after it. A turn holds one open card at a time, so at most one
/// of the two is still open.
fn open_card_kind(dir: &Path) -> Option<&'static str> {
    let events = Session::at(dir).events().ok()?;
    let mut permission = false;
    let mut question = false;
    for event in &events {
        match event.kind {
            EventKind::Permission => permission = true,
            EventKind::PermissionAnswer => permission = false,
            EventKind::Question => question = true,
            EventKind::QuestionAnswer => question = false,
            _ => {}
        }
    }
    if permission {
        Some("permission")
    } else if question {
        Some("question")
    } else if open_enhance(&events).is_some() {
        Some("enhance")
    } else {
        None
    }
}

/// Whether something is answering on the socket already. A stale socket file
/// left by a killed server refuses the connection and is not live.
fn socket_is_live(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

struct Listen {
    addr: String,
    cert: PathBuf,
    key: PathBuf,
}

struct HttpsListener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    handshakes: tokio::task::JoinSet<
        Option<(
            tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
            SocketAddr,
        )>,
    >,
}

impl Listener for HttpsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                Some(result) = self.handshakes.join_next(), if !self.handshakes.is_empty() => {
                    if let Ok(Some(connection)) = result {
                        return connection;
                    }
                }
                accepted = self.listener.accept(), if self.handshakes.len() < 64 => {
                    let (stream, addr) = match accepted {
                        Ok(pair) => pair,
                        Err(_) => {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            continue;
                        }
                    };
                    let acceptor = self.acceptor.clone();
                    self.handshakes.spawn(async move {
                        let tls = tokio::time::timeout(
                            Duration::from_secs(5),
                            acceptor.accept(stream),
                        ).await.ok()?.ok()?;
                        Some((tls, addr))
                    });
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

/// The server: the `~/.kyotoagent` root and the runner that owns the sessions.
pub struct Server {
    root: PathBuf,
    runner: Arc<Runner>,
    listen: Option<Listen>,
    https: Arc<https::Runtime>,
    config_edit: Arc<std::sync::Mutex<()>>,
    logins: Arc<login::Logins>,
}

impl Server {
    /// A server over `root`, with every session under it reloaded. The root
    /// is created if it is not there yet.
    pub fn new(root: &Path, config: &Config) -> Result<Server, ServerError> {
        fs::create_dir_all(root).map_err(ServerError::RootIo)?;
        let runner = Runner::with_config_file(config, &root.join(crate::config::CONFIG_FILE))?;
        let listen = config.listen.as_ref().map(|addr| Listen {
            addr: addr.clone(),
            cert: resolve_under(root, config.listen_cert.as_deref(), DEFAULT_LISTEN_CERT),
            key: resolve_under(root, config.listen_key.as_deref(), DEFAULT_LISTEN_KEY),
        });
        let server = Server {
            root: root.to_path_buf(),
            runner,
            listen,
            https: Arc::new(https::Runtime::default()),
            config_edit: Arc::new(std::sync::Mutex::new(())),
            logins: Arc::new(login::Logins::new(
                crate::auth::AuthClient::new(
                    config
                        .grok_client_id
                        .as_deref()
                        .unwrap_or(crate::auth::DEFAULT_CLIENT_ID),
                ),
                crate::auth::CodexAuth::at(crate::auth::CODEX_ISSUER),
            )),
        };
        server.reload()?;
        Ok(server)
    }

    /// Reload every session under the root into the runner.
    fn reload(&self) -> Result<(), ServerError> {
        let dirs = session_dirs(&self.root);
        self.runner.reload(&dirs)?;
        Ok(())
    }

    /// The runner, for a caller that has to reach the sessions directly.
    pub fn runner(&self) -> &Arc<Runner> {
        &self.runner
    }

    pub fn with_login_clients(
        mut self,
        grok: crate::auth::AuthClient,
        codex: crate::auth::CodexAuth,
    ) -> Self {
        self.logins = Arc::new(login::Logins::new(grok, codex));
        self
    }

    pub fn https_addr(&self) -> Option<SocketAddr> {
        *self.https.addr.lock().expect("https addr")
    }

    /// Bind the socket and serve the routes until the process ends.
    ///
    /// Refuses to start as root, and exits rather than replace a live socket.
    pub async fn serve(&self) -> Result<(), ServerError> {
        refuse_root()?;
        let _serving = local::serving_lock(&self.root)?;
        let socket_path = self.root.join(SOCKET_FILE);
        if socket_is_live(&socket_path) {
            return Err(ServerError::AlreadyServing);
        }
        let _lifetime = https::Lifetime(Arc::clone(&self.https));
        let https = match &self.listen {
            Some(listen) => Some(self.bind_https(listen).await?),
            None => None,
        };
        let _ = fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).map_err(|source| ServerError::Bind {
            path: socket_path.clone(),
            source,
        })?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            ServerError::Chmod {
                path: socket_path.clone(),
                source,
            }
        })?;
        println!("Kyoto Agent serving on {}", socket_path.display());
        if let Some((_, addr)) = &https {
            println!("Kyoto Agent listening on https://{addr}");
        }
        let _ = std::io::stdout().flush();
        let router = self.router();
        let https_router = https::authenticated(router.clone(), &self.root)?;
        let (sender, mut incoming) = tokio::sync::mpsc::channel(1);
        *self.https.sender.lock().expect("https sender") = Some(sender);
        let mut listeners = tokio::task::JoinSet::new();
        if let Some((listener, _)) = https {
            listeners.spawn(axum::serve(listener, https_router.clone()).into_future());
        }
        let unix = axum::serve(listener, router).into_future();
        tokio::pin!(unix);
        loop {
            tokio::select! {
                result = &mut unix => return result.map_err(ServerError::Serve),
                Some(listener) = incoming.recv() => {
                    listeners.spawn(axum::serve(listener, https_router.clone()).into_future());
                }
                Some(result) = listeners.join_next(), if !listeners.is_empty() => {
                    return result.map_err(|error| ServerError::Tls(error.to_string()))?
                        .map_err(ServerError::Serve);
                }
            }
        }
    }

    async fn bind_https(
        &self,
        listen: &Listen,
    ) -> Result<(HttpsListener, SocketAddr), ServerError> {
        let listener = https::bind(listen).await?;
        let addr = listener.local_addr().map_err(ServerError::Serve)?;
        *self.https.addr.lock().expect("https addr") = Some(addr);
        Ok((listener, addr))
    }

    /// The routes, over the shared state.
    fn router(&self) -> Router {
        let state = AppState {
            root: self.root.clone(),
            runner: Arc::clone(&self.runner),
            config_edit: Arc::clone(&self.config_edit),
            logins: Arc::clone(&self.logins),
            https: Arc::clone(&self.https),
        };
        Router::new()
            .route("/v1/https", get(https::status).post(https::enable))
            .route("/v1/share", post(https::share))
            .route(
                "/v1/login/{id}",
                post(login::start).get(login::status).delete(login::cancel),
            )
            .route(
                "/v1/providers",
                get(login::providers).post(login::create_provider),
            )
            .route("/v1/providers/{id}/key", post(login::save_key))
            .route("/v1/server", get(server_info))
            .route("/v1/pair", post(pair_client))
            .route("/v1/sessions", post(create_session).get(list_sessions))
            .route("/v1/sessions/{id}/view", get(view_session))
            .route("/v1/sessions/{id}/events", get(events))
            .route("/v1/sessions/{id}/proof", get(proof::history))
            .route("/v1/sessions/{id}/artifacts", get(proof::artifacts))
            .route(
                "/v1/sessions/{id}/artifacts/files/{file_id}",
                get(proof::download),
            )
            .route(
                "/v1/sessions/{id}/proof/files/{file_id}",
                get(proof::download),
            )
            .route(
                "/v1/sessions/{id}/messages",
                post(message).layer(axum::extract::DefaultBodyLimit::max(30 * 1024 * 1024)),
            )
            .route("/v1/sessions/{id}/queue/{queued_id}", delete(remove_queued))
            .route("/v1/sessions/{id}/compact", post(compact))
            .route("/v1/sessions/{id}/answers", post(answer))
            .route("/v1/sessions/{id}/cancel", post(cancel))
            .route("/v1/sessions/{id}/workspace", get(workspace_status))
            .route("/v1/sessions/{id}/worktree", delete(delete_worktree))
            .route("/v1/sessions/{id}", delete(delete_session))
            .route("/v1/sessions/{id}/yolo", post(set_yolo))
            .route("/v1/sessions/{id}/enhance", post(set_enhance))
            .route("/v1/sessions/{id}/closeout", post(set_closeout_show))
            .route("/v1/sessions/{id}/profile", post(set_profile))
            .route("/v1/sessions/{id}/model", post(set_model))
            .route("/v1/sessions/{id}/model/session", post(set_session_model))
            .route("/v1/profiles", get(list_profiles))
            .route("/v1/sessions/{id}/file", get(read_session_file))
            .route("/v1/sessions/{id}/tasks/{task_id}", get(read_task))
            .route("/v1/models", get(list_models))
            .route("/v1/layout", get(get_layout).put(save_layout))
            .route("/v1/projects", get(list_projects).post(add_project))
            .route("/v1/projects/{id}", axum::routing::put(edit_project))
            .with_state(state)
    }
}

/// The state every handler shares: the root, to read the sessions on disk, and
/// the runner, to drive the turns.
#[derive(Clone)]
struct AppState {
    root: PathBuf,
    runner: Arc<Runner>,
    config_edit: Arc<std::sync::Mutex<()>>,
    logins: Arc<login::Logins>,
    https: Arc<https::Runtime>,
}

/// A body the server could not use, with the status the client gets.
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl std::fmt::Display) -> ApiError {
        ApiError {
            status: StatusCode::BAD_REQUEST,
            message: message.to_string(),
        }
    }

    fn not_found() -> ApiError {
        ApiError {
            status: StatusCode::NOT_FOUND,
            message: "no such session".to_string(),
        }
    }

    fn conflict(message: impl std::fmt::Display) -> ApiError {
        ApiError {
            status: StatusCode::CONFLICT,
            message: message.to_string(),
        }
    }

    fn server(message: impl std::fmt::Display) -> ApiError {
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.message });
        (self.status, Json(body)).into_response()
    }
}

/// Something the server itself could not do, for the CLI to print.
#[derive(Debug)]
pub enum ServerError {
    /// Running as root: the socket would be a way in for anybody but the user.
    Root,
    /// Another serve already holds the socket.
    AlreadyServing,
    /// The root directory could not be created.
    RootIo(std::io::Error),
    /// The socket could not be bound.
    Bind {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The socket could not be made private.
    Chmod {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The runner could not be built, or the sessions could not reload.
    Turn(TurnError),
    /// The server could not serve.
    Serve(std::io::Error),
    ListenAddr(String),
    ListenBind {
        addr: String,
        source: std::io::Error,
    },
    CertIo {
        path: PathBuf,
        source: std::io::Error,
    },
    Tls(String),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerError::Root => write!(f, "refusing to serve as root"),
            ServerError::AlreadyServing => write!(f, "a Kyoto Agent server is already running"),
            ServerError::RootIo(source) => {
                write!(f, "could not create the Kyoto Agent directory: {source}")
            }
            ServerError::Bind { path, source } => {
                write!(f, "could not bind {}: {source}", path.display())
            }
            ServerError::Chmod { path, source } => {
                write!(f, "could not make {} private: {source}", path.display())
            }
            ServerError::Turn(source) => write!(f, "{source}"),
            ServerError::Serve(source) => write!(f, "the server stopped: {source}"),
            ServerError::ListenAddr(addr) => {
                write!(f, "could not parse listen address {addr}")
            }
            ServerError::ListenBind { addr, source } => {
                write!(f, "could not bind {addr}: {source}")
            }
            ServerError::CertIo { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            ServerError::Tls(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ServerError {}

impl From<TurnError> for ServerError {
    fn from(source: TurnError) -> ServerError {
        ServerError::Turn(source)
    }
}

fn resolve_under(root: &Path, value: Option<&str>, default: &str) -> PathBuf {
    match value {
        Some(value) => {
            let path = Path::new(value);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            }
        }
        None => root.join(default),
    }
}

fn load_tls(cert: &Path, key: &Path) -> Result<Arc<rustls::ServerConfig>, ServerError> {
    use rustls::pki_types::pem::{Error as PemError, PemObject};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let cert_file = fs::File::open(cert).map_err(|source| ServerError::CertIo {
        path: cert.to_path_buf(),
        source,
    })?;
    let key_file = fs::File::open(key).map_err(|source| ServerError::CertIo {
        path: key.to_path_buf(),
        source,
    })?;
    let certs = CertificateDer::pem_reader_iter(cert_file)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| ServerError::Tls(source.to_string()))?;
    let key = PrivateKeyDer::from_pem_reader(key_file).map_err(|source| {
        ServerError::Tls(match source {
            PemError::NoItemsFound => format!("no private key in {}", key.display()),
            source => source.to_string(),
        })
    })?;
    if certs.is_empty() {
        return Err(ServerError::Tls(format!(
            "no certificates in {}",
            cert.display()
        )));
    }
    let mut config = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .map_err(|source| ServerError::Tls(source.to_string()))?
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .map_err(|source| ServerError::Tls(source.to_string()))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// A refusal to serve as root, before anything is bound.
fn refuse_root() -> Result<(), ServerError> {
    if unsafe { libc::geteuid() } == 0 {
        return Err(ServerError::Root);
    }
    Ok(())
}

/// The body of `POST /v1/sessions`.
#[derive(Deserialize)]
struct CreateSessionRequest {
    workspace: String,
    #[serde(default, rename = "taskId")]
    task_id: Option<String>,
    #[serde(default)]
    worktree: bool,
    #[serde(default)]
    profile: Option<String>,
}

/// The row of `GET /v1/sessions`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionRow {
    id: String,
    workspace: String,
    status: Status,
    updated_at: String,
    created_at: String,
    /// What a waiting session waits on, when it waits on something.
    #[serde(skip_serializing_if = "Option::is_none")]
    waiting: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pull_url: Option<String>,
    #[serde(default)]
    compacting: bool,
    #[serde(default)]
    yolo: bool,
    #[serde(default)]
    enhance: bool,
    show_closeout: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    isolation: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    hidden: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    worktree: bool,
    allow: AllowList,
}

/// The reply to `POST /v1/sessions`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreatedSession {
    id: String,
    workspace: String,
    status: Status,
}

/// The body of `POST /v1/sessions/:id/messages`.
#[derive(Deserialize)]
struct MessageRequest {
    text: String,
    #[serde(default)]
    images: Vec<crate::attachment::ImageAttachment>,
    #[serde(default)]
    enhance: Option<bool>,
}

/// The body of `POST /v1/sessions/:id/answers`.
#[derive(Deserialize)]
struct AnswerRequest {
    /// The event id of the card being answered.
    id: String,
    /// `allow_once`, `allow_session`, `deny`, or the reply text.
    choice: String,
    #[serde(default)]
    text: String,
}

/// The three decisions a permission takes. Anything else is a question's reply
/// text, which is free-form.
fn parse_choice(choice: &str) -> Option<Decision> {
    match choice {
        "allow_once" => Some(Decision::AllowOnce),
        "allow_session" => Some(Decision::AllowSession),
        "deny" => Some(Decision::Deny),
        _ => None,
    }
}

/// Create a session for an existing directory and add it to the runner.
#[derive(Clone)]
struct RemoteClient;

async fn create_session(
    State(state): State<AppState>,
    remote: Option<Extension<RemoteClient>>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let workspace = PathBuf::from(&body.workspace);
    if !workspace.is_absolute() {
        return Err(ApiError::bad_request(
            "the workspace must be an absolute path",
        ));
    }
    if !workspace.is_dir() {
        return Err(ApiError::bad_request(
            "the workspace directory does not exist",
        ));
    }
    let requested_text = workspace.display().to_string();
    let config = state.runner.current_config();
    if remote.is_some() {
        let canonical = fs::canonicalize(&workspace).map_err(ApiError::bad_request)?;
        if !config
            .projects
            .iter()
            .any(|project| fs::canonicalize(&project.path).ok().as_ref() == Some(&canonical))
        {
            return Err(ApiError::bad_request(
                "Choose a configured project on this server.",
            ));
        }
    }
    let profile = match body.profile.as_deref().map(str::trim) {
        Some("") => None,
        Some(name) => Some(name.to_string()),
        None => config.profile_for(&requested_text, Some(&requested_text)),
    };
    if let Some(name) = profile.as_deref() {
        if config.profile(name).is_none() {
            return Err(ApiError::bad_request(format!("unknown profile: {name}")));
        }
    }
    if body
        .task_id
        .as_deref()
        .is_some_and(|id| id.trim().is_empty() || id.len() > 256)
    {
        return Err(ApiError::bad_request(
            "taskId must be nonblank and at most 256 bytes",
        ));
    }
    let id = new_session_id(&state.root);
    let requested = workspace.clone();
    let workspace = if body.worktree {
        git_worktree_workspace(&state.root, &workspace, &id)?
    } else {
        workspace
    };
    let session = Session::at(&session_dir(&state.root, &id));
    let model = state.runner.model();
    let mut meta = SessionMeta::new(&id, &workspace, &model, &now());
    meta.task_id = body.task_id;
    let requested_text = requested.display().to_string();
    let workspace_text = workspace.display().to_string();
    meta.requested_workspace = Some(requested_text.clone());
    if body.worktree {
        meta.isolation = Some("worktree".to_string());
    }
    meta.effort = state.runner.effort();
    meta.yolo = config.yolo_for(&workspace_text, Some(&requested_text));
    meta.enhance = config.enhance_for(&workspace_text, Some(&requested_text));
    meta.show_closeout = config.show_closeout_for(&workspace_text, Some(&requested_text));
    meta.profile = profile;
    session
        .create(&meta)
        .map_err(|source| ApiError::server(source.to_string()))?;
    state
        .runner
        .add_session(&session)
        .map_err(|source| ApiError::server(source.to_string()))?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedSession {
            id,
            workspace: workspace.display().to_string(),
            status: Status::Idle,
        }),
    ))
}

fn git_toplevel(workspace: &Path) -> Result<PathBuf, ApiError> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(workspace)
        .output()
        .map_err(|_| ApiError::bad_request("the workspace is not a git repository"))?;
    if !output.status.success() {
        return Err(ApiError::bad_request(
            "the workspace is not a git repository",
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Err(ApiError::bad_request(
            "the workspace is not a git repository",
        ));
    }
    Ok(PathBuf::from(path))
}

fn git_message(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        let stdout = stdout.trim();
        if stdout.is_empty() {
            "git worktree add failed".to_string()
        } else {
            stdout.to_string()
        }
    } else {
        stderr.to_string()
    }
}

fn git_worktree_workspace(root: &Path, workspace: &Path, id: &str) -> Result<PathBuf, ApiError> {
    let toplevel = git_toplevel(workspace)?;
    let repo = toplevel
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ApiError::bad_request("the workspace is not a git repository"))?;
    let dest = root.join("worktrees").join(format!("{repo}-{id}"));
    fs::create_dir_all(root.join("worktrees"))
        .map_err(|source| ApiError::server(source.to_string()))?;
    let branch = format!("kyotoagent/{id}");
    let dest_str = dest.to_string_lossy().into_owned();
    let output = Command::new("git")
        .args(["worktree", "add", "-b", &branch, &dest_str, "HEAD"])
        .current_dir(&toplevel)
        .output()
        .map_err(ApiError::bad_request)?;
    if !output.status.success() {
        return Err(ApiError::bad_request(git_message(&output)));
    }
    Ok(dest)
}

/// List the sessions, newest first.
async fn list_sessions(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let config = state.runner.current_config();
    let mut rows: Vec<SessionRow> = session_dirs(&state.root)
        .iter()
        .filter_map(|dir| {
            let meta = Session::at(dir).meta().ok()?;
            let waiting = if meta.status == Status::Waiting {
                open_card_kind(dir)
            } else {
                None
            };
            let compacting = state.runner.is_compacting(&meta.id);
            let project = config.project_for(&meta.workspace, meta.requested_workspace.as_deref());
            let worktree = removes_worktree(&state.root, &meta);
            let title = meta
                .title
                .filter(|title| !title.trim().is_empty())
                .or_else(|| meta.description.filter(|title| !title.trim().is_empty()));
            Some(SessionRow {
                id: meta.id,
                workspace: meta.workspace,
                status: meta.status,
                updated_at: meta.updated_at,
                created_at: meta.created_at,
                waiting,
                pull_url: meta.pull_url,
                compacting,
                yolo: meta.yolo,
                enhance: meta.enhance,
                show_closeout: meta.show_closeout,
                profile: meta.profile,
                provider: meta
                    .model_override
                    .as_ref()
                    .and_then(|selection| selection.provider.clone())
                    .or_else(|| config.provider.clone()),
                model: meta.model,
                effort: meta.effort,
                title,
                project,
                parent_id: meta.parent_id,
                isolation: meta.isolation,
                hidden: meta.hidden,
                worktree,
                allow: meta.allow,
            })
        })
        .collect();
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    for row in &rows {
        if row.status == Status::Idle && row.pull_url.is_none() {
            state.runner.maybe_fill_pull(&row.id);
        }
    }
    Ok(Json(rows))
}

async fn server_info(
    State(state): State<AppState>,
) -> Result<Json<crate::pairing::ServerInfo>, ApiError> {
    let config = state.runner.current_config();
    let workspace = std::env::current_dir().map_err(|error| ApiError::server(error.to_string()))?;
    let name = std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_string())
        .unwrap_or_else(|| "Kyoto Agent".to_string());
    Ok(Json(crate::pairing::ServerInfo {
        name,
        version: env!("CARGO_PKG_VERSION").to_string(),
        workspace: workspace.display().to_string(),
        model: config.model,
        effort: config.effort,
        yolo: config.yolo,
        enhance: config.enhance,
        show_closeout: config.show_closeout,
        profile: config.profile,
    }))
}

#[derive(Deserialize)]
struct ProjectRequest {
    id: String,
    name: String,
    path: String,
}

async fn add_project(
    State(state): State<AppState>,
    Json(body): Json<ProjectRequest>,
) -> Result<impl IntoResponse, ApiError> {
    save_project(&state, &body, false)?;
    Ok(StatusCode::CREATED)
}

async fn edit_project(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<ProjectRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if id != body.id {
        return Err(ApiError::bad_request(
            "Project ID does not match the selected project.",
        ));
    }
    save_project(&state, &body, true)?;
    Ok(StatusCode::NO_CONTENT)
}

fn save_project(state: &AppState, body: &ProjectRequest, editing: bool) -> Result<(), ApiError> {
    if !Config::is_provider_id(&body.id) {
        return Err(ApiError::bad_request(
            "Use lowercase letters, digits, and dashes for the project ID.",
        ));
    }
    if body.name.trim().is_empty() || body.name.chars().any(char::is_control) {
        return Err(ApiError::bad_request("Enter a project name."));
    }
    let path = Path::new(&body.path);
    if !path.is_absolute() || !path.is_dir() {
        return Err(ApiError::bad_request(
            "Enter an existing absolute directory on this server.",
        ));
    }
    let path = fs::canonicalize(path).map_err(ApiError::bad_request)?;
    let _editing = state
        .config_edit
        .lock()
        .map_err(|_| ApiError::server("Configuration is busy."))?;
    let config = state.runner.current_config();
    let exists = config.projects.iter().any(|project| project.id == body.id);
    if exists != editing {
        return Err(ApiError::conflict(if exists {
            "Project ID already exists."
        } else {
            "Project no longer exists."
        }));
    }
    let config_path = state.root.join(crate::config::CONFIG_FILE);
    Config::edit(&config_path, |doc| {
        if doc.get("projects").is_none() {
            doc["projects"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        if !editing {
            doc["projects"][&body.id] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        doc["projects"][&body.id]["name"] = toml_edit::value(body.name.trim());
        doc["projects"][&body.id]["path"] = toml_edit::value(path.to_string_lossy().as_ref());
    })
    .map_err(ApiError::server)?;
    Ok(())
}

async fn get_layout(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.runner.current_config().layout)
}

async fn save_layout(
    State(state): State<AppState>,
    Json(layout): Json<crate::config::Layout>,
) -> Result<StatusCode, ApiError> {
    layout.validate().map_err(ApiError::bad_request)?;
    let _editing = state
        .config_edit
        .lock()
        .map_err(|_| ApiError::server("Configuration is busy."))?;
    Config::edit(&state.root.join(crate::config::CONFIG_FILE), |doc| {
        let mut table = toml_edit::Table::new();
        table["left_open"] = toml_edit::value(layout.left_open);
        table["left_width"] = toml_edit::value(i64::from(layout.left_width));
        table["right_open"] = toml_edit::value(layout.right_open);
        table["right_width"] = toml_edit::value(i64::from(layout.right_width));
        let mut panes = toml_edit::Array::new();
        for pane in &layout.right_panes {
            panes.push(pane.name());
        }
        table["right_panes"] = toml_edit::value(panes);
        doc["layout"] = toml_edit::Item::Table(table);
    })
    .map_err(ApiError::server)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_projects(State(state): State<AppState>) -> impl IntoResponse {
    Json(repository_rows(&state.runner.current_config()))
}

fn repository_rows(config: &Config) -> Vec<crate::pairing::Repository> {
    config
        .projects
        .iter()
        .map(|project| crate::pairing::Repository {
            id: project.id.clone(),
            name: project.name.clone(),
            path: project.path.display().to_string(),
            yolo: project.yolo,
        })
        .collect()
}

async fn pair_client(
    State(state): State<AppState>,
    code: Option<Extension<https::PairingCode>>,
    Json(client): Json<crate::pairing::PairingClient>,
) -> Result<Json<crate::pairing::PairingInfo>, ApiError> {
    if client.name.trim().is_empty() || client.version.trim().is_empty() {
        return Err(ApiError::bad_request(
            "client name and version are required",
        ));
    }
    let server = server_info(State(state.clone())).await?.0;
    let models = state.runner.list_models().await;
    let repositories = repository_rows(&state.runner.current_config());
    let key = crate::pairing::PairingKey::load(&state.root).map_err(ApiError::server)?;
    let access_token = match code {
        Some(Extension(code)) => key.exchange(&code.0).map_err(|message| ApiError {
            status: StatusCode::UNAUTHORIZED,
            message,
        })?,
        None => key.token().map_err(ApiError::server)?,
    };
    Ok(Json(crate::pairing::PairingInfo {
        access_token,
        server,
        client,
        models,
        repositories,
    }))
}

/// The quiet view of one session: status, cards, revision.
async fn view_session(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    match state.runner.view(&id) {
        Ok(view) => Ok(Json(view)),
        Err(SessionError::MissingMeta { .. }) => Err(ApiError::not_found()),
        Err(source) => Err(ApiError::server(source.to_string())),
    }
}

/// The raw log of one session, as it sits on disk.
async fn events(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let dir = session_dir(&state.root, &id);
    if Session::at(&dir).meta().is_err() {
        return Err(ApiError::not_found());
    }
    match fs::read_to_string(dir.join(EVENTS_FILE)) {
        Ok(text) => Ok(text_response(text)),
        Err(_) => Err(ApiError::not_found()),
    }
}

/// Start a turn on a session. A free session gets 202 and its turn id. A live
/// turn or a compact queues a non-empty ask and gets 202 `{ "queued": true }`.
/// A waiting permission or question, or a ninth queued ask, gets 409.
async fn message(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<MessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    crate::attachment::validate_images(&body.images).map_err(ApiError::bad_request)?;
    match state
        .runner
        .ask_with_images(&id, &body.text, body.enhance, body.images)
    {
        Ok(crate::turn::AskOutcome::Started(turn_id)) => Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "turnId": turn_id })),
        )),
        Ok(crate::turn::AskOutcome::Enhancing(id)) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({ "id": id, "state": "enhancing" })),
        )),
        Ok(crate::turn::AskOutcome::Queued(id)) => Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "queued": true, "queuedId": id })),
        )),
        Ok(crate::turn::AskOutcome::Ignored) => {
            Ok((StatusCode::ACCEPTED, Json(serde_json::json!({}))))
        }
        Err(TurnError::NoSession) => Err(ApiError::not_found()),
        Err(TurnError::Busy) => Err(ApiError::conflict("the session is already working")),
        Err(TurnError::QueueFull) => Err(ApiError::conflict("the queue is full")),
        Err(TurnError::Goal(message)) => Err(ApiError::bad_request(message)),
        Err(source) => Err(ApiError::server(source.to_string())),
    }
}

async fn remove_queued(
    State(state): State<AppState>,
    AxumPath((id, queued_id)): AxumPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    match state.runner.remove_queued(&id, &queued_id) {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::conflict("the message is no longer queued")),
        Err(TurnError::NoSession) => Err(ApiError::not_found()),
        Err(source) => Err(ApiError::server(source.to_string())),
    }
}

async fn compact(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    match state.runner.compact(&id) {
        Ok(()) => Ok(StatusCode::ACCEPTED),
        Err(TurnError::NoSession) => Err(ApiError::not_found()),
        Err(source) => Err(ApiError::server(source.to_string())),
    }
}

/// Answer the open card. The card's kind decides which answer it takes: a
/// permission takes one of the three decisions, a question takes the reply
/// text. A card that is already settled is 409.
async fn answer(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<AnswerRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let dir = session_dir(&state.root, &id);
    let events = Session::at(&dir)
        .events()
        .map_err(|_| ApiError::not_found())?;
    let Some(card) = events.iter().find(|event| event.id == body.id) else {
        return Err(ApiError::not_found());
    };
    match card.kind {
        EventKind::Permission => {
            let Some(decision) = parse_choice(&body.choice) else {
                return Err(ApiError::bad_request(
                    "a permission takes allow_once, allow_session, or deny",
                ));
            };
            let answer = Answer::new(decision).for_permission(&body.id);
            match state.runner.answer(&id, answer) {
                Ok(()) => Ok(StatusCode::NO_CONTENT),
                Err(source) => Err(ApiError::conflict(source.to_string())),
            }
        }
        EventKind::Question => match state.runner.answer_question(&id, &body.choice) {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(source) => Err(ApiError::conflict(source.to_string())),
        },
        EventKind::Enhance => {
            match state
                .runner
                .answer_enhance(&id, &body.id, &body.choice, &body.text)
                .await
            {
                Ok(()) => Ok(StatusCode::NO_CONTENT),
                Err(crate::turn::EnhanceError::Missing) => Err(ApiError::not_found()),
                Err(source @ crate::turn::EnhanceError::Settled)
                | Err(source @ crate::turn::EnhanceError::Busy) => {
                    Err(ApiError::conflict(source.to_string()))
                }
                Err(source @ crate::turn::EnhanceError::Choice)
                | Err(source @ crate::turn::EnhanceError::EmptyRevise)
                | Err(source @ crate::turn::EnhanceError::NoDraft) => {
                    Err(ApiError::bad_request(source.to_string()))
                }
                Err(crate::turn::EnhanceError::Failed(message)) => Err(ApiError::server(message)),
            }
        }
        _ => Err(ApiError::bad_request("that card takes no answer")),
    }
}

/// Stop the turn of a session. Other sessions keep running.
async fn cancel(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    if Session::at(&session_dir(&state.root, &id)).meta().is_err() {
        return Err(ApiError::not_found());
    }
    state.runner.cancel(&id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Default, Deserialize)]
struct DeleteOptions {
    #[serde(default)]
    delete_workspace: bool,
    #[serde(default)]
    confirm_dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceStatus {
    pub managed: bool,
    pub path: String,
    pub changes: Vec<String>,
}

async fn workspace_status(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<WorkspaceStatus>, ApiError> {
    let meta = Session::at(&session_dir(&state.root, &id))
        .meta()
        .map_err(|_| ApiError::not_found())?;
    Ok(Json(check_workspace(&state.root, &meta)?))
}

fn check_workspace(root: &Path, meta: &SessionMeta) -> Result<WorkspaceStatus, ApiError> {
    let managed = removes_worktree(root, meta);
    let mut status = WorkspaceStatus {
        managed,
        path: meta.workspace.clone(),
        changes: Vec::new(),
    };
    if managed && Path::new(&meta.workspace).exists() {
        let output = Command::new("git")
            .args([
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignored",
            ])
            .current_dir(&meta.workspace)
            .output()
            .map_err(ApiError::server)?;
        if !output.status.success() {
            return Err(ApiError::conflict(
                "Cannot check workspace changes. Keep the workspace or retry.",
            ));
        }
        status.changes = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect();
    }
    Ok(status)
}

fn remove_workspace(root: &Path, meta: &SessionMeta, confirm_dirty: bool) -> Result<(), ApiError> {
    let status = check_workspace(root, meta)?;
    if !status.managed {
        return Err(ApiError::conflict(
            "Only a managed worktree can be deleted.",
        ));
    }
    if !status.changes.is_empty() && !confirm_dirty {
        return Err(ApiError::conflict(
            "Workspace has uncommitted or untracked files. Confirm their deletion explicitly.",
        ));
    }
    drop_worktree(root, meta, confirm_dirty)
}

async fn delete_session(
    State(state): State<AppState>,
    Query(options): Query<DeleteOptions>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let dir = session_dir(&state.root, &id);
    let meta = Session::at(&dir)
        .meta()
        .map_err(|_| ApiError::not_found())?;
    state.runner.finish_for_delete(&id).await;
    if options.delete_workspace {
        remove_workspace(&state.root, &meta, options.confirm_dirty)?;
    }
    state.runner.forget(&id);
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(StatusCode::NO_CONTENT),
        Err(source) => Err(ApiError::server(source.to_string())),
    }
}

async fn delete_worktree(
    State(state): State<AppState>,
    Query(options): Query<DeleteOptions>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let dir = session_dir(&state.root, &id);
    let session = Session::at(&dir);
    let meta = session.meta().map_err(|_| ApiError::not_found())?;
    if !removes_worktree(&state.root, &meta) {
        return Err(ApiError::conflict("isolation is not worktree"));
    }
    let fallback =
        parent_workspace(&state.root, &meta).or_else(|| meta.requested_workspace.clone());
    state.runner.finish_for_delete(&id).await;
    remove_workspace(&state.root, &meta, options.confirm_dirty)?;
    session
        .update(|meta| {
            meta.isolation = None;
            if let Some(workspace) = fallback.clone() {
                meta.workspace = workspace;
            }
            true
        })
        .map_err(|source| ApiError::server(source.to_string()))?;
    state
        .runner
        .reload_tools(&id)
        .map_err(|source| ApiError::server(source.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

fn parent_workspace(root: &Path, meta: &SessionMeta) -> Option<String> {
    let id = meta.parent_id.as_deref()?;
    Session::at(&session_dir(root, id))
        .meta()
        .ok()
        .map(|parent| parent.workspace)
}

fn removes_worktree(root: &Path, meta: &SessionMeta) -> bool {
    meta.isolation.as_deref() == Some("worktree") || under_worktrees(root, &meta.workspace)
}

fn under_worktrees(root: &Path, workspace: &str) -> bool {
    let base = root.join("worktrees");
    let path = Path::new(workspace);
    path.starts_with(&base) && path != base
}

fn drop_worktree(root: &Path, meta: &SessionMeta, force: bool) -> Result<(), ApiError> {
    let dest = PathBuf::from(&meta.workspace);
    if !dest.exists() {
        return Ok(());
    }
    let repo = parent_workspace(root, meta)
        .or_else(|| meta.requested_workspace.clone())
        .map(PathBuf::from)
        .ok_or_else(|| ApiError::conflict("The worktree repository is unknown."))?;
    let mut command = Command::new("git");
    command.args(["worktree", "remove"]);
    if force {
        command.arg("--force");
    }
    let output = command
        .arg(&dest)
        .current_dir(repo)
        .output()
        .map_err(ApiError::server)?;
    if !output.status.success() {
        return Err(ApiError::conflict(
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct YoloRequest {
    yolo: bool,
}

async fn list_profiles(State(state): State<AppState>) -> impl IntoResponse {
    let names: Vec<String> = state
        .runner
        .current_config()
        .profiles
        .iter()
        .map(|profile| profile.id.clone())
        .collect();
    Json(names)
}

#[derive(Deserialize)]
struct ProfileRequest {
    profile: String,
}

async fn set_profile(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<ProfileRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let session = Session::at(&session_dir(&state.root, &id));
    if session.meta().is_err() {
        return Err(ApiError::not_found());
    }
    let name = body.profile.trim();
    if name.is_empty() {
        session
            .set_profile(None)
            .map_err(|source| ApiError::server(source.to_string()))?;
        return Ok(StatusCode::NO_CONTENT);
    }
    if state.runner.current_config().profile(name).is_none() {
        return Err(ApiError::bad_request(format!("unknown profile: {name}")));
    }
    session
        .set_profile(Some(name))
        .map_err(|source| ApiError::server(source.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_yolo(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<YoloRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let session = Session::at(&session_dir(&state.root, &id));
    if session.meta().is_err() {
        return Err(ApiError::not_found());
    }
    session
        .set_yolo(body.yolo)
        .map_err(|source| ApiError::server(source.to_string()))?;
    if body.yolo {
        let _ = state.runner.answer(&id, Answer::allow_once());
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct EnhanceRequest {
    enhance: bool,
}

async fn set_enhance(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<EnhanceRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let session = Session::at(&session_dir(&state.root, &id));
    if session.meta().is_err() {
        return Err(ApiError::not_found());
    }
    session
        .set_enhance(body.enhance)
        .map_err(|source| ApiError::server(source.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct CloseoutShowRequest {
    show: bool,
}

async fn set_closeout_show(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<CloseoutShowRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let session = Session::at(&session_dir(&state.root, &id));
    if session.meta().is_err() {
        return Err(ApiError::not_found());
    }
    session
        .set_show_closeout(body.show)
        .map_err(|source| ApiError::server(source.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ModelRequest {
    model: String,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    provider: Option<String>,
}

async fn set_session_model(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<ModelRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let session = Session::at(&session_dir(&state.root, &id));
    if session.meta().is_err() {
        return Err(ApiError::not_found());
    }
    let config = state.runner.current_config();
    let rows = state.runner.list_models().await;
    let row = rows
        .iter()
        .find(|row| {
            row.matches(&body.model)
                && body.provider.as_ref().is_none_or(|provider| {
                    row.provider.as_ref().or(config.provider.as_ref()) == Some(provider)
                })
        })
        .ok_or_else(|| ApiError::bad_request("model is not available from this provider"))?;
    let provider = row.provider.clone().or(config.provider);
    session
        .set_session_model(crate::session::SessionModel {
            model: body.model,
            effort: body.effort,
            provider,
        })
        .map_err(|source| ApiError::server(source.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_model(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<ModelRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let session = Session::at(&session_dir(&state.root, &id));
    if session.meta().is_err() {
        return Err(ApiError::not_found());
    }
    let path = state.root.join(crate::config::CONFIG_FILE);
    let rows = state.runner.list_models().await;
    let current_provider = state.runner.current_config().provider;
    let row = rows.iter().find(|row| {
        row.matches(&body.model)
            && body.provider.as_ref().is_none_or(|provider| {
                row.provider.as_ref() == Some(provider)
                    || (row.provider.is_none() && current_provider.as_ref() == Some(provider))
            })
    });
    if body.provider.is_some() && row.is_none() {
        return Err(ApiError::bad_request(
            "model is not available from this provider",
        ));
    }
    Config::set_model_options(
        &path,
        row.and_then(|row| row.provider.as_deref()),
        &body.model,
        Some(body.effort.as_deref()),
    )
    .map_err(|source| ApiError::server(source.to_string()))?;
    session
        .set_model_effort(&body.model, body.effort.as_deref())
        .map_err(|source| ApiError::server(source.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn read_task(
    State(state): State<AppState>,
    AxumPath((id, task_id)): AxumPath<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    match state.runner.task(&id, &task_id) {
        Ok(task) => Ok(Json(task)),
        Err(message) => Err(ApiError {
            status: StatusCode::NOT_FOUND,
            message,
        }),
    }
}

#[derive(Deserialize)]
struct ModelQuery {
    #[serde(default)]
    diagnostics: bool,
}

async fn list_models(State(state): State<AppState>, Query(query): Query<ModelQuery>) -> Response {
    let catalog = state.runner.model_catalog().await;
    if query.diagnostics {
        Json(catalog).into_response()
    } else {
        Json(catalog.models).into_response()
    }
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

#[derive(Serialize)]
struct FileBody {
    path: String,
    text: String,
    truncated: bool,
}

async fn read_session_file(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FileQuery>,
) -> Result<impl IntoResponse, ApiError> {
    if query.path.is_empty() {
        return Err(ApiError::bad_request("path is required"));
    }
    let session = Session::at(&session_dir(&state.root, &id));
    if session.meta().is_err() {
        return Err(ApiError::not_found());
    }
    let tools = Tools::at(&session).map_err(|source| ApiError::server(source.to_string()))?;
    match tools.read_workspace_file(&query.path) {
        Ok(file) => Ok(Json(FileBody {
            path: file.path,
            text: file.text,
            truncated: file.truncated,
        })),
        Err(ToolError::Outside { .. }) => {
            Err(ApiError::bad_request("path is outside the workspace"))
        }
        Err(ToolError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(ApiError {
                status: StatusCode::NOT_FOUND,
                message: "no such file".to_string(),
            })
        }
        Err(ToolError::Io { source, .. }) if source.kind() == std::io::ErrorKind::InvalidData => {
            Err(ApiError::bad_request("the file is not text"))
        }
        Err(source) => Err(ApiError::bad_request(source.to_string())),
    }
}

/// The raw log as plain text.
fn text_response(text: String) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        text,
    )
        .into_response()
}

#[cfg(test)]
mod ios_fixtures {
    use super::{FileBody, SessionRow};
    use crate::chat::ModelRow;
    use crate::compact::{BucketId, ContextBucket, ContextUsage};
    use crate::events::{
        AskBody, EnhanceBody, Event, EventKind, PermissionBody, ProofBody, ProofItem,
        QuestionAnswerBody, QuestionBody, ResultBody, TaskStatus, TodoItem, TodoStatus,
        ToolCallBody,
    };
    use crate::pairing::Repository as ProjectRow;
    use crate::prompt::SkillEntry;
    use crate::screen::{Phase, Status};
    use crate::session::AllowList;
    use crate::task::TaskView;
    use crate::view::{cards, CloseoutRow, CloseoutStatus, ScheduleItem, TaskItem, View};
    use serde::Serialize;
    use serde_json::Value;
    use std::fs;
    use std::path::PathBuf;

    const AT: &str = "2026-10-01T21:00:00.000Z";

    fn event(id: &str, kind: EventKind, body: impl Serialize) -> Event {
        Event::new(id, AT, "t1", kind)
            .with_body(&body)
            .expect("a fixture body serializes")
    }

    fn assert_fixture(name: &str, value: &impl Serialize) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ios/Fixtures")
            .join(name);
        let rendered = serde_json::to_string_pretty(value).expect("the handler type serializes");
        let rendered = format!("{rendered}\n");
        if std::env::var_os("UPDATE_IOS_FIXTURES").is_some() {
            fs::create_dir_all(path.parent().expect("ios/Fixtures")).expect("create ios/Fixtures");
            fs::write(&path, &rendered).expect("write the fixture");
            return;
        }
        let saved = fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!("{} could not be read: {err}\n{rendered}", path.display())
        });
        assert_eq!(
            saved, rendered,
            "{name} drifted from the handler JSON. Set UPDATE_IOS_FIXTURES=1 to rewrite ios/Fixtures/{name}"
        );
    }

    fn sessions() -> Vec<SessionRow> {
        let child_allow = AllowList {
            write_paths: vec!["/home/pascal/work/kyotoagent/src/view.rs".into()],
            argv: vec![vec!["cargo".into(), "test".into(), "--offline".into()]],
            fetch_origins: vec!["https://github.com".into()],
            ..AllowList::default()
        };
        vec![
            SessionRow {
                id: "c0ffee01".into(),
                workspace: "/home/pascal/.kyotoagent/worktrees/kyotoagent-c0ffee01".into(),
                status: Status::Waiting,
                updated_at: "2026-10-01T21:40:00.000Z".into(),
                created_at: "2026-10-01T21:40:00.000Z".into(),
                waiting: Some("permission"),
                pull_url: Some("https://github.com/pmdroid/kyotoagent/pull/92".into()),
                compacting: true,
                yolo: true,
                enhance: false,
                show_closeout: true,
                profile: None,
                model: "grok-4.7".into(),
                provider: None,
                effort: Some("medium".into()),
                title: Some("Permission cards".into()),
                project: Some("kyotoagent".into()),
                parent_id: Some("a11a0001".into()),
                isolation: Some("worktree".into()),
                hidden: false,
                worktree: true,
                allow: child_allow,
            },
            SessionRow {
                id: "a11a0001".into(),
                workspace: "/home/pascal/work/kyotoagent".into(),
                status: Status::Waiting,
                updated_at: "2026-10-01T21:10:00.000Z".into(),
                created_at: "2026-10-01T21:00:00.000Z".into(),
                waiting: Some("question"),
                pull_url: None,
                compacting: false,
                yolo: false,
                enhance: true,
                show_closeout: true,
                profile: None,
                model: "grok-4.7".into(),
                provider: None,
                effort: Some("high".into()),
                title: Some("Kyoto Agent".into()),
                project: Some("kyotoagent".into()),
                parent_id: None,
                isolation: None,
                hidden: false,
                worktree: false,
                allow: AllowList::default(),
            },
        ]
    }

    fn view_document() -> View {
        let events = vec![
            event(
                "e-ask",
                EventKind::UserAsk,
                AskBody {
                    images: Vec::new(),
                    text: "Add eventId to the permission card.".into(),
                    context: String::new(),
                    skill: String::new(),
                    silent: false,
                },
            ),
            event(
                "e-open",
                EventKind::Question,
                QuestionBody {
                    text: "Which title?".into(),
                    choices: vec!["Kyoto Agent".into(), "Kyoto Agent CLI".into()],
                },
            ),
            event(
                "e-settled",
                EventKind::Question,
                QuestionBody {
                    text: "Keep the socket name?".into(),
                    choices: vec!["yes".into(), "no".into()],
                },
            ),
            event(
                "e-answer",
                EventKind::QuestionAnswer,
                QuestionAnswerBody {
                    question_id: Some("e-settled".into()),
                    answer: "yes".into(),
                },
            ),
            event(
                "e-tool",
                EventKind::ToolCall,
                ToolCallBody {
                    tool: "read_file".into(),
                    args: serde_json::json!({ "path": "quiet-read-marker" }),
                },
            ),
            event(
                "e-perm",
                EventKind::Permission,
                PermissionBody::write(
                    "Replace src/view.rs",
                    "/home/pascal/work/kyotoagent/src/view.rs",
                    &["+        \"eventId\": event.id,"],
                ),
            ),
            event(
                "e-result",
                EventKind::Result,
                ResultBody {
                    text: "Permission cards include eventId.".into(),
                    note: String::new(),
                },
            ),
            event(
                "e-proof",
                EventKind::Proof,
                ProofBody {
                    text: "cargo test passed.".into(),
                    items: vec![ProofItem {
                        id: "cargo-test".into(),
                        kind: "command".into(),
                        outcome: "passed".into(),
                        argv: Vec::new(),
                        exit: None,
                        tail: String::new(),
                    }],
                    ..ProofBody::default()
                },
            ),
            event(
                "e-enhance",
                EventKind::Enhance,
                EnhanceBody {
                    text: "Rewrite the permission card.".into(),
                    source: "Add eventId to the permission card.".into(),
                    model: "grok-4.7".into(),
                    error: None,
                },
            ),
        ];
        let projected = cards(&events);
        assert!(
            !serde_json::to_string(&projected)
                .expect("cards")
                .contains("quiet-read-marker"),
            "a tool call stays off the card list"
        );
        let permission = projected
            .iter()
            .find(|card| card.kind == crate::view::CardKind::Permission)
            .expect("an open permission");
        assert_eq!(permission.body["eventId"], Value::from("e-perm"));
        assert!(permission.body["decision"].is_null());
        let open_question = projected
            .iter()
            .find(|card| {
                card.kind == crate::view::CardKind::Question && card.body["answer"].is_null()
            })
            .expect("an open question");
        assert_eq!(open_question.body["eventId"], Value::from("e-open"));
        let enhance = projected
            .iter()
            .find(|card| card.kind == crate::view::CardKind::Enhance)
            .expect("an enhance card");
        assert_eq!(enhance.body["eventId"], Value::from("e-enhance"));
        assert_eq!(
            enhance.body["text"],
            Value::from("Rewrite the permission card.")
        );
        assert_eq!(
            enhance.body["source"],
            Value::from("Add eventId to the permission card.")
        );
        assert_eq!(enhance.body["model"], Value::from("grok-4.7"));
        assert!(enhance.body.get("error").is_none());
        View {
            status: Status::Waiting,
            cards: projected,
            revision: events.len(),
            skills: vec![SkillEntry {
                name: "preflight".into(),
                description: "Closeout for a change.".into(),
                disable_model_invocation: false,
                user_invocable: true,
                path: "/home/pascal/.agents/skills/preflight/SKILL.md".into(),
            }],
            todos: vec![TodoItem {
                id: "todo-1".into(),
                title: "Add eventId".into(),
                status: TodoStatus::InProgress,
                description: None,
                files: Vec::new(),
                links: Vec::new(),
            }],
            tasks: vec![TaskItem {
                id: "task01".into(),
                argv: vec!["cargo".into(), "test".into(), "--offline".into()],
                state: TaskStatus::Running,
            }],
            schedules: vec![ScheduleItem {
                id: "sched01".into(),
                note: "Check the pull request".into(),
                due_at: "2026-10-01T22:30:00.000Z".into(),
                remaining_min: 30,
            }],
            phase: Some(Phase::Thinking),
            action: None,
            thinking: Some("Reading permission_body.".into()),
            retry_status: None,
            queue_items: Vec::new(),
            queue: vec!["Check the fixture diff.".into()],
            allow: AllowList {
                write_paths: vec!["/home/pascal/work/kyotoagent/src/view.rs".into()],
                ..AllowList::default()
            },
            closeout: vec![CloseoutRow {
                runs: Vec::new(),
                id: "cargo-test".into(),
                kind: "command".into(),
                hint: "cargo test must pass".into(),
                required: true,
                status: CloseoutStatus::Passed,
                exit: Some(0),
                attempt: Some(1),
                tail: "ok".into(),
            }],
            goal: None,
            context: Some(ContextUsage {
                used: 12_000,
                reported_prompt_tokens: None,
                window: Some(128_000),
                percent: Some(9),
                buckets: vec![
                    ContextBucket {
                        id: BucketId::System,
                        tokens: Some(2_000),
                    },
                    ContextBucket {
                        id: BucketId::Messages,
                        tokens: Some(10_000),
                    },
                ],
            }),
        }
    }

    #[test]
    fn ios_fixtures_match_the_handler_types() {
        let rows = sessions();
        assert!(rows[0].created_at > rows[1].created_at);
        assert_eq!(rows[0].parent_id.as_deref(), Some("a11a0001"));
        assert_eq!(rows[0].waiting, Some("permission"));
        assert_eq!(rows[1].waiting, Some("question"));
        assert!(rows[0].yolo);
        assert!(rows[0].compacting);
        assert!(rows[0].pull_url.is_some());
        assert_eq!(rows[0].isolation.as_deref(), Some("worktree"));
        assert_fixture("sessions.json", &rows);

        let view = view_document();
        assert_fixture("view.json", &view);

        let models = vec![
            ModelRow {
                id: "grok-4.7".into(),
                aliases: vec!["grok".into()],
                reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
                context_length: Some(2_000_000),
                provider: Some("grok".into()),
            },
            ModelRow {
                id: "cursor/composer".into(),
                aliases: Vec::new(),
                reasoning_efforts: Vec::new(),
                context_length: None,
                provider: None,
            },
        ];
        assert_fixture("models.json", &models);

        let projects = vec![
            ProjectRow {
                id: "kyotoagent".into(),
                name: "kyotoagent".into(),
                path: "/home/pascal/work/kyotoagent".into(),
                yolo: Some(true),
            },
            ProjectRow {
                id: "notes".into(),
                name: "notes".into(),
                path: "/home/pascal/notes".into(),
                yolo: None,
            },
        ];
        assert_fixture("projects.json", &projects);

        assert_fixture(
            "file.json",
            &FileBody {
                path: "src/view.rs".into(),
                text: "fn permission_body\n".into(),
                truncated: false,
            },
        );

        assert_fixture(
            "task.json",
            &TaskView {
                id: "task01".into(),
                argv: vec!["cargo".into(), "test".into(), "--offline".into()],
                state: TaskStatus::Exited,
                exit: Some(0),
                tail: "test result: ok. 1 passed\n".into(),
            },
        );
    }
}
