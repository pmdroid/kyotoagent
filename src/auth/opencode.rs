use super::*;

#[cfg(test)]
mod tests;

pub const CONSOLE: &str = "https://opencode.ai/console";
pub const CLIENT_ID: &str = "kyotoagent";
pub const USER_AGENT: &str = concat!("kyotoagent/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug)]
pub struct OpenCodeAuth {
    http: reqwest::Client,
    pub(crate) console: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OpenCodeTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: String,
    pub org_id: String,
}

#[derive(Deserialize)]
struct Device {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    org_id: Option<String>,
}

pub fn token_path(root: &Path, provider: &str) -> PathBuf {
    root.join(format!("{provider}-auth.json"))
}

pub fn load(path: &Path) -> Result<OpenCodeTokens, AuthError> {
    let text = fs::read_to_string(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            AuthError::NeedLogin
        } else {
            AuthError::Io {
                path: path.into(),
                source,
            }
        }
    })?;
    let tokens: OpenCodeTokens = serde_json::from_str(&text).map_err(AuthError::Decode)?;
    if tokens.access_token.is_empty() || tokens.refresh_token.is_empty() || tokens.org_id.is_empty()
    {
        return Err(AuthError::NeedLogin);
    }
    Ok(tokens)
}

pub fn write(path: &Path, tokens: &OpenCodeTokens) -> Result<(), AuthError> {
    write_bytes(
        path,
        &serde_json::to_vec_pretty(tokens).map_err(AuthError::Decode)?,
    )
}

impl Default for OpenCodeAuth {
    fn default() -> Self {
        Self::at(CONSOLE)
    }
}

impl OpenCodeAuth {
    pub fn at(console: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
                .build()
                .expect("OpenCode HTTP client"),
            console: console.trim_end_matches('/').into(),
        }
    }

    async fn post(&self, path: &str, body: Value) -> Result<(u16, String), AuthError> {
        let response = self
            .http
            .post(format!("{}{path}", self.console))
            .header("accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(AuthError::Transport)?;
        let status = response.status().as_u16();
        Ok((status, response.text().await.map_err(AuthError::Transport)?))
    }

    pub async fn access(
        &self,
        path: &Path,
        rejected: Option<&str>,
    ) -> Result<OpenCodeTokens, AuthError> {
        let tokens = load(path)?;
        if fresh_stamp(&tokens.expires_at) && rejected != Some(tokens.access_token.as_str()) {
            return Ok(tokens);
        }
        let _lock = refresh_lock(path).await?;
        let previous = load(path)?;
        if fresh_stamp(&previous.expires_at) && rejected != Some(previous.access_token.as_str()) {
            return Ok(previous);
        }
        let (status, text) = self.post("/auth/device/token", serde_json::json!({
            "grant_type": "refresh_token", "refresh_token": previous.refresh_token, "client_id": CLIENT_ID,
        })).await?;
        if status == 400 || status == 401 {
            return Err(AuthError::NeedLogin);
        }
        let token = decode_token(status, &text)?;
        let tokens = issued_tokens(token, Some(&previous.org_id))?;
        write(path, &tokens)?;
        Ok(tokens)
    }

    pub async fn login(
        &self,
        config: &Path,
        path: &Path,
        provider: &str,
        out: &mut impl Write,
    ) -> Result<(), AuthError> {
        let (status, text) = self
            .post(
                "/auth/device/code",
                serde_json::json!({
                    "client_id": CLIENT_ID, "supports_org_scope": true,
                }),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(AuthError::Status { status, body: text });
        }
        let device: Device = serde_json::from_str(&text).map_err(AuthError::Decode)?;
        let base =
            reqwest::Url::parse(&format!("{}/", self.console)).map_err(|_| AuthError::NeedLogin)?;
        let verification = base
            .join(&device.verification_uri_complete)
            .map_err(|_| AuthError::NeedLogin)?;
        require_https(verification.as_str())?;
        if device.user_code.chars().any(char::is_control) || device.device_code.is_empty() {
            return Err(AuthError::NeedLogin);
        }
        writeln!(out, "{verification}\n{}", device.user_code).map_err(AuthError::Output)?;
        let expires = tokio::time::Instant::now() + Duration::from_secs(device.expires_in.min(900));
        let mut interval = device.interval;
        let token = loop {
            let poll = async {
                tokio::time::sleep(Duration::from_secs(interval)).await;
                self.post("/auth/device/token", serde_json::json!({
                    "grant_type": DEVICE_GRANT_TYPE, "device_code": device.device_code, "client_id": CLIENT_ID,
                })).await
            };
            let (status, text) = tokio::time::timeout_at(expires, poll)
                .await
                .map_err(|_| AuthError::Expired)??;
            if (200..300).contains(&status) && serde_json::from_str::<TokenErr>(&text).is_err() {
                break decode_token(status, &text)?;
            }
            match serde_json::from_str::<TokenErr>(&text)
                .ok()
                .map(|error| error.error)
                .as_deref()
            {
                Some("authorization_pending") => {}
                Some("slow_down") => interval = interval.saturating_add(SLOW_DOWN_SECS),
                Some("access_denied") => return Err(AuthError::Denied),
                Some("expired_token") => return Err(AuthError::Expired),
                _ => return Err(AuthError::Status { status, body: text }),
            }
        };
        let org_id = match token.org_id.as_deref().filter(|id| !id.is_empty()) {
            Some(id) => id.to_string(),
            None => self.single_workspace(&token.access_token).await?,
        };
        let tokens = issued_tokens(token, Some(&org_id))?;
        write(path, &tokens)?;
        Config::add_opencode_native(config, provider).map_err(AuthError::Config)?;
        Ok(())
    }

    async fn single_workspace(&self, token: &str) -> Result<String, AuthError> {
        let response = self
            .http
            .get(format!("{}/api/orgs", self.console))
            .bearer_auth(token)
            .send()
            .await
            .map_err(AuthError::Transport)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(AuthError::Transport)?;
        if !(200..300).contains(&status) {
            return Err(AuthError::Status { status, body: text });
        }
        let orgs: Vec<Value> = serde_json::from_str(&text).map_err(AuthError::Decode)?;
        if orgs.len() != 1 {
            return Err(AuthError::BadAccount);
        }
        orgs[0]
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .ok_or(AuthError::BadAccount)
    }
}

fn decode_token(status: u16, text: &str) -> Result<Token, AuthError> {
    if !(200..300).contains(&status) {
        return Err(AuthError::Status {
            status,
            body: text.into(),
        });
    }
    let token: Token = serde_json::from_str(text).map_err(AuthError::Decode)?;
    if token.access_token.is_empty() || token.refresh_token.is_empty() || token.expires_in == 0 {
        return Err(AuthError::NeedLogin);
    }
    Ok(token)
}

fn issued_tokens(token: Token, previous_org: Option<&str>) -> Result<OpenCodeTokens, AuthError> {
    let org_id = token
        .org_id
        .filter(|id| !id.is_empty())
        .or_else(|| previous_org.map(str::to_string))
        .ok_or(AuthError::BadAccount)?;
    Ok(OpenCodeTokens {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: format_millis(
            now_millis().saturating_add(token.expires_in.saturating_mul(1000)) as i64,
        ),
        org_id,
    })
}
