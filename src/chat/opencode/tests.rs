use super::*;
use axum::{
    extract::{OriginalUri, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

fn metadata() -> Value {
    json!({"opencode-go":{"npm":"@ai-sdk/openai-compatible","models":{
        "chat":{"limit":{"context":32000,"output":8192}},
        "responses":{"provider":{"npm":"@ai-sdk/openai"}},
        "claude":{"provider":{"npm":"@ai-sdk/anthropic"}},
        "gemini":{"provider":{"npm":"@ai-sdk/google"}},
        "disabled":{},"jev-1.13":{}
    }}, "opencode":{"models":{"old-grok":{"provider":{"npm":"@ai-sdk/openai"}}}}})
}

fn ids() -> Value {
    json!({"data":[{"id":"chat"},{"id":"responses"},{"id":"claude"},{"id":"gemini"},{"id":"unknown"},{"id":"disabled"},{"id":"jev-1.13"},{"id":"old-grok"}]})
}

fn remote() -> Value {
    json!({"providers":{"opencode-go":{"headers":{"x-route":"go"},"models":{"disabled":{"disabled":true}}}}})
}

#[test]
fn discovery_joins_live_ids_with_routes_and_workspace_overrides() {
    let rows = discover(
        "opencode-go",
        "https://opencode.ai/inference/go",
        &ids(),
        &metadata(),
        &remote(),
    )
    .unwrap();
    assert_eq!(rows.len(), 5);
    for (name, protocol) in [
        ("chat", Protocol::Chat),
        ("responses", Protocol::Responses),
        ("claude", Protocol::Anthropic),
        ("gemini", Protocol::Gemini),
        ("old-grok", Protocol::Responses),
    ] {
        let row = rows.iter().find(|row| row.id == name).unwrap();
        assert_eq!(row.protocol, protocol);
        assert_eq!(row.headers["x-route"], "go");
        assert!(row.base.starts_with("https://opencode.ai/inference/go/"));
    }
    assert_eq!(
        rows.iter()
            .find(|row| row.id == "chat")
            .unwrap()
            .row
            .context_length,
        Some(32000)
    );
    let mut remote = remote();
    remote["providers"]["opencode-go"]["models"]["private"] = json!({"modelID":"private-v2","package":"aisdk:@ai-sdk/anthropic","limit":{"context":64000}});
    let rows = discover(
        "opencode-go",
        "https://opencode.ai/inference/go",
        &ids(),
        &metadata(),
        &remote,
    )
    .unwrap();
    let private = rows.iter().find(|row| row.row.id == "private").unwrap();
    assert_eq!(private.id, "private-v2");
    assert_eq!(private.protocol, Protocol::Anthropic);
    remote["providers"]["opencode-go"]["settings"] =
        json!({"baseURL":"https://elsewhere.invalid/v1"});
    assert!(discover(
        "opencode-go",
        "https://opencode.ai/inference/go",
        &ids(),
        &metadata(),
        &remote
    )
    .is_err());
}

#[derive(Default)]
struct Captures(Mutex<Vec<(String, HeaderMap, Value)>>);

async fn models() -> Json<Value> {
    Json(ids())
}
async fn config() -> Json<Value> {
    Json(remote())
}
async fn meta() -> Json<Value> {
    Json(metadata())
}
async fn inference(
    State(state): State<Arc<Captures>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let path = uri.path().to_string();
    state.0.lock().unwrap().push((path.clone(), headers, body));
    Json(if path.ends_with("/responses") {
        json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"responses works"}]}]})
    } else if path.ends_with("/messages") {
        json!({"content":[{"type":"text","text":"anthropic works"}],"stop_reason":"end_turn"})
    } else if path.contains("generateContent") {
        json!({"candidates":[{"content":{"parts":[{"text":"gemini works"}]},"finishReason":"STOP"}]})
    } else {
        json!({"choices":[{"message":{"role":"assistant","content":"chat works"},"finish_reason":"stop"}]})
    })
}

#[tokio::test]
async fn native_client_discovers_and_calls_every_protocol_with_workspace_and_session_headers() {
    let captures = Arc::new(Captures::default());
    let router = Router::new()
        .route("/api/v2/config", get(config))
        .route("/v1/models", get(models))
        .route("/metadata", get(meta))
        .route("/openai/v1/chat/completions", post(inference))
        .route("/openai/v1/responses", post(inference))
        .route("/anthropic/v1/messages", post(inference))
        .route("/google/v1beta/models/{model}", post(inference))
        .with_state(captures.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let root = std::env::temp_dir().join(format!(
        "opencode-chat-{}-{}",
        std::process::id(),
        crate::events::now()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let config = Config::from_toml(&format!("provider='opencode-go'\n[providers.opencode-go]\nkind='opencode-go'\nbase_url='{base}'\nmodel='chat'\n")).unwrap();
    auth::opencode::write(
        &auth::opencode::token_path(&root, "opencode-go"),
        &auth::opencode::OpenCodeTokens {
            access_token: "access-secret".into(),
            refresh_token: "refresh-secret".into(),
            expires_at: "2099-01-01T00:00:00.000Z".into(),
            org_id: "workspace".into(),
        },
    )
    .unwrap();
    let mut client = Client::new(&config, Some(&root)).unwrap().unwrap();
    client.auth = auth::OpenCodeAuth::at(&base);
    client.metadata = format!("{base}/metadata");
    client.set_session("session-one");
    assert_eq!(client.catalog().await.unwrap().len(), 5);
    for id in ["chat", "responses", "claude", "gemini"] {
        let reply = client
            .complete(
                id,
                &[Message::User {
                    content: "hello".into(),
                }],
                &[],
                None,
                None,
                10,
            )
            .await
            .unwrap();
        assert!(reply.text().ends_with("works"));
    }
    let requests = captures.0.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);
    for (_, headers, _) in requests.iter() {
        assert_eq!(headers["authorization"], "Bearer access-secret");
        assert_eq!(headers["x-org-id"], "workspace");
        assert_eq!(headers["x-opencode-session"], "session-one");
        assert_eq!(headers["user-agent"], auth::opencode::USER_AGENT);
        assert_eq!(headers["x-route"], "go");
    }
    assert_eq!(requests[2].1["anthropic-version"], "2023-06-01");
    drop(requests);
    task.abort();
    let _ = task.await;
    std::fs::remove_dir_all(root).unwrap();
}
