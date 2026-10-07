use super::*;
use crate::config::PushConfig;
use crate::events::Event;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::{
    rand::SystemRandom,
    signature::{EcdsaKeyPair, ECDSA_P256_SHA256_FIXED_SIGNING},
};
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Device {
    #[serde(rename = "id", alias = "installationId")]
    installation_id: String,
    server_id: String,
    token: String,
    environment: Environment,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Environment {
    Sandbox,
    Production,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Removal {
    #[serde(rename = "id", alias = "installationId")]
    installation_id: String,
}

struct Provider {
    config: PushConfig,
    key: EcdsaKeyPair,
    client: reqwest::Client,
    jwt_cache: Mutex<Option<(u64, String)>>,
    #[cfg(test)]
    endpoint: Option<String>,
}

pub(super) struct Push {
    root: PathBuf,
    devices: Mutex<Vec<Device>>,
    provider: Option<Provider>,
    tui_seen: Mutex<Option<std::time::Instant>>,
}

pub(super) struct Worker(Vec<tokio::task::JoinHandle<()>>);
impl Drop for Worker {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct Notice {
    session_id: String,
    event_id: String,
    kind: EventKind,
}

impl Push {
    pub fn new(root: &Path, config: Option<&PushConfig>) -> Result<Self, String> {
        let path = root.join("devices.json");
        let devices = match fs::read(&path) {
            Ok(bytes) => {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                    .map_err(|_| "cannot secure device registry")?;
                serde_json::from_slice(&bytes).map_err(|_| "invalid device registry")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err("cannot read device registry".into()),
        };
        let provider = config
            .map(|config| {
                if [&config.team_id, &config.key_id, &config.topic]
                    .iter()
                    .any(|v| {
                        v.is_empty()
                            || !v
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
                    })
                {
                    return Err("invalid push identifiers".to_string());
                }
                let pem = fs::read_to_string(root.join(&config.private_key))
                    .map_err(|_| "cannot read APNs signing key")?;
                let encoded: String = pem
                    .lines()
                    .filter(|line| !line.starts_with("-----"))
                    .collect();
                let der = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| "invalid APNs signing key")?;
                let key = EcdsaKeyPair::from_pkcs8(
                    &ECDSA_P256_SHA256_FIXED_SIGNING,
                    &der,
                    &SystemRandom::new(),
                )
                .map_err(|_| "invalid APNs P-256 PKCS8 signing key")?;
                let client = reqwest::Client::builder()
                    .http2_prior_knowledge()
                    .timeout(Duration::from_secs(10))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|_| "cannot build APNs transport")?;
                Ok(Provider {
                    config: config.clone(),
                    key,
                    client,
                    jwt_cache: Mutex::new(None),
                    #[cfg(test)]
                    endpoint: None,
                })
            })
            .transpose()?;
        Ok(Self {
            root: root.to_owned(),
            devices: Mutex::new(devices),
            provider,
            tui_seen: Mutex::new(None),
        })
    }

    fn update(&self, change: impl FnOnce(&mut Vec<Device>)) -> Result<(), ApiError> {
        let mut devices = self
            .devices
            .lock()
            .map_err(|_| ApiError::server("device registry unavailable"))?;
        let mut next = devices.clone();
        change(&mut next);
        let path = self.root.join("devices.json");
        let temp = self.root.join("devices.json.tmp");
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temp)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            serde_json::to_writer(&mut file, &next)?;
            file.sync_all()?;
            fs::rename(&temp, &path)?;
            fs::File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        result.map_err(|_| ApiError::server("cannot save device registry"))?;
        *devices = next;
        Ok(())
    }

    pub fn start(self: &Arc<Self>) -> Worker {
        if self.provider.is_none() {
            return Worker(Vec::new());
        }
        let mut seen = HashMap::new();
        self.scan(&mut seen, true);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Notice>(64);
        let observer = Arc::clone(self);
        let poll = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let scan = Arc::clone(&observer);
                let result = tokio::task::spawn_blocking(move || {
                    let notices = scan.scan(&mut seen, false);
                    (seen, notices)
                })
                .await;
                let Ok((state, notices)) = result else {
                    break;
                };
                seen = state;
                for notice in notices {
                    let _ = tx.try_send(notice);
                }
            }
        });
        let delivery = Arc::clone(self);
        let worker = tokio::spawn(async move {
            while let Some(notice) = rx.recv().await {
                let devices = delivery
                    .devices
                    .lock()
                    .map(|v| v.clone())
                    .unwrap_or_default();
                for device in devices {
                    delivery.deliver(&device, &notice).await;
                }
            }
        });
        Worker(vec![poll, worker])
    }

    fn tui_active(&self) -> bool {
        self.tui_seen
            .lock()
            .is_ok_and(|seen| seen.is_some_and(|at| at.elapsed() < Duration::from_secs(15)))
    }

    fn scan(&self, seen: &mut HashMap<String, HashSet<String>>, baseline: bool) -> Vec<Notice> {
        let mut notices = Vec::new();
        let suppressed = self.tui_active();
        for dir in session_dirs(&self.root) {
            let session = Session::at(&dir);
            let (Ok(meta), Ok(events)) = (session.meta(), session.events()) else {
                continue;
            };
            let ids = seen.entry(meta.id.clone()).or_default();
            for event in &events {
                if baseline
                    || suppressed
                    || meta.parent_id.is_some()
                    || !matches!(
                        event.kind,
                        EventKind::Permission | EventKind::Question | EventKind::Result
                    )
                    || (event.kind == EventKind::Permission && meta.yolo)
                {
                    ids.insert(event.id.clone());
                }
                if !ids.contains(&event.id) && !baseline && eligible(event, &events, &meta) {
                    ids.insert(event.id.clone());
                    notices.push(Notice {
                        session_id: meta.id.clone(),
                        event_id: event.id.clone(),
                        kind: event.kind,
                    });
                }
            }
        }
        notices
    }

    fn current(&self, device: &Device, notice: &Notice) -> bool {
        if self.tui_active() || !self.devices.lock().is_ok_and(|v| v.contains(device)) {
            return false;
        }
        let session = Session::at(&session_dir(&self.root, &notice.session_id));
        let (Ok(meta), Ok(events)) = (session.meta(), session.events()) else {
            return false;
        };
        events
            .iter()
            .find(|event| event.id == notice.event_id)
            .is_some_and(|event| eligible(event, &events, &meta))
    }

    async fn deliver(&self, device: &Device, notice: &Notice) {
        let Some(provider) = &self.provider else {
            return;
        };
        for attempt in 0..3 {
            if !self.current(device, notice) {
                return;
            }
            let Ok(jwt) = provider.jwt() else {
                return;
            };
            let payload = serde_json::json!({
                "aps": { "alert": { "title": "Kyoto Agent", "body": if notice.kind == EventKind::Result { "A session has finished." } else { "A session needs your attention." } }, "sound": "default" },
                "serverId": device.server_id, "sessionId": notice.session_id,
                "eventId": notice.event_id, "kind": notice.kind.label()
            });
            let response = provider
                .client
                .post(format!(
                    "{}/3/device/{}",
                    provider.endpoint(device.environment),
                    device.token
                ))
                .bearer_auth(jwt)
                .header("apns-topic", &provider.config.topic)
                .header("apns-push-type", "alert")
                .header("apns-priority", "10")
                .header("apns-expiration", "0")
                .json(&payload)
                .send()
                .await;
            match response {
                Ok(response) if response.status().as_u16() == 410 => {
                    let _ =
                        self.update(|devices| devices.retain(|registered| registered != device));
                    return;
                }
                Ok(response) if response.status().is_success() => return,
                Ok(response)
                    if response.status().as_u16() == 429 || response.status().is_server_error() => {
                }
                Ok(_) => return,
                Err(_) => {}
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
            }
        }
    }
}

fn eligible(event: &Event, events: &[Event], meta: &SessionMeta) -> bool {
    if meta.parent_id.is_some() {
        return false;
    }
    match event.kind {
        EventKind::Result => true,
        EventKind::Permission | EventKind::Question => {
            if meta.status != Status::Waiting || (event.kind == EventKind::Permission && meta.yolo)
            {
                return false;
            }
            let Some(index) = events.iter().position(|e| e.id == event.id) else {
                return false;
            };
            !events[index + 1..].iter().any(|e| {
                e.turn_id != event.turn_id
                    || matches!(
                        e.kind,
                        EventKind::Permission
                            | EventKind::Question
                            | EventKind::PermissionAnswer
                            | EventKind::QuestionAnswer
                            | EventKind::Result
                    )
            })
        }
        _ => false,
    }
}

impl Provider {
    fn jwt(&self) -> Result<String, ()> {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ())?
            .as_secs();
        let mut cache = self.jwt_cache.lock().map_err(|_| ())?;
        if let Some((issued, token)) = cache.as_ref() {
            if at >= *issued && at - issued < 3000 {
                return Ok(token.clone());
            }
        }
        let header = URL_SAFE_NO_PAD
            .encode(serde_json::json!({"alg":"ES256","kid":self.config.key_id}).to_string());
        let claims = URL_SAFE_NO_PAD
            .encode(serde_json::json!({"iss":self.config.team_id,"iat":at}).to_string());
        let input = format!("{header}.{claims}");
        let signature = self
            .key
            .sign(&SystemRandom::new(), input.as_bytes())
            .map_err(|_| ())?;
        let token = format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()));
        *cache = Some((at, token.clone()));
        Ok(token)
    }
    fn endpoint(&self, environment: Environment) -> &str {
        #[cfg(test)]
        if let Some(endpoint) = &self.endpoint {
            return endpoint;
        }
        match environment {
            Environment::Sandbox => "https://api.sandbox.push.apple.com",
            Environment::Production => "https://api.push.apple.com",
        }
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

pub(super) async fn register(
    State(state): State<AppState>,
    Json(mut device): Json<Device>,
) -> Result<StatusCode, ApiError> {
    if !valid_id(&device.installation_id)
        || !valid_id(&device.server_id)
        || device.token.is_empty()
        || device.token.len() > 1024
        || device.token.len() % 2 != 0
        || !device.token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(ApiError::bad_request("invalid device registration"));
    }
    device.token.make_ascii_lowercase();
    state.push.update(|devices| {
        devices.retain(|d| {
            !d.installation_id
                .eq_ignore_ascii_case(&device.installation_id)
        });
        devices.push(device);
    })?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn tui_heartbeat(State(state): State<AppState>) -> StatusCode {
    if let Ok(mut seen) = state.push.tui_seen.lock() {
        *seen = Some(std::time::Instant::now());
    }
    StatusCode::NO_CONTENT
}

pub(super) async fn unregister(
    State(state): State<AppState>,
    Json(removal): Json<Removal>,
) -> Result<StatusCode, ApiError> {
    if !valid_id(&removal.installation_id) {
        return Err(ApiError::bad_request("invalid device identity"));
    }
    state.push.update(|devices| {
        devices.retain(|d| {
            !d.installation_id
                .eq_ignore_ascii_case(&removal.installation_id)
        })
    })?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{KeyPair, UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "kyoto-push-{}-{}",
            std::process::id(),
            generate_id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn provider() -> Provider {
        let random = SystemRandom::new();
        let der = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &random).unwrap();
        Provider {
            config: PushConfig {
                team_id: "TEAM".into(),
                key_id: "KEY".into(),
                private_key: "unused".into(),
                topic: "sh.pascal.kyotoagent".into(),
            },
            key: EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, der.as_ref(), &random)
                .unwrap(),
            client: reqwest::Client::builder()
                .http2_prior_knowledge()
                .build()
                .unwrap(),
            endpoint: None,
            jwt_cache: Mutex::new(None),
        }
    }

    fn device() -> Device {
        Device {
            installation_id: "12345678-1234-1234-1234-123456789abc".into(),
            server_id: "abcdef12-1234-1234-1234-123456789abc".into(),
            token: "ab".repeat(32),
            environment: Environment::Sandbox,
        }
    }

    #[tokio::test]
    async fn device_api_accepts_variable_tokens_and_removes_installation() {
        let root = root();
        let server = Server::new(&root, &Config::default()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/devices", listener.local_addr().unwrap());
        let task = tokio::spawn(axum::serve(listener, server.router()).into_future());
        let client = reqwest::Client::new();
        let mut registration = serde_json::json!({
            "id": device().installation_id,
            "serverId": device().server_id,
            "token": "ABCDEF01",
            "environment": "sandbox"
        });
        assert_eq!(
            client
                .put(&url)
                .json(&registration)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(server.push.devices.lock().unwrap()[0].token, "abcdef01");
        registration["token"] = serde_json::json!("ab".repeat(48));
        registration["serverId"] = serde_json::json!("ABCDEF12-5678-1234-1234-123456789ABC");
        registration["environment"] = serde_json::json!("production");
        assert_eq!(
            client
                .put(&url)
                .json(&registration)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(server.push.devices.lock().unwrap().len(), 1);
        for token in ["", "abc", "zz", &"ab".repeat(513)] {
            registration["token"] = serde_json::json!(token);
            assert_eq!(
                client
                    .put(&url)
                    .json(&registration)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        registration["token"] = serde_json::json!("abcd");
        registration["id"] = serde_json::json!("not-a-uuid");
        assert_eq!(
            client
                .put(&url)
                .json(&registration)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        let removal = serde_json::json!({"id": device().installation_id.to_uppercase()});
        for _ in 0..2 {
            assert_eq!(
                client
                    .delete(&url)
                    .json(&removal)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NO_CONTENT
            );
        }
        assert!(server.push.devices.lock().unwrap().is_empty());
        task.abort();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_is_optional_and_topic_defaults() {
        assert!(Config::default().push.is_none());
        let config =
            Config::from_toml("base_url = 'http://localhost'\nmodel = 'model'\n[push]\nteam_id = 'TEAM'\nkey_id = 'KEY'\nprivate_key = 'key.p8'\n")
                .unwrap();
        assert_eq!(config.push.unwrap().topic, "sh.pascal.kyotoagent");
        assert!(Config::from_toml(
            "base_url = 'http://localhost'\nmodel = 'model'\n[push]\nteam_id = 'TEAM'"
        )
        .is_err());
    }

    #[test]
    fn jwt_is_raw_es256_and_has_expected_claims() {
        let p = provider();
        let jwt = p.jwt().unwrap();
        let parts: Vec<_> = jwt.split('.').collect();
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        assert_eq!(signature.len(), 64);
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, p.key.public_key().as_ref())
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "TEAM");
        assert!(claims["iat"].as_u64().unwrap() > 0);
    }

    #[test]
    fn registry_private_persistent_and_rotation_safe() {
        let root = root();
        let push = Push::new(&root, None).unwrap();
        let old = device();
        push.update(|v| v.push(old.clone())).unwrap();
        assert_eq!(
            fs::metadata(root.join("devices.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let mut new = old.clone();
        new.token = "cd".repeat(32);
        push.update(|v| v[0] = new.clone()).unwrap();
        push.update(|v| v.retain(|d| d != &old)).unwrap();
        let loaded = Push::new(&root, None).unwrap();
        assert!(loaded.devices.lock().unwrap().contains(&new));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn observer_skips_history_and_deduplicates_event_ids() {
        let root = root();
        let session = Session::at(&root.join("sessions/s1"));
        let mut meta = SessionMeta::new("s1", &root, "model", &now());
        meta.status = Status::Waiting;
        session.create(&meta).unwrap();
        let old = Event::new("old", &now(), "t1", EventKind::Result);
        session.append(&old).unwrap();
        let push = Push::new(&root, None).unwrap();
        let mut seen = HashMap::new();
        assert!(push.scan(&mut seen, true).is_empty());
        let permission = Event::new("p1", &now(), "t2", EventKind::Permission);
        session.append(&permission).unwrap();
        assert_eq!(push.scan(&mut seen, false).len(), 1);
        assert!(push.scan(&mut seen, false).is_empty());
        session
            .append(&Event::new("a1", &now(), "t2", EventKind::PermissionAnswer))
            .unwrap();
        assert!(!eligible(&permission, &session.events().unwrap(), &meta));
        session
            .append(&Event::new("p2", &now(), "t2", EventKind::Permission))
            .unwrap();
        assert_eq!(push.scan(&mut seen, false).len(), 1);
        meta.yolo = true;
        assert!(!eligible(
            session.events().unwrap().last().unwrap(),
            &session.events().unwrap(),
            &meta
        ));
        session.set_yolo(true).unwrap();
        assert!(push.scan(&mut seen, false).is_empty());
        session.set_yolo(false).unwrap();
        assert!(push.scan(&mut seen, false).is_empty());
        meta.status = Status::Idle;
        assert!(!eligible(
            &permission,
            std::slice::from_ref(&permission),
            &meta
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subagents_never_produce_notices() {
        let root = root();
        let session = Session::at(&root.join("sessions/child"));
        let mut meta = SessionMeta::new("child", &root, "model", &now());
        meta.parent_id = Some("parent".into());
        meta.status = Status::Waiting;
        session.create(&meta).unwrap();
        let push = Push::new(&root, None).unwrap();
        for kind in [
            EventKind::Permission,
            EventKind::Question,
            EventKind::Result,
        ] {
            let event = Event::new(&generate_id(), &now(), "t1", kind);
            session.append(&event).unwrap();
            assert!(!eligible(&event, &session.events().unwrap(), &meta));
        }
        assert!(push.scan(&mut HashMap::new(), false).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn tui_heartbeat_suppresses_server_and_expires_without_replay() {
        let root = root();
        let server = Server::new(&root, &Config::default()).unwrap();
        let session = Session::at(&root.join("sessions/s1"));
        session
            .create(&SessionMeta::new("s1", &root, "model", &now()))
            .unwrap();
        server.push.update(|v| v.push(device())).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/tui/heartbeat", listener.local_addr().unwrap());
        let task = tokio::spawn(axum::serve(listener, server.router()).into_future());
        assert_eq!(
            reqwest::Client::new()
                .post(url)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        let event = Event::new("e1", &now(), "t1", EventKind::Result);
        session.append(&event).unwrap();
        let notice = Notice {
            session_id: "s1".into(),
            event_id: "e1".into(),
            kind: EventKind::Result,
        };
        let mut seen = HashMap::new();
        assert!(server.push.tui_active());
        assert!(server.push.scan(&mut seen, false).is_empty());
        assert!(!server.push.current(&device(), &notice));
        *server.push.tui_seen.lock().unwrap() =
            Some(std::time::Instant::now() - Duration::from_secs(16));
        assert!(!server.push.tui_active());
        assert!(server.push.scan(&mut seen, false).is_empty());
        session
            .append(&Event::new("e2", &now(), "t2", EventKind::Result))
            .unwrap();
        assert_eq!(server.push.scan(&mut seen, false).len(), 1);
        assert!(server.push.current(&device(), &notice));
        task.abort();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn fake_http2_apns_checks_headers_payload_retry_and_gone() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let router = Router::new().route(
            "/3/device/{token}",
            post(
                move |headers: axum::http::HeaderMap, Json(payload): Json<serde_json::Value>| {
                    let captured = captured.clone();
                    async move {
                        let mut requests = captured.lock().unwrap();
                        requests.push((headers, payload));
                        if requests.len() == 1 {
                            StatusCode::SERVICE_UNAVAILABLE
                        } else {
                            StatusCode::GONE
                        }
                    }
                },
            ),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(axum::serve(listener, router).into_future());
        let root = root();
        let session = Session::at(&root.join("sessions/s1"));
        session
            .create(&SessionMeta::new("s1", &root, "model", &now()))
            .unwrap();
        session
            .append(&Event::new("e1", &now(), "t1", EventKind::Result))
            .unwrap();
        let mut p = provider();
        p.endpoint = Some(format!("http://{addr}"));
        let push = Push {
            root: root.clone(),
            devices: Mutex::new(vec![device()]),
            provider: Some(p),
            tui_seen: Mutex::new(None),
        };
        push.deliver(
            &device(),
            &Notice {
                session_id: "s1".into(),
                event_id: "e1".into(),
                kind: EventKind::Result,
            },
        )
        .await;
        assert!(push.devices.lock().unwrap().is_empty());
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let (headers, payload) = &requests[0];
        assert_eq!(headers["apns-topic"], "sh.pascal.kyotoagent");
        assert_eq!(headers["apns-push-type"], "alert");
        assert_eq!(headers["apns-priority"], "10");
        assert_eq!(headers["apns-expiration"], "0");
        assert!(headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("Bearer "));
        assert_eq!(payload["serverId"], device().server_id);
        assert_eq!(payload["eventId"], "e1");
        assert_eq!(payload.as_object().unwrap().len(), 5);
        server.abort();
        fs::remove_dir_all(root).unwrap();
    }
}
