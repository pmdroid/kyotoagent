use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub workspace: String,
    pub model: String,
    pub effort: Option<String>,
    pub yolo: bool,
    pub enhance: bool,
    pub show_closeout: bool,
    pub profile: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PairingClient {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Repository {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yolo: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PairingInfo {
    pub access_token: String,
    pub server: ServerInfo,
    pub client: PairingClient,
    pub models: Vec<crate::chat::ModelRow>,
    pub repositories: Vec<Repository>,
}

#[derive(Default, Deserialize)]
pub struct Connections {
    pub server: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, String>,
    #[serde(default)]
    pub server_names: BTreeMap<String, String>,
}

impl Connections {
    pub fn load(path: &Path) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let saved: Self =
                    toml::from_str(&text).map_err(|_| "invalid server configuration")?;
                for uri in saved.servers.values() {
                    connection(uri)?;
                }
                if saved
                    .server
                    .as_ref()
                    .is_some_and(|id| !saved.servers.contains_key(id))
                {
                    return Err("selected server is not saved".to_string());
                }
                Ok(saved)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn selected(&self) -> Option<&str> {
        self.server
            .as_ref()
            .and_then(|id| self.servers.get(id))
            .map(String::as_str)
    }

    pub fn remember(path: &Path, uri: &str, select: bool) -> Result<String, String> {
        let _guard = crate::config::Config::lock_file(path).map_err(|error| error.to_string())?;
        let (base, _) = connection(uri)?;
        Self::load(path)?;
        let id = base.trim_start_matches("https://").to_string();
        let mut doc = Self::document(path)?;
        doc["servers"][&id] = toml_edit::value(uri);
        if select {
            doc["server"] = toml_edit::value(&id);
        }
        Self::write(path, &doc)?;
        Ok(id)
    }

    pub fn select(path: &Path, id: Option<&str>) -> Result<(), String> {
        let _guard = crate::config::Config::lock_file(path).map_err(|error| error.to_string())?;
        let saved = Self::load(path)?;
        let mut doc = Self::document(path)?;
        match id {
            Some(id) if saved.servers.contains_key(id) => doc["server"] = toml_edit::value(id),
            Some(_) => return Err("server is not saved".to_string()),
            None => {
                doc.remove("server");
            }
        }
        Self::write(path, &doc)
    }

    pub fn rename(path: &Path, id: &str, name: &str) -> Result<(), String> {
        let _guard = crate::config::Config::lock_file(path).map_err(|error| error.to_string())?;
        let saved = Self::load(path)?;
        if !saved.servers.contains_key(id) {
            return Err("server is not saved".into());
        }
        let name = name.trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err("Enter a server name without control characters.".into());
        }
        if name.eq_ignore_ascii_case("local")
            || saved.servers.keys().any(|other| {
                other != id
                    && saved
                        .server_names
                        .get(other)
                        .unwrap_or(other)
                        .eq_ignore_ascii_case(name)
            })
        {
            return Err("A server already uses that name.".into());
        }
        let mut doc = Self::document(path)?;
        doc["server_names"][id] = toml_edit::value(name);
        Self::write(path, &doc)
    }

    pub fn remove(path: &Path, id: &str) -> Result<(), String> {
        let _guard = crate::config::Config::lock_file(path).map_err(|error| error.to_string())?;
        let saved = Self::load(path)?;
        if !saved.servers.contains_key(id) {
            return Err("server is not saved".into());
        }
        let mut doc = Self::document(path)?;
        for key in ["servers", "server_names"] {
            if let Some(table) = doc
                .get_mut(key)
                .and_then(toml_edit::Item::as_table_like_mut)
            {
                table.remove(id);
            }
        }
        if saved.server.as_deref() == Some(id) {
            doc.remove("server");
        }
        Self::write(path, &doc)
    }

    fn document(path: &Path) -> Result<toml_edit::DocumentMut, String> {
        match fs::read_to_string(path) {
            Ok(text) => text
                .parse()
                .map_err(|_| "invalid server configuration".to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(toml_edit::DocumentMut::new())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn write(path: &Path, doc: &toml_edit::DocumentMut) -> Result<(), String> {
        let parent = path.parent().ok_or("configuration directory missing")?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        let temporary = parent.join(format!(
            "connections-{}-{}.tmp",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(doc.to_string().as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        let _ = fs::remove_file(&temporary);
        result.map_err(|error: std::io::Error| error.to_string())
    }
}

#[derive(Clone)]
pub struct PairingKey {
    bytes: Vec<u8>,
    codes: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
struct Claims {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exp: Option<u64>,
    iss: String,
    aud: String,
}

impl PairingKey {
    pub fn load(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|error| error.to_string())?;
        let path = root.join("pairing.key");
        if !path.exists() {
            let mut bytes = [0u8; 32];
            SystemRandom::new()
                .fill(&mut bytes)
                .map_err(|_| "could not generate pairing key")?;
            static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
            let temporary = root.join(format!(
                "pairing-{}-{}.tmp",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|error| error.to_string())?;
            let result = file
                .write_all(&bytes)
                .and_then(|()| fs::hard_link(&temporary, &path));
            let _ = fs::remove_file(&temporary);
            match result {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        if fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("pairing key must be private to its owner".to_string());
        }
        let bytes = fs::read(&path).map_err(|error| error.to_string())?;
        if bytes.len() != 32 {
            return Err("invalid pairing key".to_string());
        }
        Ok(Self {
            bytes,
            codes: root.join("pairing-codes"),
        })
    }

    pub fn code(&self) -> Result<String, String> {
        fs::create_dir_all(&self.codes).map_err(|error| error.to_string())?;
        fs::set_permissions(&self.codes, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs();
        for entry in fs::read_dir(&self.codes).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if fs::read_to_string(entry.path())
                .ok()
                .and_then(|text| text.parse::<u64>().ok())
                .is_some_and(|expires| expires <= now)
            {
                let _ = fs::remove_file(entry.path());
            }
        }
        let mut bytes = [0u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| "could not generate pairing code")?;
        let code = format!(
            "pair_{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        );
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.codes.join(&code))
            .map_err(|error| error.to_string())?;
        write!(file, "{}", now + 600).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        Ok(code)
    }

    pub fn accepts_code(&self, code: &str) -> bool {
        if code.len() != 48
            || !code.starts_with("pair_")
            || !code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return false;
        }
        let Some(expires) = fs::read_to_string(self.codes.join(code))
            .ok()
            .and_then(|text| text.parse::<u64>().ok())
        else {
            return false;
        };
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .is_ok_and(|now| now.as_secs() < expires)
    }

    pub fn exchange(&self, code: &str) -> Result<String, String> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.codes.with_extension("lock"))
            .map_err(|error| error.to_string())?;
        lock.lock().map_err(|error| error.to_string())?;
        if !self.accepts_code(code) {
            return Err("pairing code expired or already used".to_string());
        }
        let token = self.token()?;
        fs::remove_file(self.codes.join(code))
            .map_err(|_| "pairing code expired or already used".to_string())?;
        Ok(token)
    }

    pub fn token(&self) -> Result<String, String> {
        let claims = Claims {
            exp: None,
            iss: "kyotoagent".to_string(),
            aud: "kyotoagent-paired-client".to_string(),
        };
        jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(&self.bytes),
        )
        .map_err(|error| error.to_string())
    }

    pub fn accepts(&self, token: &str) -> bool {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 0;
        validation.set_issuer(&["kyotoagent"]);
        validation.set_audience(&["kyotoagent-paired-client"]);
        validation.set_required_spec_claims(&["iss", "aud"]);
        jsonwebtoken::decode::<Claims>(token, &DecodingKey::from_secret(&self.bytes), &validation)
            .is_ok()
    }
}

pub fn connection(value: &str) -> Result<(String, Option<String>), String> {
    let mut url = reqwest::Url::parse(value).map_err(|_| "invalid connection URL")?;
    if url.scheme() == "https" {
        if url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("invalid HTTPS connection URL".to_string());
        }
        return Ok((url.as_str().trim_end_matches('/').to_string(), None));
    }
    if url.scheme() != "kyotoagent"
        || url.host_str().is_none()
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err("use https://host:port or kyotoagent://host:port?token=...".to_string());
    }
    let pairs: Vec<_> = url.query_pairs().collect();
    if pairs.len() != 1 || pairs[0].0 != "token" || pairs[0].1.is_empty() {
        return Err("connection URI requires one token".to_string());
    }
    let token = pairs[0].1.to_string();
    url.set_query(None);
    let base = format!(
        "https://{}:{}",
        url.host().ok_or("connection host missing")?,
        url.port().ok_or("connection port missing")?
    );
    Ok((base, Some(token)))
}

pub async fn verify(uri: &str, name: &str) -> Result<(String, PairingInfo), String> {
    let client = crate::tui::Client::at_url(uri)?;
    let identity = PairingClient {
        name: name.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let body = serde_json::to_string(&identity).map_err(|error| error.to_string())?;
    let (status, response) = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        client.request("POST", "/v1/pair", Some(&body)),
    )
    .await
    .map_err(|_| "server pairing timed out")??;
    if status != 200 {
        return Err(format!("server pairing failed with status {status}"));
    }
    let info: PairingInfo =
        serde_json::from_str(&response).map_err(|_| "invalid server pairing response")?;
    if info.access_token.is_empty()
        || info.server.name.trim().is_empty()
        || info.server.version.trim().is_empty()
        || !Path::new(&info.server.workspace).is_absolute()
        || info.client.name != identity.name
        || info.client.version != identity.version
    {
        return Err("invalid server pairing response".to_string());
    }
    let (base, _) = connection(uri)?;
    let base = reqwest::Url::parse(&base).map_err(|_| "invalid server URL")?;
    let mut saved = reqwest::Url::parse(&format!(
        "kyotoagent://{}:{}",
        base.host().ok_or("connection host missing")?,
        base.port_or_known_default()
            .ok_or("connection port missing")?
    ))
    .map_err(|_| "invalid server URL")?;
    saved
        .query_pairs_mut()
        .append_pair("token", &info.access_token);
    let saved = saved.to_string();
    crate::tui::Client::at_url(&saved)?;
    Ok((saved, info))
}
