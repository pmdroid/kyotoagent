use std::io::Write;
use std::path::Path;

use crate::auth::{self, AuthClient, AuthError, CodexAuth};
use crate::chat::ChatClient;
use crate::closeout::CloseoutState;
use crate::config::Config;
use crate::server::SOCKET_FILE;
use crate::tui::Client;

pub async fn run(
    home: Option<&Path>,
    cwd: Option<&Path>,
    url: Option<&str>,
    out: &mut impl Write,
) -> bool {
    let root = home
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.join(".kyotoagent"));
    let (config_ok, config) = check_config(root.as_deref(), out);
    let mut all_ok = config_ok;
    all_ok &= check_auth(
        root.as_deref(),
        config.as_ref(),
        &CodexAuth::at(auth::CODEX_ISSUER),
        out,
    )
    .await;
    all_ok &= check_models(root.as_deref(), config.as_ref(), out).await;
    all_ok &= check_web_credentials(config.as_ref(), out).await;
    all_ok &= check_serve(root.as_deref(), url, out).await;
    all_ok &= check_closeout(cwd, out);
    all_ok
}

fn check_config(root: Option<&Path>, out: &mut impl Write) -> (bool, Option<Config>) {
    let Some(root) = root else {
        return (
            write_check(out, false, "config", "no home directory for the config"),
            None,
        );
    };
    let path = root.join(crate::config::CONFIG_FILE);
    match Config::load(&path) {
        Ok(config) => {
            let detail = match &config.provider {
                Some(id) => format!(
                    "{}  {id}  {}  {}",
                    path.display(),
                    config.base_url,
                    config.model
                ),
                None => format!("{}  {}  {}", path.display(), config.base_url, config.model),
            };
            (write_check(out, true, "config", &detail), Some(config))
        }
        Err(error) => (
            write_check(out, false, "config", &one_line(&error.to_string())),
            None,
        ),
    }
}

async fn check_auth(
    root: Option<&Path>,
    config: Option<&Config>,
    codex: &CodexAuth,
    out: &mut impl Write,
) -> bool {
    let Some(config) = config else {
        return write_check(out, false, "auth", "no config");
    };
    let mut all_ok = check_provider_auth(root, config, codex, out).await;
    for id in config
        .providers
        .keys()
        .filter(|id| Some(id.as_str()) != config.provider.as_deref())
    {
        match config.for_provider(id) {
            Ok(provider) => all_ok &= check_provider_auth(root, &provider, codex, out).await,
            Err(error) => {
                all_ok &= write_check(
                    out,
                    false,
                    "auth",
                    &format!("{id} {}", one_line(&error.to_string())),
                )
            }
        }
    }
    all_ok
}

fn write_auth(out: &mut impl Write, ok: bool, config: &Config, detail: &str) -> bool {
    let detail = match &config.provider {
        Some(id) => format!("{id}  {detail}"),
        None => detail.to_string(),
    };
    write_check(out, ok, "auth", &detail)
}

async fn check_provider_auth(
    root: Option<&Path>,
    config: &Config,
    codex: &CodexAuth,
    out: &mut impl Write,
) -> bool {
    if config.is_codex() {
        let Some(root) = root else {
            return write_auth(
                out,
                false,
                config,
                "Open Providers in Kyoto Agent to sign in.",
            );
        };
        let path = root.join(auth::CODEX_AUTH_FILE);
        return match auth::codex_access(codex, &path).await {
            Ok(_) => write_auth(out, true, config, &path.display().to_string()),
            Err(error) if login_needed(&error) || matches!(error, AuthError::BadAccount) => {
                write_auth(
                    out,
                    false,
                    config,
                    "Open Providers in Kyoto Agent to sign in.",
                )
            }
            Err(error) => write_auth(out, false, config, &one_line(&error.to_string())),
        };
    }
    if config.provider.as_deref() == Some(auth::GROK_PROVIDER) && !config.is_opencode() {
        let Some(root) = root else {
            return write_auth(
                out,
                false,
                config,
                "Open Providers in Kyoto Agent to sign in.",
            );
        };
        let path = root.join(auth::AUTH_FILE);
        let client = AuthClient::new(
            config
                .grok_client_id
                .as_deref()
                .unwrap_or(auth::DEFAULT_CLIENT_ID),
        );
        return match auth::access_token(&client, &path).await {
            Ok(_) => write_auth(out, true, config, &path.display().to_string()),
            Err(error) if login_needed(&error) => write_auth(
                out,
                false,
                config,
                "Open Providers in Kyoto Agent to sign in.",
            ),
            Err(error) => write_auth(out, false, config, &one_line(&error.to_string())),
        };
    }
    if let Some(name) = config.api_key_env.as_deref() {
        if std::env::var(name).is_ok_and(|value| !value.trim().is_empty()) {
            return write_auth(out, true, config, name);
        }
    }
    if let Some(root) = root {
        let saved = auth::provider_key_path(root, config.provider.as_deref());
        let path = if config.is_opencode() && !saved.exists() {
            root.join(auth::OPENCODE_AUTH_FILE)
        } else {
            saved
        };
        if path.exists() || config.is_opencode() {
            return match auth::load_opencode_key(&path) {
                Ok(_) => write_auth(out, true, config, &path.display().to_string()),
                Err(_) => write_auth(
                    out,
                    false,
                    config,
                    &format!(
                        "{} is missing or invalid: {}",
                        if config.is_opencode() {
                            "OpenCode API key"
                        } else {
                            "API key"
                        },
                        path.display()
                    ),
                ),
            };
        }
    }
    if config.is_opencode() {
        return write_auth(out, false, config, "OpenCode API key is missing");
    }
    if let Some(name) = config.api_key_env.as_deref() {
        let detail = match std::env::var(name) {
            Ok(_) => format!("{name} is empty"),
            Err(_) => format!("{name} is unset"),
        };
        return write_auth(out, false, config, &detail);
    }
    write_auth(out, true, config, "no auth")
}

async fn check_models(root: Option<&Path>, config: Option<&Config>, out: &mut impl Write) -> bool {
    let mut all_ok = check_selected_model(root, config, out).await;
    let Some(config) = config else {
        return all_ok;
    };
    for id in config
        .providers
        .keys()
        .filter(|id| Some(id.as_str()) != config.provider.as_deref())
    {
        let provider = match config.for_provider(id) {
            Ok(provider) => provider,
            Err(error) => {
                all_ok &= write_check(
                    out,
                    false,
                    "models",
                    &format!("{id} {}", one_line(&error.to_string())),
                );
                continue;
            }
        };
        let client = match ChatClient::in_root(&provider, root) {
            Ok(client) => client,
            Err(error) => {
                all_ok &= write_check(
                    out,
                    false,
                    "models",
                    &format!("{id} {}", model_error(&error)),
                );
                continue;
            }
        };
        match client.catalog().await {
            Ok(rows) => {
                all_ok &= write_check(
                    out,
                    true,
                    "models",
                    &format!("{id}  GET {}  {} models", provider.models_url(), rows.len()),
                );
            }
            Err(error) => {
                all_ok &= write_check(
                    out,
                    false,
                    "models",
                    &format!("{id} {}", model_error(&error)),
                );
            }
        }
    }
    all_ok
}

fn model_error(error: &crate::chat::ChatError) -> String {
    match error {
        crate::chat::ChatError::Status { status, .. } => {
            format!("the model server answered {status}")
        }
        _ => one_line(&error.to_string()),
    }
}

async fn check_selected_model(
    root: Option<&Path>,
    config: Option<&Config>,
    out: &mut impl Write,
) -> bool {
    let Some(config) = config else {
        return write_check(out, false, "models", "no config");
    };
    let provider = config.provider.as_deref().unwrap_or("default");
    let client = match ChatClient::in_root(config, root) {
        Ok(client) => client,
        Err(error) => {
            return write_check(
                out,
                false,
                "models",
                &format!("{provider} {}", model_error(&error)),
            );
        }
    };
    let url = config.models_url();
    match client.catalog().await {
        Ok(rows) => match rows.iter().find(|row| row.matches(&config.model)) {
            Some(row) => {
                let mut detail = format!("{provider}  GET {url}  {}", config.model);
                if let Some(length) = row.context_length {
                    detail.push_str(&format!("  {length}"));
                } else if let Some(length) = client.model_length(&config.model).await {
                    detail.push_str(&format!("  {length}"));
                }
                write_check(out, true, "models", &detail)
            }
            None => write_check(
                out,
                false,
                "models",
                &format!(
                    "{provider}  GET {url}  {} is not in the catalog",
                    config.model
                ),
            ),
        },
        Err(error) => write_check(
            out,
            false,
            "models",
            &format!("{provider} {}", model_error(&error)),
        ),
    }
}

async fn check_web_credentials(config: Option<&Config>, out: &mut impl Write) -> bool {
    let Some(config) = config else {
        return true;
    };
    let mut all_ok = true;
    for (label, name, url, header) in [
        (
            "Exa",
            &config.exa_api_key_env,
            "https://api.exa.ai/websets/v0/teams/me",
            "x-api-key",
        ),
        (
            "Firecrawl",
            &config.firecrawl_api_key_env,
            "https://api.firecrawl.dev/v2/team/credit-usage",
            "authorization",
        ),
    ] {
        let key = std::env::var(name);
        if !config.configured_web_key_envs.contains(name)
            && !key.as_ref().is_ok_and(|key| !key.trim().is_empty())
        {
            continue;
        }
        let key = match key {
            Ok(key) if !key.trim().is_empty() => key,
            Ok(_) => {
                all_ok &= write_check(out, false, "auth", &format!("{label}  {name} is empty"));
                continue;
            }
            Err(_) => {
                all_ok &= write_check(out, false, "auth", &format!("{label}  {name} is unset"));
                continue;
            }
        };
        let value = if header == "authorization" {
            format!("Bearer {key}")
        } else {
            key
        };
        all_ok &= check_web_key(label, name, url, header, &value, out).await;
    }
    all_ok
}

async fn check_web_key(
    label: &str,
    name: &str,
    url: &str,
    header: &str,
    value: &str,
    out: &mut impl Write,
) -> bool {
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            return write_check(
                out,
                false,
                "auth",
                &format!("{label}  could not prepare credential check"),
            )
        }
    };
    let response = match client
        .get(url)
        .header(header, value)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => {
            return write_check(
                out,
                false,
                "auth",
                &format!("{label}  GET {url} could not be reached"),
            )
        }
    };
    if !response.status().is_success() {
        return write_check(
            out,
            false,
            "auth",
            &format!("{label}  GET {url} answered {}", response.status().as_u16()),
        );
    }
    let body = match response.json::<serde_json::Value>().await {
        Ok(body) => body,
        Err(_) => {
            return write_check(
                out,
                false,
                "auth",
                &format!("{label}  GET {url} returned an invalid response"),
            )
        }
    };
    let valid = if label == "Firecrawl" {
        body.get("success").and_then(serde_json::Value::as_bool) == Some(true)
    } else {
        body.get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| !id.is_empty())
    };
    write_check(
        out,
        valid,
        "auth",
        &format!(
            "{label}  {name}  GET {url}{}",
            if valid {
                ""
            } else {
                " returned an invalid response"
            }
        ),
    )
}

async fn check_serve(root: Option<&Path>, url: Option<&str>, out: &mut impl Write) -> bool {
    match url {
        Some(url) => match Client::at_url(url) {
            Ok(client) => {
                let target = url.split('?').next().unwrap_or(url);
                probe_serve(&client, target, false, out).await
            }
            Err(error) => write_check(out, false, "serve", &one_line(&error)),
        },
        None => {
            let Some(root) = root else {
                return write_check(out, false, "serve", "no home directory for the socket");
            };
            let socket = root.join(SOCKET_FILE);
            let target = format!("unix://{}", socket.display());
            let client = Client::at(socket);
            probe_serve(&client, &target, true, out).await
        }
    }
}

async fn probe_serve(client: &Client, target: &str, unix: bool, out: &mut impl Write) -> bool {
    match client.request("GET", "/v1/sessions", None).await {
        Ok((200, _)) => write_check(out, true, "serve", &format!("GET /v1/sessions  {target}")),
        Ok((status, _)) => write_check(
            out,
            false,
            "serve",
            &format!("GET /v1/sessions  answered {status}"),
        ),
        Err(error) if unix && down_socket(&error) => {
            write_check(out, false, "serve", "kyotoagent serve is not running")
        }
        Err(error) => write_check(out, false, "serve", &one_line(&error)),
    }
}

fn check_closeout(cwd: Option<&Path>, out: &mut impl Write) -> bool {
    let Some(cwd) = cwd else {
        return write_check(out, false, "closeout", "no current directory");
    };
    let path = crate::closeout::located(cwd)
        .unwrap_or_else(|| cwd.join(".kyotoagent").join("closeout.yaml"));
    match CloseoutState::new(cwd) {
        Ok(state) => match state.file {
            None => write_check(
                out,
                true,
                "closeout",
                &format!("{}  no closeout file", path.display()),
            ),
            Some(file) => {
                let n = file.items.len();
                let items = if n == 1 {
                    "1 item".to_string()
                } else {
                    format!("{n} items")
                };
                let ok = write_check(
                    out,
                    true,
                    "closeout",
                    &format!("{}  {items}", path.display()),
                );
                for item in &file.items {
                    let kind = item.kind.label();
                    let mut line = format!("                {}  {kind}  {}", item.id, item.run);
                    if !item.paths.is_empty() {
                        line.push_str("  ");
                        line.push_str(&item.paths.join("  "));
                    }
                    let _ = writeln!(out, "{line}");
                }
                ok
            }
        },
        Err(error) => write_check(out, false, "closeout", &one_line(&error.to_string())),
    }
}

fn write_check(out: &mut impl Write, ok: bool, name: &str, detail: &str) -> bool {
    let status = if ok { "ok" } else { "fail" };
    let _ = writeln!(out, "{status:<6}{name:<10}{detail}");
    ok
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn login_needed(error: &AuthError) -> bool {
    matches!(error, AuthError::NeedLogin | AuthError::Decode(_))
}

fn down_socket(error: &str) -> bool {
    error.contains("kyotoagent serve") || error.contains("cannot connect")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    async fn codex_check(
        expires: Option<&str>,
        status: u16,
        expected_ok: bool,
        expected_requests: usize,
    ) {
        let root = std::env::temp_dir().join(format!(
            "kyoto-doctor-codex-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join(auth::CODEX_AUTH_FILE);
        if let Some(expires) = expires {
            auth::write_codex_tokens(
                &path,
                &auth::CodexTokens {
                    access_token: "old-access".into(),
                    refresh_token: "old-refresh".into(),
                    id_token: "old-id-token".into(),
                    account_id: "account-1".into(),
                    expires_at: expires.into(),
                },
            )
            .unwrap();
        }
        let requests = Arc::new(AtomicUsize::new(0));
        let received = Arc::clone(&requests);
        let app = axum::Router::new().route(
            "/oauth/token",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let received = Arc::clone(&received);
                async move {
                    assert_eq!(body["grant_type"], "refresh_token");
                    assert_eq!(body["refresh_token"], "old-refresh");
                    received.fetch_add(1, Ordering::SeqCst);
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
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
        let client = CodexAuth::at(&format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = Config::from_toml("provider = \"account\"\n[providers.account]\nkind = \"codex\"\nmodel = \"gpt-6.1-sol\"\n").unwrap();
        let mut out = Vec::new();
        let ok = check_auth(Some(&root), Some(&config), &client, &mut out).await;
        let text = String::from_utf8(out).unwrap();
        server.abort();
        assert_eq!(ok, expected_ok, "{text}");
        if expected_ok {
            assert!(text.starts_with("ok"), "{text}");
            assert!(text.contains(auth::CODEX_AUTH_FILE), "{text}");
        } else {
            assert!(text.starts_with("fail"), "{text}");
            assert!(
                text.contains("Open Providers in Kyoto Agent to sign in."),
                "{text}"
            );
        }
        assert_eq!(requests.load(Ordering::SeqCst), expected_requests);
        if expected_requests == 1 && expected_ok {
            assert_eq!(
                auth::load_codex(&path).unwrap().access_token,
                "fresh-access"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn legacy_selected_credentials_are_checked_alongside_named_providers() {
        let config = Config::from_toml("base_url = \"http://localhost/v1\"\nmodel = \"m\"\napi_key_env = \"KYOTO_DOCTOR_LEGACY_MISSING_KEY\"\n[providers.local]\nbase_url = \"http://localhost/v1\"\nmodel = \"m\"\n").unwrap();
        let mut out = Vec::new();
        let ok = check_auth(
            None,
            Some(&config),
            &CodexAuth::at(auth::CODEX_ISSUER),
            &mut out,
        )
        .await;
        let text = String::from_utf8(out).unwrap();
        assert!(!ok, "{text}");
        assert!(
            text.contains("KYOTO_DOCTOR_LEGACY_MISSING_KEY is unset"),
            "{text}"
        );
        assert!(text.contains("local"), "{text}");
    }

    #[tokio::test]
    async fn an_unselected_provider_with_missing_credentials_fails_auth() {
        let config = Config::from_toml("provider = \"local\"\n[providers.local]\nbase_url = \"http://localhost/v1\"\nmodel = \"m\"\n[providers.office]\nbase_url = \"http://localhost/v1\"\nmodel = \"m\"\napi_key_env = \"KYOTO_DOCTOR_UNSELECTED_MISSING_KEY\"\n").unwrap();
        let mut out = Vec::new();
        let ok = check_auth(
            None,
            Some(&config),
            &CodexAuth::at(auth::CODEX_ISSUER),
            &mut out,
        )
        .await;
        let text = String::from_utf8(out).unwrap();
        assert!(!ok, "{text}");
        assert!(text.contains("office"), "{text}");
        assert!(
            text.contains("KYOTO_DOCTOR_UNSELECTED_MISSING_KEY is unset"),
            "{text}"
        );
        assert!(text.contains("local"), "{text}");
    }

    #[tokio::test]
    async fn an_unselected_subscription_without_credentials_fails_auth() {
        let config = Config::from_toml("provider = \"local\"\n[providers.local]\nbase_url = \"http://localhost/v1\"\nmodel = \"m\"\n[providers.account]\nkind = \"codex\"\nmodel = \"gpt-6.1-sol\"\n").unwrap();
        let mut out = Vec::new();
        let ok = check_auth(
            None,
            Some(&config),
            &CodexAuth::at(auth::CODEX_ISSUER),
            &mut out,
        )
        .await;
        let text = String::from_utf8(out).unwrap();
        assert!(!ok, "{text}");
        assert!(text.contains("account"), "{text}");
        assert!(text.contains("Open Providers"), "{text}");
    }

    #[tokio::test]
    async fn saved_provider_keys_are_checked_without_printing_them() {
        let root = std::env::temp_dir().join(format!(
            "kyoto-doctor-saved-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let config = Config::from_toml("provider = \"office\"\n[providers.office]\nbase_url = \"http://localhost/v1\"\nmodel = \"m\"\n").unwrap();
        let path = auth::provider_key_path(&root, Some("office"));
        let client = CodexAuth::at(auth::CODEX_ISSUER);
        let secret = "doctor-stored-key-canary";
        auth::write_opencode_key(&path, secret).unwrap();
        let mut out = Vec::new();
        assert!(check_auth(Some(&root), Some(&config), &client, &mut out).await);
        let text = std::str::from_utf8(&out).unwrap();
        assert!(text.contains("api-key-office.json"), "{text}");
        assert!(!text.contains(secret), "{text}");
        fs::write(&path, "{invalid").unwrap();
        out.clear();
        assert!(!check_auth(Some(&root), Some(&config), &client, &mut out).await);
        fs::remove_dir_all(root).unwrap();
    }

    async fn inactive_catalog_check(accepted: bool) {
        let root = std::env::temp_dir().join(format!(
            "kyoto-doctor-catalog-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let secret = "doctor-catalog-key-canary";
        auth::write_opencode_key(&auth::provider_key_path(&root, Some("office")), secret).unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let received = Arc::clone(&requests);
        let app = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let received = Arc::clone(&received);
                async move {
                    if let Some(value) = headers.get("authorization") {
                        assert_eq!(value.to_str().unwrap(), format!("Bearer {secret}"));
                        received.fetch_add(1, Ordering::SeqCst);
                        if !accepted {
                            return (
                                axum::http::StatusCode::UNAUTHORIZED,
                                axum::Json(serde_json::json!({"error": secret})),
                            );
                        }
                    }
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(
                            serde_json::json!({"data": [{"id": "m", "context_length": 128000}]}),
                        ),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = Config::from_toml(&format!("provider = \"local\"\n[providers.local]\nbase_url = \"{base}\"\nmodel = \"m\"\n[providers.office]\nbase_url = \"{base}\"\nmodel = \"m\"\n")).unwrap();
        let mut out = Vec::new();
        let ok = check_models(Some(&root), Some(&config), &mut out).await;
        let text = String::from_utf8(out).unwrap();
        server.abort();
        assert_eq!(ok, accepted, "{text}");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(text.contains("office"), "{text}");
        assert!(!text.contains(secret), "{text}");
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn inactive_provider_credentials_are_verified_online() {
        inactive_catalog_check(true).await;
    }

    #[tokio::test]
    async fn rejected_inactive_credentials_fail_without_echoing_secrets() {
        inactive_catalog_check(false).await;
    }

    #[tokio::test]
    async fn explicitly_configured_web_keys_are_required() {
        let config = Config::from_toml("base_url = \"http://localhost/v1\"\nmodel = \"m\"\n[web]\nexa_api_key_env = \"KYOTO_DOCTOR_MISSING_EXA\"\nfirecrawl_api_key_env = \"KYOTO_DOCTOR_MISSING_FIRECRAWL\"\n").unwrap();
        let mut out = Vec::new();
        assert!(!check_web_credentials(Some(&config), &mut out).await);
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("Exa  KYOTO_DOCTOR_MISSING_EXA is unset"),
            "{text}"
        );
        assert!(
            text.contains("Firecrawl  KYOTO_DOCTOR_MISSING_FIRECRAWL is unset"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn unconfigured_web_tools_remain_optional() {
        let config = Config {
            exa_api_key_env: "KYOTO_DOCTOR_OPTIONAL_EXA".into(),
            firecrawl_api_key_env: "KYOTO_DOCTOR_OPTIONAL_FIRECRAWL".into(),
            ..Config::default()
        };
        let mut out = Vec::new();
        assert!(check_web_credentials(Some(&config), &mut out).await);
        assert!(out.is_empty());
        std::env::set_var(&config.exa_api_key_env, "");
        std::env::set_var(&config.firecrawl_api_key_env, "  ");
        let ok = check_web_credentials(Some(&config), &mut out).await;
        std::env::remove_var(&config.exa_api_key_env);
        std::env::remove_var(&config.firecrawl_api_key_env);
        assert!(ok, "{}", String::from_utf8_lossy(&out));
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn web_key_checks_validate_headers_status_and_response_without_secrets() {
        let secret = "doctor-web-key-canary";
        for label in ["Exa", "Firecrawl"] {
            for (status, valid) in [(200, true), (401, false), (200, false)] {
                let header = if label == "Exa" {
                    "x-api-key"
                } else {
                    "authorization"
                };
                let value = if label == "Exa" {
                    secret.to_string()
                } else {
                    format!("Bearer {secret}")
                };
                let expected = value.clone();
                let app = axum::Router::new().route("/account", axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let expected = expected.clone();
                    async move {
                        assert_eq!(headers.get(header).unwrap().to_str().unwrap(), expected);
                        let body = if valid {
                            if label == "Exa" { serde_json::json!({"id": "team-1"}) } else { serde_json::json!({"success": true, "data": {"remainingCredits": 0}}) }
                        } else { serde_json::json!({"error": secret}) };
                        (axum::http::StatusCode::from_u16(status).unwrap(), axum::Json(body))
                    }
                }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}/account", listener.local_addr().unwrap());
                let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
                let mut out = Vec::new();
                let ok = check_web_key(label, "KEY_ENV", &url, header, &value, &mut out).await;
                server.abort();
                let text = String::from_utf8(out).unwrap();
                assert_eq!(ok, valid, "{text}");
                assert!(!text.contains(secret), "{text}");
            }
        }
    }

    #[tokio::test]
    async fn unreadable_codex_credentials_report_the_file_error() {
        let root = std::env::temp_dir().join(format!(
            "kyoto-doctor-io-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join(auth::CODEX_AUTH_FILE);
        fs::create_dir_all(&path).unwrap();
        let config = Config::from_toml("provider = \"account\"\n[providers.account]\nkind = \"codex\"\nmodel = \"gpt-6.1-sol\"\n").unwrap();
        let mut out = Vec::new();
        let ok = check_provider_auth(
            Some(&root),
            &config,
            &CodexAuth::at(auth::CODEX_ISSUER),
            &mut out,
        )
        .await;
        let text = String::from_utf8(out).unwrap();
        assert!(!ok, "{text}");
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(!text.contains("Open Providers"), "{text}");
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn missing_codex_credentials_fail_the_auth_check() {
        codex_check(None, 200, false, 0).await;
    }

    #[tokio::test]
    async fn fresh_codex_credentials_pass_without_refreshing() {
        codex_check(Some("2099-01-01T00:00:00.000Z"), 200, true, 0).await;
    }

    #[tokio::test]
    async fn expired_codex_credentials_refresh_during_the_auth_check() {
        codex_check(Some("2000-01-01T00:00:00.000Z"), 200, true, 1).await;
    }

    #[tokio::test]
    async fn rejected_codex_refresh_fails_the_auth_check() {
        codex_check(Some("2000-01-01T00:00:00.000Z"), 401, false, 1).await;
    }
}
