use super::*;
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct StateData {
    calls: Mutex<Vec<Value>>,
    replies: Mutex<std::collections::VecDeque<(StatusCode, Value)>>,
}

async fn code(State(state): State<Arc<StateData>>, Json(body): Json<Value>) -> Json<Value> {
    state.calls.lock().unwrap().push(body);
    Json(
        json!({"device_code":"device-secret", "user_code":"ABCD-EFGH", "verification_uri_complete":"https://opencode.ai/console/activate?code=ABCD-EFGH", "expires_in":600, "interval":0}),
    )
}

async fn token(
    State(state): State<Arc<StateData>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    state.calls.lock().unwrap().push(body);
    let (status, value) = state.replies.lock().unwrap().pop_front().unwrap();
    (status, Json(value))
}

fn issued(access: &str, refresh: &str, org: Option<&str>) -> (StatusCode, Value) {
    (
        StatusCode::OK,
        json!({"access_token":access,"refresh_token":refresh,"expires_in":3600,"org_id":org}),
    )
}

async fn server(
    replies: Vec<(StatusCode, Value)>,
) -> (OpenCodeAuth, Arc<StateData>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(StateData {
        replies: Mutex::new(replies.into()),
        ..Default::default()
    });
    let router = Router::new()
        .route("/auth/device/code", post(code))
        .route("/auth/device/token", post(token))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = OpenCodeAuth::at(&format!("http://{}", listener.local_addr().unwrap()));
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (client, state, task)
}

fn root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "opencode-{name}-{}-{}",
        std::process::id(),
        now_millis()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

#[tokio::test]
async fn device_login_saves_rotating_workspace_tokens_and_selects_each_plan() {
    use std::os::unix::fs::PermissionsExt;
    for provider in crate::config::OPENCODE_NATIVE {
        let (client, state, task) = server(vec![
            (StatusCode::OK, json!({"error":"authorization_pending"})),
            issued("access-one", "refresh-one", Some("workspace")),
            issued("access-two", "refresh-two", None),
        ])
        .await;
        let root = root(provider);
        let path = token_path(&root, provider);
        let mut output = Vec::new();
        client
            .login(&root.join("config.toml"), &path, provider, &mut output)
            .await
            .unwrap();
        let shown = String::from_utf8(output).unwrap();
        assert!(shown.contains("ABCD-EFGH"));
        assert!(!shown.contains("device-secret"));
        let saved = load(&path).unwrap();
        assert_eq!(saved.org_id, "workspace");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let config = Config::load(&root.join("config.toml")).unwrap();
        assert_eq!(config.opencode_native(), Some(provider));
        assert!(!fs::read_to_string(root.join("config.toml"))
            .unwrap()
            .contains("access-one"));
        let refreshed = client.access(&path, Some("access-one")).await.unwrap();
        assert_eq!(refreshed.access_token, "access-two");
        assert_eq!(refreshed.refresh_token, "refresh-two");
        assert_eq!(refreshed.org_id, "workspace");
        assert_eq!(
            client
                .access(&path, Some("access-one"))
                .await
                .unwrap()
                .access_token,
            "access-two"
        );
        let calls = state.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls[0],
            json!({"client_id":"kyotoagent","supports_org_scope":true})
        );
        assert_eq!(calls[1]["grant_type"], DEVICE_GRANT_TYPE);
        assert_eq!(calls[3]["grant_type"], "refresh_token");
        assert_eq!(calls[3]["refresh_token"], "refresh-one");
        drop(calls);
        task.abort();
        let _ = task.await;
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn denied_and_expired_logins_leave_no_credentials_or_provider() {
    for error in ["access_denied", "expired_token"] {
        let (client, _, task) =
            server(vec![(StatusCode::BAD_REQUEST, json!({"error":error}))]).await;
        let root = root(error);
        let path = token_path(&root, "opencode-go");
        let result = client
            .login(
                &root.join("config.toml"),
                &path,
                "opencode-go",
                &mut Vec::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(AuthError::Denied | AuthError::Expired)
        ));
        assert!(!path.exists());
        assert!(!root.join("config.toml").exists());
        task.abort();
        let _ = task.await;
        fs::remove_dir_all(root).unwrap();
    }
}
