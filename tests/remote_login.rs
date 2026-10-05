use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use kyotoagent::{auth, config::Config, server::Server, tui::Client};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

struct OAuth {
    grant: Semaphore,
    fail: AtomicBool,
    keys: Mutex<Vec<String>>,
}

async fn user_code() -> Json<Value> {
    Json(json!({"device_auth_id":"private-device-id","user_code":"ABCD-EFGH","interval":0}))
}

async fn grant(State(state): State<Arc<OAuth>>) -> Json<Value> {
    state.grant.acquire().await.unwrap().forget();
    Json(json!({"authorization_code":"private-grant","code_verifier":"private-verifier"}))
}

async fn exchange(State(state): State<Arc<OAuth>>) -> (StatusCode, Json<Value>) {
    if state.fail.load(Ordering::Relaxed) {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({"error":"private-token-error","access_token":"remote-access-secret","refresh_token":"remote-refresh-secret","id_token":"private-id-token"}),
            ),
        );
    }
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({auth::CODEX_AUTH_CLAIM:{"chatgpt_account_id":"remote-account"}}).to_string(),
    );
    (
        StatusCode::OK,
        Json(json!({
            "access_token":"remote-access-secret",
            "refresh_token":"remote-refresh-secret",
            "id_token":format!("e30.{claims}.signature"),
            "expires_in":3600
        })),
    )
}

async fn grok_code() -> Json<Value> {
    Json(
        json!({"device_code":"private-device","user_code":"GROK-CODE","verification_uri":"https://example.invalid/activate","interval":0}),
    )
}

async fn grok_token(State(state): State<Arc<OAuth>>) -> (StatusCode, Json<Value>) {
    if state.fail.load(Ordering::Relaxed) {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"expired_token"})),
        )
    } else {
        (
            StatusCode::OK,
            Json(
                json!({"access_token":"remote-grok-secret","refresh_token":"remote-grok-refresh","expires_in":3600}),
            ),
        )
    }
}

async fn models(State(state): State<Arc<OAuth>>, headers: axum::http::HeaderMap) -> Json<Value> {
    state.keys.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );
    Json(json!({"data":[{"id":"remote-model"}]}))
}

async fn chat(State(state): State<Arc<OAuth>>, headers: axum::http::HeaderMap) -> Json<Value> {
    state.keys.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );
    Json(json!({"choices":[{"message":{"role":"assistant","content":"hello"}}]}))
}

fn tls(root: &Path) {
    let certs = root.join("certs");
    fs::create_dir_all(&certs).unwrap();
    let mut params = rcgen::CertificateParams::default();
    params.subject_alt_names = vec![rcgen::SanType::IpAddress("127.0.0.1".parse().unwrap())];
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    fs::write(certs.join("server.crt"), cert.pem()).unwrap();
    fs::write(certs.join("server.key"), key.serialize_pem()).unwrap();
}

struct Fixture {
    root: PathBuf,
    uri: String,
    issuer: String,
    client: Client,
    oauth: Arc<OAuth>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "kyotoagent-remote-login-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        tls(&root);
        let oauth = Arc::new(OAuth {
            grant: Semaphore::new(0),
            fail: AtomicBool::new(false),
            keys: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route("/api/accounts/deviceauth/usercode", post(user_code))
            .route("/api/accounts/deviceauth/token", post(grant))
            .route("/oauth/token", post(exchange))
            .route("/grok/device", post(grok_code))
            .route("/grok/token", post(grok_token))
            .route("/v1/models", get(models))
            .route("/v1/chat/completions", post(chat))
            .with_state(Arc::clone(&oauth));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let oauth_task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let text = "base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"remote-model\"\nlisten = \"127.0.0.1:0\"\n";
        fs::write(root.join("config.toml"), text).unwrap();
        let config = Config::from_toml(text).unwrap();
        let server = Arc::new(Server::new(&root, &config).unwrap().with_login_clients(
            auth::AuthClient::at(
                &format!("{issuer}/grok/device"),
                &format!("{issuer}/grok/token"),
                "test",
            ),
            auth::CodexAuth::at(&issuer),
        ));
        let running = Arc::clone(&server);
        let server_task = tokio::spawn(async move {
            running.serve().await.unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let address = loop {
            if let Some(address) = server.https_addr() {
                break address;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let token = kyotoagent::pairing::PairingKey::load(&root)
            .unwrap()
            .token()
            .unwrap();
        let uri = format!("kyotoagent://{address}?token={token}");
        let client = Client::at_url(&uri).unwrap();
        Self {
            root,
            uri,
            issuer,
            client,
            oauth,
            tasks: vec![oauth_task, server_task],
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn wait_for(client: &Client, path: &str, expected: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, body) = client.request("GET", path, None).await.unwrap();
            assert_eq!(status, 200);
            let progress: Value = serde_json::from_str(&body).unwrap();
            if progress["status"] == expected {
                return progress;
            }
            assert_ne!(progress["status"], "failed", "{body}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn provider_login_returns_the_challenge_and_writes_only_server_credentials() {
    let fixture = Fixture::new("providers").await;
    let (status, body) = fixture
        .client
        .request("POST", "/v1/login/codex", None)
        .await
        .unwrap();
    assert_eq!(status, 202);
    let progress: Value = serde_json::from_str(&body).unwrap();
    let path = format!("/v1/login/{}", progress["id"].as_str().unwrap());
    let pending = wait_for(&fixture.client, &path, "pending").await;
    assert!(pending["verification_url"]
        .as_str()
        .unwrap()
        .ends_with("/codex/device"));
    assert_eq!(pending["user_code"], "ABCD-EFGH");
    assert!(!fixture.root.join(auth::CODEX_AUTH_FILE).exists());
    assert_eq!(
        fixture
            .client
            .request("POST", "/v1/login/codex", None)
            .await
            .unwrap()
            .0,
        409
    );
    fixture.oauth.grant.add_permits(1);
    let complete = wait_for(&fixture.client, &path, "complete").await;
    assert!(!complete.to_string().contains("secret"));
    for file in [auth::CODEX_AUTH_FILE, "config.toml"] {
        assert_eq!(
            fs::metadata(fixture.root.join(file))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        auth::load_codex(&fixture.root.join(auth::CODEX_AUTH_FILE))
            .unwrap()
            .access_token,
        "remote-access-secret"
    );
    assert!(Config::load(&fixture.root.join("config.toml"))
        .unwrap()
        .is_codex());
}

#[tokio::test]
async fn remote_progress_is_permissioned_and_provider_failures_are_sanitized() {
    let fixture = Fixture::new("errors").await;
    let address = kyotoagent::pairing::connection(&fixture.uri).unwrap().0;
    let invalid = Client::at_url(&format!(
        "{}?token=bad",
        fixture.uri.split('?').next().unwrap()
    ))
    .unwrap();
    let (status, _) = invalid
        .request("POST", "/v1/login/codex", None)
        .await
        .unwrap();
    assert_eq!(status, 401);
    let (status, _) = Client::at_url(&address)
        .unwrap()
        .request("GET", "/v1/login/unknown", None)
        .await
        .unwrap();
    assert_eq!(status, 401);
    for provider in ["codex", "grok"] {
        fixture.oauth.fail.store(true, Ordering::Relaxed);
        let (status, body) = fixture
            .client
            .request("POST", &format!("/v1/login/{provider}"), None)
            .await
            .unwrap();
        assert_eq!(status, 202);
        let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        fixture.oauth.grant.add_permits(1);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let (status, body) = fixture
                .client
                .request("GET", &format!("/v1/login/{id}"), None)
                .await
                .unwrap();
            assert_eq!(status, 200);
            for secret in [
                "remote-access-secret",
                "remote-refresh-secret",
                "private-token-error",
                "private-device-id",
                "private-grant",
                "private-verifier",
                "access_token",
                "refresh_token",
                "id_token",
            ] {
                assert!(!body.contains(secret), "{body}");
            }
            let data: Value = serde_json::from_str(&body).unwrap();
            if data["status"] == "failed" {
                assert_eq!(
                    data["error"],
                    if provider == "grok" {
                        "device code expired"
                    } else {
                        "provider answered HTTP 400"
                    }
                );
                break;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    assert!(!fixture.root.join(auth::CODEX_AUTH_FILE).exists());
    assert!(!fixture.root.join(auth::AUTH_FILE).exists());
}

#[tokio::test]
async fn successful_grok_progress_exposes_only_the_device_challenge_and_status() {
    let fixture = Fixture::new("grok").await;
    let (status, body) = fixture
        .client
        .request("POST", "/v1/login/grok", None)
        .await
        .unwrap();
    assert_eq!(status, 202);
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let (_, body) = fixture
            .client
            .request("GET", &format!("/v1/login/{id}"), None)
            .await
            .unwrap();
        let data: Value = serde_json::from_str(&body).unwrap();
        for key in data.as_object().unwrap().keys() {
            assert!(matches!(
                key.as_str(),
                "id" | "provider" | "status" | "verification_url" | "user_code"
            ));
        }
        assert!(!body.contains("remote-grok-secret"));
        assert!(!body.contains("remote-grok-refresh"));
        assert!(!body.contains("private-device"));
        if data["status"] == "complete" {
            assert_eq!(data["verification_url"], "https://example.invalid/activate");
            assert_eq!(data["user_code"], "GROK-CODE");
            break;
        }
        assert!(Instant::now() < deadline, "{body}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        auth::load(&fixture.root.join(auth::AUTH_FILE))
            .unwrap()
            .access_token,
        "remote-grok-secret"
    );
    for file in [auth::AUTH_FILE, "config.toml"] {
        assert_eq!(
            fs::metadata(fixture.root.join(file))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn cancelled_device_job_cannot_write_credentials_and_can_restart() {
    let fixture = Fixture::new("cancel").await;
    let (_, body) = fixture
        .client
        .request("POST", "/v1/login/codex", None)
        .await
        .unwrap();
    let progress: Value = serde_json::from_str(&body).unwrap();
    let path = format!("/v1/login/{}", progress["id"].as_str().unwrap());
    let (status, body) = fixture.client.request("DELETE", &path, None).await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["status"],
        "cancelled"
    );
    fixture.oauth.grant.add_permits(1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!fixture.root.join(auth::CODEX_AUTH_FILE).exists());
    assert!(!Config::load(&fixture.root.join("config.toml"))
        .unwrap()
        .is_codex());
    let (_, providers) = fixture
        .client
        .request("GET", "/v1/providers", None)
        .await
        .unwrap();
    let rows: Vec<Value> = serde_json::from_str(&providers).unwrap();
    assert!(rows.iter().find(|row| row["id"] == "codex").unwrap()["login"].is_null());
    let (status, _) = fixture
        .client
        .request("POST", "/v1/login/codex", None)
        .await
        .unwrap();
    assert_eq!(status, 202);
}

#[tokio::test]
async fn api_keys_stay_on_server_and_authenticate_chat_and_catalog() {
    let fixture = Fixture::new("keys").await;
    let text = format!("provider = \"office\"\n[providers.office]\nbase_url = \"{}/v1\"\nmodel = \"remote-model\"\n", fixture.issuer);
    fs::write(fixture.root.join("config.toml"), &text).unwrap();
    let key = "remote-api-key-secret";
    let body = json!({"api_key":key}).to_string();
    let socket = Client::at(fixture.root.join("kyotoagent.sock"));
    for client in [&fixture.client, &socket] {
        let (status, _) = client
            .request("POST", "/v1/providers/office/key", Some(&body))
            .await
            .unwrap();
        assert_eq!(status, 204);
        let (status, response) = client.request("GET", "/v1/providers", None).await.unwrap();
        assert_eq!(status, 200);
        assert!(!response.contains(key));
        let providers: Vec<Value> = serde_json::from_str(&response).unwrap();
        assert!(providers
            .iter()
            .any(|row| row["id"] == "office" && row["authenticated"] == true));
        let (status, _) = client.request("GET", "/v1/models", None).await.unwrap();
        assert_eq!(status, 200);
    }
    assert_eq!(
        fs::read_to_string(fixture.root.join("config.toml")).unwrap(),
        text
    );
    let path = auth::provider_key_path(&fixture.root, Some("office"));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let config = Config::load(&fixture.root.join("config.toml")).unwrap();
    let client = kyotoagent::chat::ChatClient::in_root(&config, Some(&fixture.root)).unwrap();
    client
        .complete(
            &[kyotoagent::chat::Message::User {
                content: "hello".into(),
            }],
            &[],
        )
        .await
        .unwrap();
    assert!(fixture
        .oauth
        .keys
        .lock()
        .unwrap()
        .iter()
        .all(|seen| seen == &format!("Bearer {key}")));
    assert_eq!(fixture.oauth.keys.lock().unwrap().len(), 3);
    for (id, input) in [
        ("missing", body),
        ("office", json!({"api_key":"bad\nsecret"}).to_string()),
        ("codex", json!({"api_key":key}).to_string()),
    ] {
        let (status, response) = fixture
            .client
            .request("POST", &format!("/v1/providers/{id}/key"), Some(&input))
            .await
            .unwrap();
        assert_eq!(status, 400);
        assert!(!response.contains("secret"));
    }
}

async fn popup_until(app: &mut kyotoagent::tui::App, client: &Client, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        kyotoagent::tui::apply(app, client, kyotoagent::tui::Effect::Paste(String::new()))
            .await
            .unwrap();
        if format!("{:?}", kyotoagent::tui::screen_model(app).overlay).contains(needle) {
            return;
        }
        assert!(Instant::now() < deadline, "waiting for {needle}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn provider_form_saves_on_the_selected_server_and_keeps_failed_input() {
    use kyotoagent::tui::{apply, screen_model, App, Effect};
    let fixture = Fixture::new("create-provider").await;
    let home = fixture.root.join("client-home");
    fs::create_dir_all(&home).unwrap();
    let config_path = fixture.root.join("config.toml");
    let original = fs::read_to_string(&config_path).unwrap();
    fs::write(&config_path, format!("{original}providers = {{ existing = {{ base_url = \"{}/v1\", model = \"remote-model\" }} }}\n", fixture.issuer)).unwrap();
    let mut app = App::new(fixture.root.clone(), home.clone(), String::new());
    app.ask = "unsent provider draft".into();
    apply(&mut app, &fixture.client, Effect::OpenProviders)
        .await
        .unwrap();
    popup_until(&mut app, &fixture.client, "Add OpenAI-compatible provider").await;
    apply(&mut app, &fixture.client, Effect::Choose(0))
        .await
        .unwrap();
    for value in [
        "existing",
        &format!("{}/v1", fixture.issuer),
        "remote-model",
        "provider-secret",
    ] {
        apply(&mut app, &fixture.client, Effect::Paste(value.into()))
            .await
            .unwrap();
        assert!(!format!("{:?}", screen_model(&app)).contains("provider-secret"));
        apply(&mut app, &fixture.client, Effect::Submit)
            .await
            .unwrap();
    }
    apply(&mut app, &fixture.client, Effect::Submit)
        .await
        .unwrap();
    popup_until(&mut app, &fixture.client, "already exists").await;
    assert!(!format!("{:?}", screen_model(&app)).contains("provider-secret"));
    assert!(!auth::provider_key_path(&fixture.root, Some("existing")).exists());
    for _ in 0..4 {
        apply(&mut app, &fixture.client, Effect::ScrollUp)
            .await
            .unwrap();
    }
    apply(&mut app, &fixture.client, Effect::DeleteLine)
        .await
        .unwrap();
    apply(&mut app, &fixture.client, Effect::Paste("meridian".into()))
        .await
        .unwrap();
    for _ in 0..5 {
        apply(&mut app, &fixture.client, Effect::Submit)
            .await
            .unwrap();
    }
    popup_until(&mut app, &fixture.client, "signed in").await;
    let config = Config::load(&config_path).unwrap();
    assert_eq!(
        config.providers["meridian"].base_url,
        format!("{}/v1", fixture.issuer)
    );
    assert_eq!(config.providers["meridian"].model, "remote-model");
    assert!(config.providers.contains_key("existing"));
    assert!(config.provider.is_none());
    assert_eq!(config.base_url, "http://127.0.0.1:1/v1");
    let key_path = auth::provider_key_path(&fixture.root, Some("meridian"));
    assert_eq!(
        auth::load_opencode_key(&key_path).unwrap(),
        "provider-secret"
    );
    assert_eq!(
        fs::metadata(key_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!fs::read_to_string(&config_path)
        .unwrap()
        .contains("provider-secret"));
    assert_eq!(app.ask, "unsent provider draft");
    assert!(!home.join(".kyotoagent").exists());
    let (_, body) = fixture
        .client
        .request("GET", "/v1/models", None)
        .await
        .unwrap();
    let rows: Vec<Value> = serde_json::from_str(&body).unwrap();
    assert!(rows
        .iter()
        .any(|row| row["provider"] == "meridian" && row["id"] == "remote-model"));
    assert!(fixture
        .oauth
        .keys
        .lock()
        .unwrap()
        .contains(&"Bearer provider-secret".into()));
}

#[tokio::test]
async fn provider_creation_validates_before_writing_and_allows_no_key() {
    let fixture = Fixture::new("validate-provider").await;
    let config_path = fixture.root.join("config.toml");
    let before = fs::read(&config_path).unwrap();
    for input in [
        json!({"id":"../escape", "base_url":fixture.issuer, "model":"remote-model"}),
        json!({"id":"codex", "base_url":fixture.issuer, "model":"remote-model"}),
        json!({"id":"new", "base_url":"file:///tmp/models", "model":"remote-model"}),
        json!({"id":"new", "base_url":"http://user:secret@localhost/v1", "model":"remote-model"}),
        json!({"id":"new", "base_url":fixture.issuer, "model":"  "}),
        json!({"id":"new", "base_url":fixture.issuer, "model":"remote-model", "api_key":"bad\nsecret"}),
    ] {
        let (status, response) = fixture
            .client
            .request("POST", "/v1/providers", Some(&input.to_string()))
            .await
            .unwrap();
        assert_eq!(status, 400, "{response}");
        assert!(!response.contains("secret"));
        assert_eq!(fs::read(&config_path).unwrap(), before);
    }
    let input = json!({"id":"local", "base_url":format!("{}/v1", fixture.issuer), "model":"remote-model", "api_key":""});
    let client = Client::at(fixture.root.join("kyotoagent.sock"));
    assert_eq!(
        client
            .request("POST", "/v1/providers", Some(&input.to_string()))
            .await
            .unwrap()
            .0,
        201
    );
    assert!(Config::load(&config_path)
        .unwrap()
        .providers
        .contains_key("local"));
    assert!(!auth::provider_key_path(&fixture.root, Some("local")).exists());
    fs::create_dir(auth::provider_key_path(&fixture.root, Some("blocked"))).unwrap();
    let input = json!({"id":"blocked", "base_url":fixture.issuer, "model":"remote-model", "api_key":"secret"});
    let (status, response) = client
        .request("POST", "/v1/providers", Some(&input.to_string()))
        .await
        .unwrap();
    assert_eq!(status, 500);
    assert!(!response.contains("secret"));
    let config = Config::load(&config_path).unwrap();
    assert!(config.providers.contains_key("local"));
    assert!(!config.providers.contains_key("blocked"));
}

#[tokio::test]
async fn model_picker_reports_provider_errors_and_keeps_available_models() {
    use kyotoagent::tui::{apply, screen_model, App, Effect};
    let fixture = Fixture::new("catalog-errors").await;
    fs::write(fixture.root.join("config.toml"), format!("provider = \"working\"\nproviders = {{ working = {{ base_url = \"{}/v1\", model = \"remote-model\" }}, meridian = {{ base_url = \"{}/missing\", model = \"claude-opus-5-5\" }} }}\n", fixture.issuer, fixture.issuer)).unwrap();
    let (status, body) = fixture
        .client
        .request("GET", "/v1/models?diagnostics=true", None)
        .await
        .unwrap();
    assert_eq!(status, 200);
    let catalog: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(catalog["models"][0]["id"], "remote-model");
    assert!(catalog["errors"]["meridian"]
        .as_str()
        .unwrap()
        .contains("404"));
    assert!(!body.contains("claude-opus-5-5"));
    let mut app = App::new(
        fixture.root.clone(),
        fixture.root.join("client-home"),
        String::new(),
    );
    app.ask = "model picker draft".into();
    apply(&mut app, &fixture.client, Effect::OpenModel)
        .await
        .unwrap();
    popup_until(&mut app, &fixture.client, "remote-model").await;
    let model = screen_model(&app);
    let error = model.toast.unwrap();
    assert!(
        error.contains("meridian") && error.contains("404"),
        "{error}"
    );
    assert_eq!(app.ask, "model picker draft");
    apply(&mut app, &fixture.client, Effect::CloseOverlay)
        .await
        .unwrap();
    fs::write(fixture.root.join("config.toml"), format!("provider = \"meridian\"\nproviders = {{ meridian = {{ base_url = \"{}/missing\", model = \"claude-opus-5-5\" }} }}\n", fixture.issuer)).unwrap();
    apply(&mut app, &fixture.client, Effect::OpenModel)
        .await
        .unwrap();
    popup_until(&mut app, &fixture.client, "meridian:").await;
    let overlay = format!("{:?}", screen_model(&app).overlay);
    assert!(
        overlay.contains("404") && overlay.contains("Enter retry"),
        "{overlay}"
    );
    assert!(!overlay.contains("claude-opus-5-5"));
}

#[tokio::test]
async fn provider_popup_saves_masked_key_on_remote_and_reopens_device_job() {
    use kyotoagent::tui::{apply, screen_model, App, Effect};
    let fixture = Fixture::new("popup").await;
    let config_before = fs::read(fixture.root.join("config.toml")).unwrap();
    let home = fixture.root.join("client-home");
    fs::create_dir_all(&home).unwrap();
    let mut app = App::new(fixture.root.clone(), home.clone(), String::new());
    app.ask = "unsent chat draft".into();
    apply(&mut app, &fixture.client, Effect::OpenProviders)
        .await
        .unwrap();
    popup_until(&mut app, &fixture.client, "default").await;
    apply(&mut app, &fixture.client, Effect::Choose(2))
        .await
        .unwrap();
    let key = "popup-api-key-secret";
    apply(&mut app, &fixture.client, Effect::Paste(key.into()))
        .await
        .unwrap();
    let model = screen_model(&app);
    assert!(!format!("{model:?}").contains(key));
    assert!(format!("{:?}", model.overlay).contains("••••"));
    apply(&mut app, &fixture.client, Effect::Submit)
        .await
        .unwrap();
    popup_until(&mut app, &fixture.client, "default").await;
    assert_eq!(
        auth::load_opencode_key(&auth::provider_key_path(&fixture.root, None)).unwrap(),
        key
    );
    assert!(!auth::provider_key_path(&home.join(".kyotoagent"), None).exists());
    assert_eq!(
        fs::read(fixture.root.join("config.toml")).unwrap(),
        config_before
    );
    assert_eq!(app.ask, "unsent chat draft");
    apply(&mut app, &fixture.client, Effect::Choose(1))
        .await
        .unwrap();
    popup_until(&mut app, &fixture.client, "ABCD-EFGH").await;
    apply(&mut app, &fixture.client, Effect::CloseOverlay)
        .await
        .unwrap();
    apply(&mut app, &fixture.client, Effect::OpenProviders)
        .await
        .unwrap();
    popup_until(&mut app, &fixture.client, "default").await;
    apply(&mut app, &fixture.client, Effect::Choose(1))
        .await
        .unwrap();
    assert!(format!("{:?}", screen_model(&app).overlay).contains("ABCD-EFGH"));
    apply(&mut app, &fixture.client, Effect::Cancel)
        .await
        .unwrap();
    popup_until(&mut app, &fixture.client, "cancelled").await;
    fixture.oauth.grant.add_permits(1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!fixture.root.join(auth::CODEX_AUTH_FILE).exists());
    assert_eq!(app.ask, "unsent chat draft");
}
