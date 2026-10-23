use super::*;

#[cfg(test)]
mod tests;
use std::collections::BTreeMap;
use tokio::sync::Mutex as AsyncMutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Protocol {
    Chat,
    Responses,
    Anthropic,
    Gemini,
}

#[derive(Clone)]
pub(super) struct Model {
    pub row: ModelRow,
    pub protocol: Protocol,
    pub id: String,
    pub base: String,
    pub output: u64,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

type ModelCache = Arc<AsyncMutex<Option<(std::time::Instant, Vec<Model>)>>>;

#[derive(Clone)]
pub(super) struct Client {
    http: reqwest::Client,
    auth: auth::OpenCodeAuth,
    path: PathBuf,
    base: String,
    provider: String,
    metadata: String,
    models: ModelCache,
    session: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCode")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(config: &Config, root: Option<&Path>) -> Result<Option<Self>, ChatError> {
        let Some(provider) = config.opencode_native() else {
            return Ok(None);
        };
        let root = root
            .map(Path::to_path_buf)
            .or_else(auth::default_root)
            .ok_or(ChatError::NeedLogin)?;
        Ok(Some(Self {
            http: reqwest::Client::builder()
                .user_agent(auth::opencode::USER_AGENT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(ChatError::Transport)?,
            auth: auth::OpenCodeAuth::default(),
            path: auth::opencode::token_path(&root, config.provider.as_deref().unwrap_or(provider)),
            base: config.base_url.trim_end_matches('/').into(),
            provider: if provider == "opencode-go" {
                "opencode-go"
            } else {
                "opencode"
            }
            .into(),
            metadata: "https://models.opencode.ai/api.json".into(),
            models: Arc::new(AsyncMutex::new(None)),
            session: format!("kyoto-{}-{}", std::process::id(), crate::events::now()),
        }))
    }

    pub fn set_session(&mut self, session: &str) {
        self.session = session.into();
    }

    async fn send(
        &self,
        url: &str,
        body: Option<&Value>,
        headers: &BTreeMap<String, String>,
        timeout: u64,
    ) -> Result<reqwest::Response, ChatError> {
        let mut tokens = self
            .auth
            .access(&self.path, None)
            .await
            .map_err(need_login)?;
        for attempt in 0..2 {
            let mut request = match body {
                Some(body) => self.http.post(url).json(body),
                None => self.http.get(url),
            }
            .timeout(Duration::from_secs(timeout));
            for (name, value) in headers {
                if !matches!(
                    name.to_ascii_lowercase().as_str(),
                    "authorization"
                        | "x-api-key"
                        | "x-goog-api-key"
                        | "host"
                        | "cookie"
                        | "user-agent"
                        | "x-org-id"
                        | "x-opencode-session"
                ) {
                    request = request.header(name, value);
                }
            }
            let response = request
                .bearer_auth(&tokens.access_token)
                .header("x-org-id", &tokens.org_id)
                .header("x-opencode-session", &self.session)
                .send()
                .await
                .map_err(ChatError::Transport)?;
            if response.status().as_u16() != 401 || attempt == 1 {
                return Ok(response);
            }
            tokens = self
                .auth
                .access(&self.path, Some(&tokens.access_token))
                .await
                .map_err(need_login)?;
        }
        unreachable!()
    }

    async fn json(&self, url: &str) -> Result<Value, ChatError> {
        let response = self
            .send(url, None, &BTreeMap::new(), REQUEST_TIMEOUT_SECS)
            .await?;
        let status = response.status().as_u16();
        let text = read_response_text(response, MAX_RESPONSE_BYTES).await?;
        if !(200..300).contains(&status) {
            return Err(ChatError::Status { status, body: text });
        }
        serde_json::from_str(&text).map_err(ChatError::Decode)
    }

    async fn models(&self) -> Result<Vec<Model>, ChatError> {
        let mut cache = self.models.lock().await;
        if let Some((_, models)) = cache
            .as_ref()
            .filter(|(time, _)| time.elapsed() < Duration::from_secs(60))
        {
            return Ok(models.clone());
        }
        let remote_url = format!("{}/api/v2/config", self.auth.console);
        let ids_url = format!("{}/v1/models", self.base);
        let remote = self.json(&remote_url);
        let ids = self.json(&ids_url);
        let metadata = async {
            let response = self
                .http
                .get(&self.metadata)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(ChatError::Transport)?;
            let status = response.status().as_u16();
            let text = read_response_text(response, MAX_RESPONSE_BYTES).await?;
            if !(200..300).contains(&status) {
                return Err(ChatError::Status { status, body: text });
            }
            serde_json::from_str::<Value>(&text).map_err(ChatError::Decode)
        };
        let (remote, ids, metadata) = tokio::join!(remote, ids, metadata);
        let models = discover(&self.provider, &self.base, &ids?, &metadata?, &remote?)?;
        *cache = Some((std::time::Instant::now(), models.clone()));
        Ok(models)
    }

    pub async fn catalog(&self) -> Result<Vec<ModelRow>, ChatError> {
        Ok(self
            .models()
            .await?
            .into_iter()
            .map(|model| model.row)
            .collect())
    }

    pub async fn complete(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[Tool],
        effort: Option<&str>,
        thoughts: Option<&Arc<Mutex<String>>>,
        timeout: u64,
    ) -> Result<Reply, ChatError> {
        let model = self
            .models()
            .await?
            .into_iter()
            .find(|entry| entry.row.matches(model))
            .ok_or(ChatError::NoChoice)?;
        let (url, mut body) = super::protocols::request(&model, messages, tools, effort)?;
        if let (Some(body), Some(overlay)) = (body.as_object_mut(), model.body.as_object()) {
            for (key, value) in overlay {
                if !matches!(
                    key.as_str(),
                    "model"
                        | "messages"
                        | "input"
                        | "contents"
                        | "tools"
                        | "system"
                        | "instructions"
                        | "systemInstruction"
                        | "stream"
                ) {
                    body.insert(key.clone(), value.clone());
                }
            }
        }
        let mut headers = model.headers.clone();
        if model.protocol == Protocol::Anthropic {
            headers.insert("anthropic-version".into(), "2023-06-01".into());
        }
        let response = self.send(&url, Some(&body), &headers, timeout).await?;
        super::protocols::read(response, &model, thoughts).await
    }
}

fn package(value: &str) -> Result<Protocol, ChatError> {
    match value.trim_start_matches("aisdk:") {
        "@ai-sdk/openai-compatible"
        | "@ai-sdk/mistral"
        | "@opencode/ai/providers/openai-compatible"
        | "@opencode/ai/providers/mistral" => Ok(Protocol::Chat),
        "@ai-sdk/openai" | "@opencode/ai/providers/openai" => Ok(Protocol::Responses),
        "@ai-sdk/anthropic" | "@opencode/ai/providers/anthropic" => Ok(Protocol::Anthropic),
        "@ai-sdk/google" | "@opencode/ai/providers/google" => Ok(Protocol::Gemini),
        _ => Err(ChatError::Status {
            status: 502,
            body: format!("OpenCode advertised an unsupported model protocol: {value}"),
        }),
    }
}

fn default_base(base: &str, protocol: Protocol) -> String {
    format!(
        "{base}/{}",
        match protocol {
            Protocol::Chat | Protocol::Responses => "openai/v1",
            Protocol::Anthropic => "anthropic/v1",
            Protocol::Gemini => "google/v1beta",
        }
    )
}

fn same_origin(base: &str, target: &str) -> bool {
    let (Ok(base), Ok(target)) = (reqwest::Url::parse(base), reqwest::Url::parse(target)) else {
        return false;
    };
    base.origin() == target.origin()
        && target.username().is_empty()
        && target.password().is_none()
        && target.query().is_none()
        && target.fragment().is_none()
}

fn discover(
    provider: &str,
    base: &str,
    ids: &Value,
    metadata: &Value,
    remote: &Value,
) -> Result<Vec<Model>, ChatError> {
    let source = &metadata[provider];
    let providers = remote["providers"].as_object().ok_or(ChatError::NoChoice)?;
    let overlay = providers.get(provider).or_else(|| {
        providers
            .values()
            .find(|p| p["canonical"].as_str() == Some(provider))
    });
    let empty = Value::Null;
    let overlay = overlay.unwrap_or(&empty);
    let mut names: std::collections::BTreeSet<String> = ids["data"]
        .as_array()
        .ok_or(ChatError::NoChoice)?
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_string))
        .collect();
    if let Some(models) = overlay["models"].as_object() {
        names.extend(models.keys().cloned());
    }
    let mut result = Vec::new();
    for name in names {
        let custom = &overlay["models"][&name];
        if custom["disabled"].as_bool() == Some(true) {
            continue;
        }
        let id = custom["modelID"].as_str().unwrap_or(&name).to_string();
        if id.starts_with("jev-") {
            continue;
        }
        let original = source["models"]
            .get(&id)
            .or_else(|| source["models"].get(&name))
            .or_else(|| metadata["opencode"]["models"].get(&id))
            .unwrap_or(&empty);
        let advertised_package = custom["package"]
            .as_str()
            .or_else(|| overlay["package"].as_str());
        if original.is_null() && advertised_package.is_none() {
            continue;
        }
        if original["tool_call"].as_bool() == Some(false) {
            continue;
        }
        let protocol = package(
            advertised_package
                .or_else(|| original["provider"]["npm"].as_str())
                .or_else(|| source["npm"].as_str())
                .ok_or(ChatError::NoChoice)?,
        )?;
        let endpoint = custom["settings"]["baseURL"]
            .as_str()
            .or_else(|| overlay["settings"]["baseURL"].as_str());
        let endpoint = endpoint
            .map(str::to_string)
            .unwrap_or_else(|| default_base(base, protocol));
        if !same_origin(base, &endpoint) {
            return Err(ChatError::Status {
                status: 502,
                body: "OpenCode returned a model endpoint outside its configured origin.".into(),
            });
        }
        let mut headers = BTreeMap::new();
        let mut body = serde_json::json!({});
        for value in [overlay, custom] {
            if let Some(fields) = value["headers"].as_object() {
                for (key, value) in fields {
                    if let Some(value) = value.as_str() {
                        headers.insert(key.clone(), value.into());
                    }
                }
            }
            if let Some(fields) = value["body"].as_object() {
                for (key, value) in fields {
                    body[key] = value.clone();
                }
            }
        }
        let efforts = custom["capabilities"]["reasoning"]["effort"]
            .as_array()
            .or_else(|| {
                original["reasoning_options"].as_array().and_then(|items| {
                    items
                        .iter()
                        .find(|item| item["type"] == "effort")
                        .and_then(|item| item["values"].as_array())
                })
            })
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|value| *value != "default")
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        result.push(Model {
            row: ModelRow {
                id: name,
                aliases: Vec::new(),
                reasoning_efforts: efforts,
                service_tiers: Vec::new(),
                default_reasoning_effort: None,
                context_length: custom["limit"]["context"]
                    .as_u64()
                    .or_else(|| original["limit"]["context"].as_u64()),
                provider: None,
            },
            protocol,
            id,
            base: endpoint,
            output: custom["limit"]["output"]
                .as_u64()
                .or_else(|| original["limit"]["output"].as_u64())
                .unwrap_or(8192),
            headers,
            body,
        });
    }
    if result.is_empty() {
        return Err(ChatError::NoChoice);
    }
    Ok(result)
}
