use super::*;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;

#[derive(Default)]
pub(super) struct Runtime {
    pub addr: Mutex<Option<SocketAddr>>,
    pub sender: Mutex<Option<tokio::sync::mpsc::Sender<HttpsListener>>>,
    configure: tokio::sync::Mutex<()>,
}

pub(super) struct Lifetime(pub Arc<Runtime>);

#[derive(Clone)]
pub(super) struct PairingCode(pub String);

impl Drop for Lifetime {
    fn drop(&mut self) {
        self.0.sender.lock().expect("https sender").take();
        self.0.addr.lock().expect("https addr").take();
    }
}

pub(super) fn authenticated(router: Router, root: &Path) -> Result<Router, ServerError> {
    let key = crate::pairing::PairingKey::load(root).map_err(ServerError::Tls)?;
    Ok(router.layer(axum::middleware::from_fn(
        move |mut request: axum::extract::Request, next: axum::middleware::Next| {
            let key = key.clone();
            async move {
                let token = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("Bearer "));
                let accepted = token.is_some_and(|token| key.accepts(token));
                let code = token.filter(|token| {
                    request.method() == axum::http::Method::POST
                        && request.uri().path() == "/v1/pair"
                        && key.accepts_code(token)
                }).map(str::to_string);
                if accepted || code.is_some() {
                    if let Some(code) = code {
                        request.extensions_mut().insert(PairingCode(code));
                    }
                    request.extensions_mut().insert(RemoteClient);
                    next.run(request).await
                } else {
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({"error": "valid access token or unexpired pairing code required"})),
                    )
                        .into_response()
                }
            }
        },
    )))
}

pub(super) async fn bind(listen: &Listen) -> Result<HttpsListener, ServerError> {
    let addr: SocketAddr = listen
        .addr
        .parse()
        .map_err(|_| ServerError::ListenAddr(listen.addr.clone()))?;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| ServerError::ListenBind {
            addr: listen.addr.clone(),
            source,
        })?;
    let tls = load_tls(&listen.cert, &listen.key)?;
    Ok(HttpsListener {
        listener,
        acceptor: TlsAcceptor::from(tls),
        handshakes: tokio::task::JoinSet::new(),
    })
}

#[derive(Serialize)]
pub(super) struct StatusBody {
    addr: Option<SocketAddr>,
}

pub(super) async fn status(State(state): State<AppState>) -> Json<StatusBody> {
    Json(StatusBody {
        addr: *state.https.addr.lock().expect("https addr"),
    })
}

#[derive(Deserialize)]
pub(super) struct EnableRequest {
    listen: String,
}

pub(super) async fn enable(
    State(state): State<AppState>,
    Json(request): Json<EnableRequest>,
) -> Result<Json<StatusBody>, ApiError> {
    let _configure = state.https.configure.lock().await;
    if state.https.addr.lock().expect("https addr").is_some() {
        return Err(ApiError::conflict("HTTPS is already listening"));
    }
    request
        .listen
        .parse::<SocketAddr>()
        .map_err(|_| ApiError::bad_request("use an IP address and port for listening"))?;
    let sender = state
        .https
        .sender
        .lock()
        .expect("https sender")
        .clone()
        .ok_or_else(|| ApiError::conflict("server is shutting down"))?;
    let permit = sender
        .reserve()
        .await
        .map_err(|_| ApiError::conflict("server is shutting down"))?;
    let path = state.root.join(crate::config::CONFIG_FILE);
    let config = match Config::load(&path) {
        Ok(config) => config,
        Err(crate::config::ConfigError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            state.runner.current_config()
        }
        Err(error) => return Err(ApiError::server(error.to_string())),
    };
    let listen = Listen {
        addr: request.listen,
        cert: resolve_under(
            &state.root,
            config.listen_cert.as_deref(),
            DEFAULT_LISTEN_CERT,
        ),
        key: resolve_under(
            &state.root,
            config.listen_key.as_deref(),
            DEFAULT_LISTEN_KEY,
        ),
    };
    ensure_certificate(&listen).map_err(ApiError::server)?;
    let listener = bind(&listen)
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let addr = listener.local_addr().map_err(ApiError::server)?;
    let active = state.https.sender.lock().expect("https sender");
    if active.is_none() {
        return Err(ApiError::conflict("server is shutting down"));
    }
    Config::set_listen(&path, &addr.to_string())
        .map_err(|error| ApiError::server(error.to_string()))?;
    *state.https.addr.lock().expect("https addr") = Some(addr);
    permit.send(listener);
    drop(active);
    Ok(Json(StatusBody { addr: Some(addr) }))
}

fn ensure_certificate(listen: &Listen) -> Result<(), String> {
    match (listen.cert.exists(), listen.key.exists()) {
        (true, true) => return Ok(()),
        (false, false) => {}
        _ => return Err("both the TLS certificate and key are required".to_string()),
    }
    let key = rcgen::KeyPair::generate().map_err(|error| error.to_string())?;
    let certificate = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .map_err(|error| error.to_string())?
        .self_signed(&key)
        .map_err(|error| error.to_string())?;
    let mut created = Vec::new();
    let result = (|| {
        for (path, text) in [
            (&listen.key, key.serialize_pem()),
            (&listen.cert, certificate.pem()),
        ] {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map_err(|error| error.to_string())?;
            created.push(path.clone());
            file.write_all(text.as_bytes())
                .map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    if result.is_err() {
        for path in created {
            let _ = fs::remove_file(path);
        }
    }
    result
}

#[derive(Deserialize)]
pub(super) struct ShareRequest {
    host: String,
}

pub(super) async fn share(
    State(state): State<AppState>,
    Json(request): Json<ShareRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let addr = state
        .https
        .addr
        .lock()
        .expect("https addr")
        .ok_or_else(|| ApiError::conflict("configure HTTPS before sharing this server"))?;
    let host = request.host.trim();
    let (base, _) = crate::pairing::connection(&format!("kyotoagent://{host}?token=placeholder"))
        .map_err(ApiError::bad_request)?;
    let url = reqwest::Url::parse(&base).map_err(ApiError::bad_request)?;
    if url.port_or_known_default() != Some(addr.port()) {
        return Err(ApiError::bad_request(
            "advertised port must match the HTTPS listener",
        ));
    }
    let token = crate::pairing::PairingKey::load(&state.root)
        .and_then(|key| key.code())
        .map_err(ApiError::server)?;
    Ok(Json(
        serde_json::json!({"uri": format!("kyotoagent://{host}?token={token}")}),
    ))
}
