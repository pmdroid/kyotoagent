use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRow {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

impl ModelRow {
    pub fn label(&self) -> String {
        match &self.provider {
            Some(provider) => format!("{}  {provider}", self.id),
            None => self.id.clone(),
        }
    }

    pub fn takes_effort(&self) -> bool {
        !self.reasoning_efforts.is_empty()
    }

    pub fn effort_for(&self, current: Option<&str>) -> Option<String> {
        let known = |effort: &str| self.reasoning_efforts.iter().any(|row| row == effort);
        current
            .filter(|effort| known(effort))
            .map(str::to_string)
            .or_else(|| {
                self.default_reasoning_effort
                    .as_deref()
                    .filter(|effort| known(effort))
                    .map(str::to_string)
            })
            .or_else(|| self.reasoning_efforts.first().cloned())
    }

    pub fn matches(&self, model: &str) -> bool {
        self.id == model || self.aliases.iter().any(|alias| alias == model)
    }

    pub fn canonical(&self) -> String {
        match &self.provider {
            Some(provider) => format!("{provider}/{}", self.id),
            None => self.id.clone(),
        }
    }
}

pub fn canonical_model(rows: &[ModelRow], model: &str) -> Option<String> {
    let matches: Vec<_> = rows.iter().filter(|row| row.matches(model)).collect();
    (matches.len() == 1).then(|| matches[0].canonical())
}

pub fn fallback_models(config: &Config) -> Vec<ModelRow> {
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut push = |id: String, provider: Option<String>| {
        if seen.insert((id.clone(), provider.clone())) {
            rows.push(ModelRow {
                id,
                aliases: Vec::new(),
                reasoning_efforts: Vec::new(),
                default_reasoning_effort: None,
                context_length: None,
                provider,
            });
        }
    };
    push(config.model.clone(), None);
    for (id, provider) in &config.providers {
        let other = config.provider.as_deref() != Some(id.as_str());
        push(provider.model.clone(), other.then(|| id.clone()));
    }
    rows
}

pub fn parse_catalog(text: &str) -> Option<Vec<ModelRow>> {
    let value: Value = serde_json::from_str(text).ok()?;
    let data = value.get("data")?.as_array()?;
    let rows: Vec<ModelRow> = data.iter().filter_map(row_from_value).collect();
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

pub(super) fn parse_codex_catalog(text: &str) -> Option<Vec<ModelRow>> {
    let value: Value = serde_json::from_str(text).ok()?;
    let models = value.get("models")?.as_array()?;
    let rows: Vec<_> = models
        .iter()
        .filter(|model| model.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|model| {
            let id = model.get("slug")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            Some(ModelRow {
                id: id.to_string(),
                aliases: string_list(model, "aliases"),
                reasoning_efforts: codex_efforts(model),
                default_reasoning_effort: model
                    .get("default_reasoning_level")
                    .and_then(|level| level.get("effort"))
                    .and_then(Value::as_str)
                    .filter(|effort| !effort.is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        model
                            .get("default_reasoning_effort")
                            .and_then(Value::as_str)
                            .filter(|effort| !effort.is_empty())
                            .map(str::to_string)
                    }),
                context_length: model
                    .get("max_context_window")
                    .and_then(Value::as_u64)
                    .filter(|length| *length > 0)
                    .or_else(|| advertised_length(model)),
                provider: None,
            })
        })
        .collect();
    (!rows.is_empty()).then_some(rows)
}

pub(super) fn row_from_value(value: &Value) -> Option<ModelRow> {
    let id = value.get("id")?.as_str()?.to_string();
    if id.is_empty() {
        return None;
    }
    let mut reasoning_efforts = string_list(value, "reasoning_efforts");
    if reasoning_efforts.is_empty() {
        reasoning_efforts = capability_efforts(value);
    }
    Some(ModelRow {
        id,
        aliases: string_list(value, "aliases"),
        reasoning_efforts,
        default_reasoning_effort: advertised_default_effort(value),
        context_length: advertised_length(value),
        provider: None,
    })
}

fn codex_efforts(model: &Value) -> Vec<String> {
    model
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| level.get("effort").and_then(Value::as_str))
                .filter(|effort| !effort.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn advertised_default_effort(value: &Value) -> Option<String> {
    value
        .get("default_reasoning_effort")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("capabilities")
                .and_then(|capabilities| capabilities.get("default_reasoning_effort"))
                .and_then(Value::as_str)
        })
        .filter(|effort| !effort.is_empty())
        .map(str::to_string)
}

const EFFORT_ORDER: [&str; 8] = [
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

fn capability_efforts(value: &Value) -> Vec<String> {
    let Some(capabilities) = value.get("capabilities") else {
        return Vec::new();
    };
    let mut efforts = string_list(capabilities, "reasoning_effort");
    if efforts.is_empty() {
        if let Some(levels) = capabilities.get("effort").and_then(Value::as_object) {
            efforts = levels
                .iter()
                .filter(|(level, capability)| {
                    !level.is_empty()
                        && capability.get("supported").and_then(Value::as_bool) == Some(true)
                })
                .map(|(level, _)| level.clone())
                .collect();
        }
    }
    efforts.retain(|level| !level.is_empty());
    efforts.sort_by_key(|level| {
        EFFORT_ORDER
            .iter()
            .position(|known| known == level)
            .unwrap_or(usize::MAX)
    });
    efforts
}

pub(super) fn string_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_model(text: &str) -> Option<ModelRow> {
    if let Some(mut rows) = parse_catalog(text) {
        return rows.drain(..).next();
    }
    let value: Value = serde_json::from_str(text).ok()?;
    if let Some(data) = value.get("data") {
        if !data.is_array() {
            return row_from_value(data);
        }
    }
    row_from_value(&value)
}

pub(super) fn advertised_length(value: &Value) -> Option<u64> {
    for key in [
        "context_length",
        "context_window",
        "max_model_len",
        "max_input_tokens",
    ] {
        if let Some(n) = value.get(key).and_then(Value::as_u64) {
            return Some(n);
        }
    }
    None
}

pub fn merge_other_providers(rows: &mut Vec<ModelRow>, config: &Config) {
    for row in fallback_models(config) {
        if row.provider.is_some()
            && !rows
                .iter()
                .any(|existing| existing.id == row.id && existing.provider == row.provider)
        {
            rows.push(row);
        }
    }
}

pub async fn list_models(config: &Config, root: Option<&Path>) -> Vec<ModelRow> {
    model_catalog(config, root).await.models
}

#[derive(Default, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub models: Vec<ModelRow>,
    pub errors: std::collections::BTreeMap<String, String>,
}

pub async fn model_catalog(config: &Config, root: Option<&Path>) -> ModelCatalog {
    let mut catalogs = vec![(None, config.clone())];
    for id in config.providers.keys() {
        if config.provider.as_deref() == Some(id) {
            continue;
        }
        if let Ok(other) = config.for_provider(id) {
            catalogs.push((Some(id.clone()), other));
        }
    }
    let mut requests = tokio::task::JoinSet::new();
    for (provider, config) in catalogs {
        let root = root.map(Path::to_path_buf);
        requests.spawn(async move {
            let name = config.provider.clone().unwrap_or_else(|| "default".into());
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                ChatClient::in_root(&config, root.as_deref())?
                    .catalog()
                    .await
            })
            .await
            .map_err(|_| "Model request timed out.".into())
            .and_then(|result| result.map_err(catalog_error));
            (provider, name, result)
        });
    }
    let mut catalog = ModelCatalog::default();
    while let Some(reply) = requests.join_next().await {
        if let Ok((provider, name, result)) = reply {
            match result {
                Ok(mut rows) => {
                    for row in &mut rows {
                        row.provider = provider.clone();
                    }
                    catalog.models.extend(rows);
                }
                Err(error) => {
                    catalog.errors.insert(name, error);
                }
            }
        }
    }
    let rows = &mut catalog.models;
    rows.sort_by(|a, b| a.provider.cmp(&b.provider));
    let mut seen = std::collections::BTreeSet::new();
    rows.retain(|row| seen.insert((row.provider.clone(), row.id.clone())));
    catalog
}

fn catalog_error(error: ChatError) -> String {
    match error {
        ChatError::Status { status, .. } => format!("Model endpoint returned HTTP {status}."),
        ChatError::Transport(error) if error.is_timeout() => "Model request timed out.".into(),
        ChatError::Transport(error) if error.is_builder() => "Invalid model endpoint URL.".into(),
        ChatError::Transport(_) => "Could not reach the model endpoint.".into(),
        ChatError::Decode(_) | ChatError::NoChoice => {
            "Model endpoint returned no usable models.".into()
        }
        ChatError::NeedLogin => {
            "Open Providers in Kyoto Agent to sign in or enter an API key.".into()
        }
        ChatError::NeedKey => "API key is missing or could not be read.".into(),
        ChatError::Idle { .. } => "Model request timed out.".into(),
    }
}
