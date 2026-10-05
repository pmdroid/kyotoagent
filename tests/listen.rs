use std::fs;
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kyotoagent::config::Config;
use kyotoagent::server::{Server, SOCKET_FILE};
use kyotoagent::tui::Client;

fn mint_loopback(dir: &Path) -> (PathBuf, PathBuf) {
    fs::create_dir_all(dir).expect("the cert dir exists");
    let mut params = rcgen::CertificateParams::default();
    params.subject_alt_names = vec![rcgen::SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST))];
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, "127.0.0.1");
    params.distinguished_name = dn;
    let key = rcgen::KeyPair::generate().expect("a key pair");
    let cert = params.self_signed(&key).expect("a certificate");
    let cert_path = dir.join("server.crt");
    let key_path = dir.join("server.key");
    fs::write(&cert_path, cert.pem()).expect("the cert writes");
    fs::write(&key_path, key.serialize_pem()).expect("the key writes");
    (cert_path, key_path)
}

async fn wait_for_socket(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::net::UnixStream::connect(socket).await.is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the server did not create the socket"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_https(server: &Server) -> std::net::SocketAddr {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(addr) = server.https_addr() {
            return addr;
        }
        assert!(Instant::now() < deadline, "the server did not bind HTTPS");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn config_with_listen(root: &Path, model_url: &str) -> Config {
    let certs = root.join("certs");
    mint_loopback(&certs);
    Config::from_toml(&format!(
        "base_url = \"{model_url}\"\nmodel = \"test/model\"\nlisten = \"127.0.0.1:0\"\n"
    ))
    .expect("the config parses")
}

#[tokio::test]
async fn an_idle_tcp_peer_does_not_block_other_https_clients() {
    let root = std::env::temp_dir().join(format!("kyotoagent-tls-idle-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let config = config_with_listen(&root, "http://127.0.0.1:1/v1");
    let server = Arc::new(Server::new(&root, &config).unwrap());
    let serving = server.clone();
    let task = tokio::spawn(async move { serving.serve().await });
    let address = wait_for_https(&server).await;
    let stalled = tokio::net::TcpStream::connect(address).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let response = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap()
        .get(format!("https://{address}/v1/sessions"))
        .send()
        .await;
    drop(stalled);
    task.abort();
    let _ = task.await;
    fs::remove_dir_all(root).unwrap();
    assert_eq!(response.unwrap().status(), 401);
}

#[tokio::test]
async fn an_idle_tls_handshake_is_closed_after_its_deadline() {
    use tokio::io::AsyncReadExt;

    let root = std::env::temp_dir().join(format!("kyotoagent-tls-timeout-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let config = config_with_listen(&root, "http://127.0.0.1:1/v1");
    let server = Arc::new(Server::new(&root, &config).unwrap());
    let serving = server.clone();
    let task = tokio::spawn(async move { serving.serve().await });
    let address = wait_for_https(&server).await;
    let mut stalled = tokio::net::TcpStream::connect(address).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(7), stalled.read(&mut [0])).await;
    drop(stalled);
    task.abort();
    let _ = task.await;
    fs::remove_dir_all(root).unwrap();
    assert_eq!(result.unwrap().unwrap(), 0);
}

#[tokio::test]
async fn pairing_import_exchanges_remote_data_before_saving_the_connection() {
    let root = std::env::temp_dir().join(format!("ka-handshake-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let server_root = root.join("server/.kyotoagent");
    let client_home = root.join("client");
    fs::create_dir_all(&client_home).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}", listener.local_addr().unwrap());
    let model_server = tokio::spawn(async move {
        let router = axum::Router::new().route(
            "/models",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({"data": [{"id": "remote-advertised"}]}))
            }),
        );
        axum::serve(listener, router).await.unwrap();
    });
    let mut config = config_with_listen(&server_root, &model_url);
    let repository = server_root.join("repository");
    fs::create_dir_all(&repository).unwrap();
    config.projects.push(kyotoagent::config::Project {
        closeout: None,
        id: "remote-repo".into(),
        name: "Remote repository".into(),
        path: repository.clone(),
        yolo: None,
        enhance: None,
        show_closeout: None,
        profile: None,
    });
    let server = Arc::new(Server::new(&server_root, &config).unwrap());
    let serving = Arc::clone(&server);
    let handle = tokio::spawn(async move { serving.serve().await });
    let address = wait_for_https(&server).await;
    let issued = tokio::process::Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["pair", &address.to_string()])
        .env("HOME", server_root.parent().unwrap())
        .env("KYOTOAGENT_ROOT", &server_root)
        .output()
        .await
        .unwrap();
    assert!(issued.status.success());
    let printed = String::from_utf8(issued.stdout).unwrap();
    let uri = printed.lines().last().unwrap().to_string();
    let (_, token) = kyotoagent::pairing::connection(&uri).unwrap();
    let token = token.unwrap();
    let client = Client::at_url(&uri).unwrap();
    assert_eq!(
        client.request("GET", "/v1/sessions", None).await.unwrap().0,
        401
    );
    let identity = serde_json::json!({"name": "test client", "version": "1"}).to_string();
    for (method, route) in [
        ("GET", "/v1/pair"),
        ("POST", "/v1/share"),
        ("GET", "/v1/projects"),
    ] {
        assert_eq!(client.request(method, route, None).await.unwrap().0, 401);
    }
    assert_eq!(
        client
            .request(
                "POST",
                "/v1/pair",
                Some("{\"name\":\"\",\"version\":\"1\"}")
            )
            .await
            .unwrap()
            .0,
        400
    );
    let (status, body) = client
        .request("POST", "/v1/pair", Some(&identity))
        .await
        .unwrap();
    assert_eq!(status, 200);
    let info: kyotoagent::pairing::PairingInfo = serde_json::from_str(&body).unwrap();
    assert_eq!(info.client.name, "test client");
    assert!(!info.server.name.is_empty());
    assert_eq!(info.server.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        Path::new(&info.server.workspace),
        std::env::current_dir().unwrap()
    );
    assert_eq!(info.models[0].id, "remote-advertised");
    assert_eq!(info.repositories[0].id, "remote-repo");
    assert_eq!(Path::new(&info.repositories[0].path), repository);
    assert!(!body.contains(&token));
    let response: serde_json::Value = serde_json::from_str(&body).unwrap();
    let access_token = response["access_token"].as_str().unwrap();
    assert!(kyotoagent::pairing::PairingKey::load(&server_root)
        .unwrap()
        .accepts(access_token));
    assert_eq!(
        client
            .request("POST", "/v1/pair", Some(&identity))
            .await
            .unwrap()
            .0,
        401
    );
    let credential = format!("kyotoagent://{address}?token={access_token}");
    let key = kyotoagent::pairing::PairingKey::load(&server_root).unwrap();
    let expired = key.code().unwrap();
    fs::write(server_root.join("pairing-codes").join(&expired), "0").unwrap();
    let expired_client =
        Client::at_url(&format!("kyotoagent://{address}?token={expired}")).unwrap();
    assert_eq!(
        expired_client
            .request("POST", "/v1/pair", Some(&identity))
            .await
            .unwrap()
            .0,
        401
    );
    assert_eq!(
        Client::at_url(&credential)
            .unwrap()
            .request("GET", "/v1/sessions", None)
            .await
            .unwrap()
            .0,
        200
    );
    let run = |uri: String| {
        let home = client_home.clone();
        async move {
            tokio::process::Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
                .args(["pair", &uri])
                .env("HOME", &home)
                .env_remove("KYOTOAGENT_URL")
                .output()
                .await
                .unwrap()
        }
    };
    let output = run(format!("kyotoagent://{address}?token=invalid-token")).await;
    assert!(!output.status.success());
    let saved_path = client_home.join(".kyotoagent/config.toml");
    assert!(!saved_path.exists());
    let issued = tokio::process::Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["pair", &address.to_string()])
        .env("HOME", server_root.parent().unwrap())
        .env("KYOTOAGENT_ROOT", &server_root)
        .output()
        .await
        .unwrap();
    assert!(issued.status.success());
    let printed = String::from_utf8(issued.stdout).unwrap();
    let uri = printed.lines().last().unwrap().to_string();
    let output = run(uri.clone()).await;
    assert!(output.status.success(), "{:?}", output.stderr);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Paired with"));
    assert!(stdout.contains("Models: 1. Repositories: 1."));
    assert!(!stdout.contains(&token));
    let saved = kyotoagent::pairing::Connections::load(&saved_path).unwrap();
    let saved_uri = saved.selected().unwrap();
    assert_ne!(saved_uri, uri);
    assert_eq!(
        Client::at_url(saved_uri)
            .unwrap()
            .request("GET", "/v1/sessions", None)
            .await
            .unwrap()
            .0,
        200
    );
    assert_eq!(
        fs::metadata(&saved_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!fs::read_to_string(&saved_path)
        .unwrap()
        .contains("remote-advertised"));
    handle.abort();
    model_server.abort();
    let _ = handle.await;
    let _ = model_server.await;
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn without_listen_only_the_socket_is_bound() {
    let root = std::env::temp_dir().join(format!("kyotoagent-listen-unix-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let config =
        Config::from_toml("base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"test/model\"\n")
            .expect("the config parses");
    let server = Arc::new(Server::new(&root, &config).expect("the server is built"));
    assert!(server.https_addr().is_none());
    let serving = Arc::clone(&server);
    let handle = tokio::spawn(async move {
        let _ = serving.serve().await;
    });
    wait_for_socket(&root.join(SOCKET_FILE)).await;
    assert!(
        server.https_addr().is_none(),
        "unix-only serve does not bind TCP"
    );
    let client = Client::at(root.join(SOCKET_FILE));
    let (status, body) = client
        .request("GET", "/v1/sessions", None)
        .await
        .expect("the socket answers");
    assert_eq!(status, 200, "{body}");
    handle.abort();
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn https_and_unix_serve_the_same_sessions_over_http2() {
    let root = std::env::temp_dir().join(format!("kyotoagent-listen-https-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("the root exists");
    let config = config_with_listen(&root, "http://127.0.0.1:1/v1");
    let server = Arc::new(Server::new(&root, &config).expect("the server is built"));
    let serving = Arc::clone(&server);
    let handle = tokio::spawn(async move {
        let _ = serving.serve().await;
    });
    wait_for_socket(&root.join(SOCKET_FILE)).await;
    let addr = wait_for_https(&server).await;

    let workspace = root.join("work");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    let workspace = fs::canonicalize(&workspace).unwrap_or(workspace);
    let socket = Client::at(root.join(SOCKET_FILE));
    let body = serde_json::json!({ "workspace": workspace.display().to_string() }).to_string();
    let (status, created) = socket
        .request("POST", "/v1/sessions", Some(&body))
        .await
        .expect("create over the socket");
    assert_eq!(status, 201, "{created}");
    let created: serde_json::Value = serde_json::from_str(&created).expect("created JSON");
    let id = created["id"].as_str().expect("an id");

    let (status, listed) = socket
        .request("GET", "/v1/sessions", None)
        .await
        .expect("list over the socket");
    assert_eq!(status, 200, "{listed}");

    let url = format!("https://{addr}");
    let token = kyotoagent::pairing::PairingKey::load(&root)
        .expect("key")
        .token()
        .expect("token");
    let uri = format!("kyotoagent://{addr}?token={token}");
    let https = Client::at_url(&uri).expect("the URL client");
    let (status, over_url) = https
        .request("GET", "/v1/sessions", None)
        .await
        .expect("list over HTTPS");
    assert_eq!(status, 200, "{over_url}");
    assert_eq!(listed, over_url);
    let mut doctor = Vec::new();
    kyotoagent::doctor::run(None, None, Some(&uri), &mut doctor).await;
    let doctor = String::from_utf8(doctor).expect("doctor text");
    assert!(
        doctor
            .lines()
            .any(|line| line.starts_with("ok") && line.contains("serve")),
        "{doctor}"
    );
    assert!(!doctor.contains(&token), "doctor hides pairing credentials");
    assert!(!doctor.contains("?token="), "doctor hides the token query");
    assert!(over_url.contains(id), "{over_url}");

    let sessions_url = format!("{url}/v1/sessions");
    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("an HTTP/2 client");
    let denied = http
        .get(&sessions_url)
        .send()
        .await
        .expect("unauthenticated request");
    assert_eq!(denied.status().as_u16(), 401);
    let legacy = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &serde_json::json!({"iss": "kyotoagent", "aud": "kyotoagent-client"}),
        &jsonwebtoken::EncodingKey::from_secret(&fs::read(root.join("pairing.key")).unwrap()),
    )
    .unwrap();
    let legacy = Client::at_url(&format!("kyotoagent://{addr}?token={legacy}")).unwrap();
    let identity = serde_json::json!({"name":"old client", "version":"1"}).to_string();
    for (method, path) in [
        ("GET", "/v1/sessions"),
        ("GET", "/v1/server"),
        ("POST", "/v1/pair"),
    ] {
        assert_eq!(
            legacy
                .request(method, path, Some(&identity))
                .await
                .unwrap()
                .0,
            401,
            "{method} {path}"
        );
    }
    let denied = http
        .get(&sessions_url)
        .bearer_auth("invalid")
        .send()
        .await
        .expect("invalid token");
    assert_eq!(denied.status().as_u16(), 401);
    let expired = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &serde_json::json!({"exp": 0, "iss": "kyotoagent", "aud": "kyotoagent-paired-client"}),
        &jsonwebtoken::EncodingKey::from_secret(
            &fs::read(root.join("pairing.key")).expect("private key"),
        ),
    )
    .expect("expired token");
    let denied = http
        .get(&sessions_url)
        .bearer_auth(expired)
        .send()
        .await
        .expect("expired token request");
    assert_eq!(denied.status().as_u16(), 401);
    let request = http
        .get(&sessions_url)
        .bearer_auth(&token)
        .build()
        .expect("request");
    let response = http.execute(request).await.expect("HTTP/2 GET");
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.version(), reqwest::Version::HTTP_2);

    let http1 = reqwest::Client::builder()
        .http1_only()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("an HTTP/1.1 client");
    let response = http1
        .get(&sessions_url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("HTTP/1.1 GET");
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.version(), reqwest::Version::HTTP_11);

    handle.abort();
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_binary_lists_the_same_sessions_over_url() {
    let home = std::env::temp_dir().join(format!("kyotoagent-listen-cli-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let root = home.join(".kyotoagent");
    fs::create_dir_all(&root).expect("the root exists");
    let config = config_with_listen(&root, "http://127.0.0.1:1/v1");
    let server = Arc::new(Server::new(&root, &config).expect("the server is built"));
    let serving = Arc::clone(&server);
    let handle = tokio::spawn(async move {
        let _ = serving.serve().await;
    });
    wait_for_socket(&root.join(SOCKET_FILE)).await;
    let addr = wait_for_https(&server).await;

    let workspace = home.join("work");
    fs::create_dir_all(&workspace).expect("the workspace exists");
    let workspace = fs::canonicalize(&workspace).unwrap_or(workspace);
    let home_for_bin = home.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
            .args(["new"])
            .env("HOME", &home_for_bin)
            .current_dir(&workspace)
            .output()
            .expect("kyotoagent new runs")
    })
    .await
    .expect("the thread joins");
    assert!(
        output.status.success(),
        "kyotoagent new: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let token = kyotoagent::pairing::PairingKey::load(&root)
        .expect("key")
        .code()
        .expect("token");
    let url = format!("kyotoagent://{addr}?token={token}");
    let home_for_bin = home.clone();
    let url_for_bin = url.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
            .args(["--url", &url_for_bin, "sessions"])
            .env("HOME", &home_for_bin)
            .output()
            .expect("kyotoagent --url sessions runs")
    })
    .await
    .expect("the thread joins");
    assert!(
        output.status.success(),
        "kyotoagent --url sessions: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let over_url = String::from_utf8(output.stdout).expect("stdout is text");
    let saved = kyotoagent::pairing::Connections::load(&root.join("config.toml")).unwrap();
    assert_ne!(saved.selected().unwrap(), url);
    let (_, credential) = kyotoagent::pairing::connection(saved.selected().unwrap()).unwrap();
    assert!(kyotoagent::pairing::PairingKey::load(&root)
        .unwrap()
        .accepts(&credential.unwrap()));

    let home_for_bin = home.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
            .args(["sessions"])
            .env("HOME", &home_for_bin)
            .output()
            .expect("kyotoagent sessions runs")
    })
    .await
    .expect("the thread joins");
    assert!(
        output.status.success(),
        "kyotoagent sessions: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let over_socket = String::from_utf8(output.stdout).expect("stdout is text");
    assert_eq!(over_url, over_socket);
    assert!(!over_url.trim().is_empty(), "{over_url}");

    handle.abort();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn serve_listen_writes_config_and_prints_the_bound_address() {
    let home = std::env::temp_dir().join(format!("kyotoagent-listen-bin-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let root = home.join(".kyotoagent");
    fs::create_dir_all(root.join("certs")).expect("the certs dir exists");
    mint_loopback(&root.join("certs"));
    fs::write(
        root.join("config.toml"),
        "base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"test/model\"\n",
    )
    .expect("the config writes");

    let mut child = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["serve", "--listen", "127.0.0.1:0"])
        .env("HOME", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("kyotoagent serve starts");
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut https = None;
    let mut serving = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let line = line.expect("a line");
        if let Some(rest) = line.strip_prefix("Kyoto Agent serving on ") {
            serving = Some(rest.to_string());
        }
        if let Some(rest) = line.strip_prefix("Kyoto Agent listening on ") {
            https = Some(rest.to_string());
            break;
        }
        if Instant::now() > deadline {
            break;
        }
    }
    let https = https.expect("serve printed the HTTPS address");
    assert!(https.starts_with("https://127.0.0.1:"), "{https}");
    let serving = serving.expect("serve printed the socket");
    assert!(
        serving.ends_with("/.kyotoagent/kyotoagent.sock"),
        "{serving}"
    );
    assert!(!serving.contains("/.pagent/"), "{serving}");

    let text = fs::read_to_string(root.join("config.toml")).expect("the config reads");
    assert!(text.contains("listen = \"127.0.0.1:0\""), "{text}");

    let home_for_bin = home.clone();
    let token = kyotoagent::pairing::PairingKey::load(&root)
        .expect("key")
        .token()
        .expect("token");
    let url = format!(
        "kyotoagent://{}?token={token}",
        https.trim_start_matches("https://")
    );
    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
            .args(["--url", &url, "sessions"])
            .env("HOME", &home_for_bin)
            .output()
            .expect("kyotoagent --url sessions runs")
    })
    .await
    .expect("the thread joins");
    assert!(
        output.status.success(),
        "kyotoagent --url sessions: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let home_for_env = home.clone();
    let url = format!(
        "kyotoagent://{}?token={token}",
        https.trim_start_matches("https://")
    );
    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_kyoto"))
            .args(["sessions"])
            .env("HOME", &home_for_env)
            .env("KYOTOAGENT_URL", &url)
            .output()
            .expect("kyoto sessions runs")
    })
    .await
    .expect("the thread joins");
    assert!(
        output.status.success(),
        "KYOTOAGENT_URL selects the server: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&home);
}

#[tokio::test]
async fn https_can_be_enabled_and_shared_without_restarting_the_server() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("ka-enable-https-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model.local_addr().unwrap();
    let requested = Arc::new(tokio::sync::Notify::new());
    let notify = Arc::clone(&requested);
    let model_task = tokio::spawn(async move {
        let router = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move || {
                let notify = Arc::clone(&notify);
                async move {
                    notify.notify_one();
                    std::future::pending::<axum::Json<serde_json::Value>>().await
                }
            }),
        );
        axum::serve(model, router).await.unwrap();
    });
    fs::write(
        &path,
        format!("base_url = \"http://{model_addr}\"\nmodel = \"test\"\n"),
    )
    .unwrap();
    let server = Arc::new(Server::new(&root, &Config::load(&path).unwrap()).unwrap());
    let serving = Arc::clone(&server);
    let handle = tokio::spawn(async move { serving.serve().await });
    wait_for_socket(&root.join(SOCKET_FILE)).await;
    let local = Client::at(root.join(SOCKET_FILE));
    let body = serde_json::json!({"workspace": root}).to_string();
    let (status, body) = local
        .request("POST", "/v1/sessions", Some(&body))
        .await
        .unwrap();
    assert_eq!(status, 201);
    let session: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = session["id"].as_str().unwrap();
    let messages = format!("/v1/sessions/{id}/messages");
    assert_eq!(
        local
            .request(
                "POST",
                &messages,
                Some("{\"text\":\"keep running\",\"enhance\":false}")
            )
            .await
            .unwrap()
            .0,
        202
    );
    tokio::time::timeout(Duration::from_secs(5), requested.notified())
        .await
        .unwrap();
    assert_eq!(
        local
            .request(
                "POST",
                &messages,
                Some("{\"text\":\"keep queued\",\"enhance\":false}")
            )
            .await
            .unwrap()
            .0,
        202
    );
    let busy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let before = fs::read_to_string(&path).unwrap();
    let body = serde_json::json!({"listen": busy.local_addr().unwrap().to_string()}).to_string();
    assert_eq!(
        local
            .request("POST", "/v1/https", Some(&body))
            .await
            .unwrap()
            .0,
        400
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    assert!(server.https_addr().is_none());
    let body = "{\"listen\":\"127.0.0.1:0\"}";
    let (status, response) = local
        .request("POST", "/v1/https", Some(body))
        .await
        .unwrap();
    assert_eq!(status, 200, "{response}");
    let address = server.https_addr().unwrap();
    assert_eq!(
        Config::load(&path).unwrap().listen,
        Some(address.to_string())
    );
    for file in ["certs/server.crt", "certs/server.key"] {
        assert_eq!(
            fs::metadata(root.join(file)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let body = serde_json::json!({"host": address.to_string()}).to_string();
    let (status, response) = local
        .request("POST", "/v1/share", Some(&body))
        .await
        .unwrap();
    assert_eq!(status, 200, "{response}");
    let value: serde_json::Value = serde_json::from_str(&response).unwrap();
    let uri = value["uri"].as_str().unwrap();
    let temporary = Client::at_url(uri).unwrap();
    assert_eq!(
        temporary
            .request("GET", "/v1/sessions", None)
            .await
            .unwrap()
            .0,
        401
    );
    let (credential, _) = kyotoagent::pairing::verify(uri, "test client")
        .await
        .unwrap();
    let remote = Client::at_url(&credential).unwrap();
    assert_eq!(
        remote
            .request("GET", &format!("/v1/sessions/{id}/view"), None)
            .await
            .unwrap()
            .0,
        200
    );
    assert_eq!(
        local
            .request("GET", &format!("/v1/sessions/{id}/view"), None)
            .await
            .unwrap()
            .0,
        200
    );
    let (_, body) = remote
        .request("GET", &format!("/v1/sessions/{id}/view"), None)
        .await
        .unwrap();
    let view: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(view["status"], "working");
    assert_eq!(view["queue"], serde_json::json!(["keep queued"]));
    let unauthenticated = Client::at_url(&format!("https://{address}")).unwrap();
    assert_eq!(
        unauthenticated
            .request("GET", "/v1/sessions", None)
            .await
            .unwrap()
            .0,
        401
    );
    assert_eq!(
        local
            .request("POST", "/v1/https", Some("{\"listen\":\"127.0.0.1:0\"}"))
            .await
            .unwrap()
            .0,
        409
    );
    server.runner().cancel(id);
    model_task.abort();
    let _ = model_task.await;
    handle.abort();
    let _ = handle.await;
    assert!(server.https_addr().is_none());
    tokio::net::TcpListener::bind(address).await.unwrap();
    fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn https_setup_creates_configuration_for_a_server_started_with_defaults() {
    let root = std::env::temp_dir().join(format!("ka-default-https-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let server = Arc::new(Server::new(&root, &Config::default()).unwrap());
    let serving = Arc::clone(&server);
    let task = tokio::spawn(async move { serving.serve().await });
    wait_for_socket(&root.join(SOCKET_FILE)).await;
    let client = Client::at(root.join(SOCKET_FILE));
    let (status, response) = client
        .request("POST", "/v1/https", Some("{\"listen\":\"127.0.0.1:0\"}"))
        .await
        .unwrap();
    assert_eq!(status, 200, "{response}");
    let addr = server.https_addr().unwrap();
    assert_eq!(
        Config::load(&root.join("config.toml")).unwrap().listen,
        Some(addr.to_string())
    );
    task.abort();
    let _ = task.await;
    fs::remove_dir_all(root).unwrap();
}
