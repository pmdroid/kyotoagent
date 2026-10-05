//! Which model this machine talks to, and with what key.
//!
//! The whole of the configuration is one TOML file, `~/.kyotoagent/config.toml`.
//! A file names the current provider and a table per provider:
//!
//! ```toml
//! provider = "office"
//!
//! [providers.office]
//! base_url = "https://openrouter.ai/api/v1"
//! model = "openai/gpt-4o"
//! api_key_env = "OPENROUTER_API_KEY"
//!
//! [providers.local]
//! base_url = "http://127.0.0.1:11434/v1"
//! model = "qwen2.5-coder"
//! ```
//!
//! `base_url` is the OpenAI-compatible root. The path is appended by
//! [`Config::chat_url`], so the file never spells out `/chat/completions`.
//! `api_key_env` is the *name* of an environment variable and never the secret.
//! A local server omits it.
//!
//! A file that still has top-level `base_url` and `model` loads as it did:
//! that is the current provider when `provider` and `[providers]` are absent.
//!
//! [`Config::chat_url`] and [`Config::api_key`] read the selected provider.
//!
//! The file is read from a path that is passed in, like a session directory, so
//! a test uses a temporary one and [`Config::default_path`] stays the server's
//! business.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file the configuration lives in, inside the `~/.kyotoagent` root.
pub const CONFIG_FILE: &str = "config.toml";

pub const EFFORTS: [&str; 4] = ["low", "medium", "high", "xhigh"];

pub const DEFAULT_TITLE_MODEL: &str = "google/gemini-3.8-flash";

pub const CODEX_KIND: &str = "codex";
pub const CODEX_RESPONSES_ROOT: &str = "https://chatgpt.com/backend-api/codex";
pub const OPENCODE_KIND: &str = "opencode";
pub const OPENCODE_DEFAULT_MODEL: &str = "kimi-k2.7-code";

pub fn opencode_zen_root() -> String {
    format!("https:{}opencode.ai/zen/v1", "/".repeat(2))
}

pub fn opencode_responses_model(model: &str) -> bool {
    model.starts_with("gpt-") || model.starts_with("grok-") || model.starts_with("muse-spark-")
}

pub fn opencode_picker_keeps(id: &str) -> bool {
    if id.starts_with("claude-") || id.starts_with("gemini-") || id.starts_with("jev-") {
        return false;
    }
    if id.starts_with("qwen3.") && id != "qwen3.8-max" {
        return false;
    }
    true
}

/// One OpenAI-compatible server: a root URL, a model id, and an optional
/// environment variable holding the key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Provider {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The OpenAI-compatible root, with no trailing `/chat/completions`.
    #[serde(default)]
    pub base_url: String,
    /// The model's id as that server spells it.
    pub model: String,
    /// The name of the environment variable holding the API key. Absent when
    /// the server has no auth, and the secret itself is never written to the
    /// file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarPane {
    Todos,
    Closeout,
    Tasks,
    Schedules,
    #[serde(rename = "artifacts", alias = "proof")]
    Proof,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub left_open: bool,
    pub left_width: u16,
    pub right_open: bool,
    pub right_width: u16,
    pub right_panes: BTreeSet<SidebarPane>,
}

impl Layout {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.left_width < 2 || self.right_width < 2 {
            return Err(ConfigError::LayoutWidth);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
struct File {
    #[serde(default)]
    layout: Option<Layout>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    providers: BTreeMap<String, Provider>,
    #[serde(default)]
    grok_client_id: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default = "default_compact_percent")]
    compact_percent: u32,
    #[serde(default = "default_prefire_percent")]
    prefire_percent: u32,
    #[serde(default)]
    listen: Option<String>,
    #[serde(default)]
    listen_cert: Option<String>,
    #[serde(default)]
    listen_key: Option<String>,
    #[serde(default)]
    title_model: Option<String>,
    #[serde(default)]
    yolo: Option<bool>,
    #[serde(default)]
    enhance: Option<bool>,
    #[serde(default)]
    show_closeout: Option<bool>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    projects: BTreeMap<String, ProjectFile>,
    #[serde(default)]
    web: WebFile,
}

#[derive(Clone, Debug, Deserialize)]
struct ProjectFile {
    path: String,
    #[serde(default)]
    closeout: Option<PathBuf>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    yolo: Option<bool>,
    #[serde(default)]
    enhance: Option<bool>,
    #[serde(default)]
    show_closeout: Option<bool>,
    #[serde(default)]
    profile: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct WebFile {
    #[serde(default)]
    exa_api_key_env: Option<String>,
    #[serde(default)]
    firecrawl_api_key_env: Option<String>,
}

/// The server and the model, read from one TOML file.
///
/// `base_url`, `model`, and `api_key_env` are the selected provider's, whether
/// that is a `[providers]` table or the top-level fields of a legacy file.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub layout: Option<Layout>,
    /// The id of the selected `[providers]` table, when the file has one.
    pub provider: Option<String>,
    /// The OpenAI-compatible root of the selected provider, with no trailing
    /// `/chat/completions`.
    ///
    /// `https://openrouter.ai/api/v1` for OpenRouter,
    /// `http://127.0.0.1:11434/v1` for a local Ollama, and
    /// `http://127.0.0.1:8080/v1` for a local llama.cpp server.
    pub base_url: String,
    /// The model's id as that server spells it. `openai/gpt-4o` is a sample,
    /// not a choice this crate makes.
    pub model: String,
    /// The name of the environment variable holding the API key, such as
    /// `OPENROUTER_API_KEY`. Absent when the server has no auth, and the secret
    /// itself is never written to the file.
    pub api_key_env: Option<String>,
    /// Named servers in the file. Empty when the file is the top-level shape.
    pub providers: BTreeMap<String, Provider>,
    pub grok_client_id: Option<String>,
    pub effort: Option<String>,
    pub compact_percent: u32,
    pub prefire_percent: u32,
    pub listen: Option<String>,
    pub listen_cert: Option<String>,
    pub listen_key: Option<String>,
    pub title_model: Option<String>,
    pub yolo: bool,
    pub enhance: bool,
    pub show_closeout: bool,
    pub profile: Option<String>,
    pub profiles: Vec<Profile>,
    pub projects: Vec<Project>,
    pub configured_web_key_envs: BTreeSet<String>,
    pub exa_api_key_env: String,
    pub firecrawl_api_key_env: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub tools: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub closeout: Option<PathBuf>,
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub yolo: Option<bool>,
    pub enhance: Option<bool>,
    pub show_closeout: Option<bool>,
    pub profile: Option<String>,
}

impl Project {
    pub fn label(&self) -> &str {
        self.name.as_str()
    }
}

fn default_compact_percent() -> u32 {
    crate::compact::DEFAULT_COMPACT_PERCENT
}

fn default_prefire_percent() -> u32 {
    crate::compact::DEFAULT_PREFIRE_PERCENT
}

fn filled(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

fn env_name(value: Option<String>, fallback: &str) -> String {
    match value {
        Some(name) if !name.trim().is_empty() => name.trim().to_string(),
        _ => fallback.to_string(),
    }
}

fn trim_dir(text: &str) -> &str {
    let text = text.trim();
    if text.len() > 1 {
        text.trim_end_matches('/')
    } else {
        text
    }
}

fn path_len(path: &Path) -> usize {
    path.components().count()
}

fn path_in(root: &Path, candidate: &str) -> bool {
    let text = trim_dir(candidate);
    if text.is_empty() {
        return false;
    }
    let child = Path::new(text);
    child == root || child.starts_with(root)
}

impl Default for Config {
    /// A configuration for the sample OpenRouter server, with no key. The
    /// server starts with this when `~/.kyotoagent/config.toml` is not there yet,
    /// so sessions can be listed and created before a model is configured;
    /// a turn then ends with the one-sentence result the chat client gives.
    fn default() -> Config {
        Config {
            layout: None,
            provider: None,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            model: "openai/gpt-4o".to_string(),
            api_key_env: None,
            providers: BTreeMap::new(),
            grok_client_id: None,
            effort: None,
            compact_percent: crate::compact::DEFAULT_COMPACT_PERCENT,
            prefire_percent: crate::compact::DEFAULT_PREFIRE_PERCENT,
            listen: None,
            listen_cert: None,
            listen_key: None,
            title_model: None,
            yolo: false,
            enhance: false,
            show_closeout: true,
            profile: None,
            profiles: Vec::new(),
            projects: Vec::new(),
            configured_web_key_envs: BTreeSet::new(),
            exa_api_key_env: crate::web::EXA_ENV.to_string(),
            firecrawl_api_key_env: crate::web::FIRECRAWL_ENV.to_string(),
        }
    }
}

impl Config {
    /// The configuration held in the text of one TOML file.
    pub fn from_toml(text: &str) -> Result<Config, ConfigError> {
        let file: File = toml::from_str(text).map_err(ConfigError::Parse)?;
        let projects = projects_in(text, &file.projects)?;
        let mut config = Config::from_file(file)?;
        if let Some(layout) = &config.layout {
            layout.validate()?;
        }
        config.projects = projects;
        config.profiles = profiles_in(text)?;
        Ok(config)
    }

    fn from_file(file: File) -> Result<Config, ConfigError> {
        let selected = match file.provider.as_deref() {
            Some(id) if !file.providers.is_empty() => Some(
                file.providers
                    .get(id)
                    .cloned()
                    .ok_or_else(|| ConfigError::UnknownProvider { id: id.to_string() })?,
            ),
            _ => None,
        };
        let (base_url, model, api_key_env) = match selected {
            Some(provider) => {
                let base_url = if provider.base_url.is_empty()
                    && provider.kind.as_deref() == Some(CODEX_KIND)
                {
                    CODEX_RESPONSES_ROOT.to_string()
                } else if provider.base_url.is_empty()
                    && provider.kind.as_deref() == Some(OPENCODE_KIND)
                {
                    opencode_zen_root()
                } else if provider.base_url.is_empty() {
                    return Err(ConfigError::Missing {
                        field: "base_url".to_string(),
                    });
                } else {
                    provider.base_url
                };
                (base_url, provider.model, provider.api_key_env)
            }
            None => {
                let base_url = file.base_url.clone().ok_or_else(|| ConfigError::Missing {
                    field: "base_url".to_string(),
                })?;
                let model = file.model.clone().ok_or_else(|| ConfigError::Missing {
                    field: "model".to_string(),
                })?;
                (base_url, model, file.api_key_env.clone())
            }
        };
        if base_url.trim().is_empty() {
            return Err(ConfigError::Missing {
                field: "base_url".to_string(),
            });
        }
        if model.trim().is_empty() {
            return Err(ConfigError::Missing {
                field: "model".to_string(),
            });
        }
        for (id, project) in &file.projects {
            if !Config::is_provider_id(id) {
                return Err(ConfigError::InvalidId { id: id.to_string() });
            }
            if project.path.is_empty() {
                return Err(ConfigError::Missing {
                    field: format!("projects.{id}.path"),
                });
            }
            let _ = project.name.as_deref();
            let _ = project.yolo;
            let _ = project.enhance;
            let _ = project.show_closeout;
            let _ = project.profile.as_deref();
        }
        let configured_web_key_envs = [
            file.web
                .exa_api_key_env
                .as_ref()
                .map(|name| env_name(Some(name.clone()), crate::web::EXA_ENV)),
            file.web
                .firecrawl_api_key_env
                .as_ref()
                .map(|name| env_name(Some(name.clone()), crate::web::FIRECRAWL_ENV)),
        ]
        .into_iter()
        .flatten()
        .collect();
        Ok(Config {
            layout: file.layout,
            provider: file.provider,
            base_url,
            model,
            api_key_env,
            providers: file.providers,
            grok_client_id: file.grok_client_id,
            effort: file.effort,
            compact_percent: file.compact_percent,
            prefire_percent: file.prefire_percent,
            listen: file.listen,
            listen_cert: file.listen_cert,
            listen_key: file.listen_key,
            title_model: file.title_model,
            yolo: file.yolo.unwrap_or(false),
            enhance: file.enhance.unwrap_or(false),
            show_closeout: file.show_closeout.unwrap_or(true),
            profile: filled(file.profile),
            profiles: Vec::new(),
            projects: Vec::new(),
            configured_web_key_envs,
            exa_api_key_env: env_name(file.web.exa_api_key_env, crate::web::EXA_ENV),
            firecrawl_api_key_env: env_name(
                file.web.firecrawl_api_key_env,
                crate::web::FIRECRAWL_ENV,
            ),
        })
    }

    pub fn yolo_for(&self, workspace: &str, requested: Option<&str>) -> bool {
        if let Some(id) = self.project_for(workspace, requested) {
            if let Some(flag) = self
                .projects
                .iter()
                .find(|project| project.id == id)
                .and_then(|project| project.yolo)
            {
                return flag;
            }
        }
        self.yolo
    }

    pub fn enhance_for(&self, workspace: &str, requested: Option<&str>) -> bool {
        if let Some(id) = self.project_for(workspace, requested) {
            if let Some(flag) = self
                .projects
                .iter()
                .find(|project| project.id == id)
                .and_then(|project| project.enhance)
            {
                return flag;
            }
        }
        self.enhance
    }

    pub fn show_closeout_for(&self, workspace: &str, requested: Option<&str>) -> bool {
        if let Some(id) = self.project_for(workspace, requested) {
            if let Some(flag) = self
                .projects
                .iter()
                .find(|project| project.id == id)
                .and_then(|project| project.show_closeout)
            {
                return flag;
            }
        }
        self.show_closeout
    }

    pub fn closeout_for(
        &self,
        workspace: &Path,
        requested: Option<&str>,
    ) -> Result<Option<crate::closeout::CloseoutFile>, crate::closeout::CloseoutError> {
        if let Some(file) = crate::closeout::read(workspace)? {
            return Ok(Some(file));
        }
        let id = self.project_for(&workspace.to_string_lossy(), requested);
        let Some(project) = self
            .projects
            .iter()
            .find(|project| Some(&project.id) == id.as_ref())
        else {
            return Ok(None);
        };
        let Some(path) = &project.closeout else {
            return Ok(None);
        };
        let path = project.path.join(path);
        let root = if path.starts_with(&project.path) {
            project.path.as_path()
        } else {
            path.parent().unwrap_or(Path::new("."))
        };
        crate::closeout::parse_from_root(&path, root).map(Some)
    }

    pub fn profile_for(&self, workspace: &str, requested: Option<&str>) -> Option<String> {
        if let Some(id) = self.project_for(workspace, requested) {
            if let Some(name) = self
                .projects
                .iter()
                .find(|project| project.id == id)
                .and_then(|project| project.profile.clone())
            {
                return Some(name);
            }
        }
        self.profile.clone()
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == name)
    }

    pub fn tool_allowed(&self, profile: Option<&str>, tool: &str) -> bool {
        let Some(name) = profile else {
            return true;
        };
        let Some(profile) = self.profile(name) else {
            return false;
        };
        match &profile.tools {
            None => true,
            Some(tools) => tools.iter().any(|item| item == tool),
        }
    }

    pub fn skill_allowance(&self, profile: Option<&str>) -> Option<Vec<String>> {
        let name = profile?;
        let Some(profile) = self.profile(name) else {
            return Some(Vec::new());
        };
        profile.skills.clone()
    }

    pub fn project_for(&self, workspace: &str, requested: Option<&str>) -> Option<String> {
        let mut best: Option<(u8, usize, &Project)> = None;
        for project in &self.projects {
            let on_requested = requested.is_some_and(|path| path_in(&project.path, path));
            let on_workspace = path_in(&project.path, workspace);
            if !on_requested && !on_workspace {
                continue;
            }
            let rank = (u8::from(on_requested), path_len(&project.path));
            let replace = match best {
                None => true,
                Some((flag, len, _)) => rank > (flag, len),
            };
            if replace {
                best = Some((rank.0, rank.1, project));
            }
        }
        best.map(|(_, _, project)| project.id.clone())
    }

    pub fn title_model(&self) -> Option<&str> {
        match self.title_model.as_deref() {
            Some("") => None,
            Some(model) if model != DEFAULT_TITLE_MODEL => Some(model),
            _ if self.provider.as_deref() == Some("grok") => Some(crate::auth::GROK_MODEL),
            _ if self.is_codex() => Some("gpt-6-luna"),
            _ if self.is_opencode() => Some("deepseek-v4.1-flash"),
            _ => Some(DEFAULT_TITLE_MODEL),
        }
    }

    pub fn title_effort(&self) -> Option<&str> {
        (self.provider.as_deref() == Some("grok")).then_some("low")
    }

    pub(crate) fn lock_file(path: &Path) -> Result<std::fs::File, ConfigError> {
        use std::os::unix::fs::OpenOptionsExt;
        let io = |source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)
            .map_err(io)?;
        file.lock().map_err(io)?;
        Ok(file)
    }

    pub fn edit(
        path: &Path,
        update: impl FnOnce(&mut toml_edit::DocumentMut),
    ) -> Result<Config, ConfigError> {
        let _guard = Self::lock_file(path)?;
        let io = |source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut doc: toml_edit::DocumentMut = match fs::read_to_string(path) {
            Ok(text) => text.parse().map_err(ConfigError::Edit)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let defaults = Config::default();
                let mut doc = toml_edit::DocumentMut::new();
                doc["base_url"] = toml_edit::value(defaults.base_url);
                doc["model"] = toml_edit::value(defaults.model);
                doc
            }
            Err(error) => return Err(io(error)),
        };
        update(&mut doc);
        Self::write_document(path, &doc)
    }

    fn write_document(path: &Path, doc: &toml_edit::DocumentMut) -> Result<Config, ConfigError> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::sync::atomic::{AtomicU64, Ordering};
        let io = |source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        };
        let text = doc.to_string();
        let config = Config::from_toml(&text)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = path.with_extension(format!(
            "{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        let _ = fs::remove_file(temporary);
        result.map_err(io)?;
        Ok(config)
    }

    pub fn write_missing_title_model(path: &Path) -> Result<(), ConfigError> {
        let _guard = Self::lock_file(path)?;
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut doc: toml_edit::DocumentMut = text.parse().map_err(ConfigError::Edit)?;
        if doc.get("title_model").is_some() {
            return Ok(());
        }
        doc["title_model"] = toml_edit::value(DEFAULT_TITLE_MODEL);
        Self::write_document(path, &doc).map(|_| ())
    }

    /// The configuration in the file at `path`, which is `~/.kyotoagent/config.toml`
    /// in normal use.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        match Config::from_toml(&text) {
            Ok(config) => Ok(config),
            Err(ConfigError::Parse(source)) => Err(ConfigError::Toml {
                path: path.to_path_buf(),
                source,
            }),
            Err(other) => Err(other),
        }
    }

    /// Whether `id` is a provider id: lowercase letters, digits, and dashes.
    pub fn is_provider_id(id: &str) -> bool {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }

    /// Write `provider = "<id>"` in the file at `path`. The id has to name a
    /// table already in the file; a missing id leaves the file unchanged.
    pub fn for_provider(&self, id: &str) -> Result<Config, ConfigError> {
        let provider = self
            .providers
            .get(id)
            .ok_or_else(|| ConfigError::UnknownProvider { id: id.into() })?;
        let mut config = self.clone();
        config.provider = Some(id.into());
        config.base_url = if !provider.base_url.is_empty() {
            provider.base_url.clone()
        } else if provider.kind.as_deref() == Some(CODEX_KIND) {
            CODEX_RESPONSES_ROOT.into()
        } else if provider.kind.as_deref() == Some(OPENCODE_KIND) {
            opencode_zen_root()
        } else {
            return Err(ConfigError::Missing {
                field: "base_url".into(),
            });
        };
        config.model = provider.model.clone();
        config.api_key_env = provider.api_key_env.clone();
        Ok(config)
    }

    pub fn use_provider(path: &Path, id: &str) -> Result<Config, ConfigError> {
        let _guard = Self::lock_file(path)?;
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let config = Config::from_toml(&text)?;
        if !config.providers.contains_key(id) {
            return Err(ConfigError::UnknownProvider { id: id.to_string() });
        }
        let mut doc: toml_edit::DocumentMut = text.parse().map_err(ConfigError::Edit)?;
        doc["provider"] = toml_edit::value(id);
        Self::write_document(path, &doc)
    }

    /// Insert a `[providers.<id>]` table, replace it when the id is already
    /// there, and select it. The file is created when it is not there yet.
    pub fn add_provider(
        path: &Path,
        id: &str,
        base_url: &str,
        model: &str,
        api_key_env: Option<&str>,
    ) -> Result<Config, ConfigError> {
        Config::insert_provider(path, id, None, base_url, model, api_key_env)
    }

    pub fn add_opencode(
        path: &Path,
        base_url: Option<&str>,
        model: Option<&str>,
        api_key_env: Option<&str>,
    ) -> Result<Config, ConfigError> {
        let base = match base_url.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => value.to_string(),
            None => opencode_zen_root(),
        };
        let model = model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(OPENCODE_DEFAULT_MODEL);
        Config::insert_provider(
            path,
            "opencode",
            Some(OPENCODE_KIND),
            &base,
            model,
            api_key_env,
        )
    }

    fn insert_provider(
        path: &Path,
        id: &str,
        kind: Option<&str>,
        base_url: &str,
        model: &str,
        api_key_env: Option<&str>,
    ) -> Result<Config, ConfigError> {
        let _guard = Self::lock_file(path)?;
        if !Config::is_provider_id(id) {
            return Err(ConfigError::InvalidId { id: id.to_string() });
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut doc = match fs::read_to_string(path) {
            Ok(text) if text.trim().is_empty() => toml_edit::DocumentMut::new(),
            Ok(text) => text.parse().map_err(ConfigError::Edit)?,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                toml_edit::DocumentMut::new()
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if let Some(providers) = doc
            .get_mut("providers")
            .and_then(|item| item.as_table_like_mut())
        {
            providers.remove(id);
        }
        if let Some(kind) = kind {
            doc["providers"][id]["kind"] = toml_edit::value(kind);
        }
        doc["providers"][id]["base_url"] = toml_edit::value(base_url);
        doc["providers"][id]["model"] = toml_edit::value(model);
        if let Some(name) = api_key_env {
            doc["providers"][id]["api_key_env"] = toml_edit::value(name);
        }
        doc["provider"] = toml_edit::value(id);
        Self::write_document(path, &doc)
    }

    pub fn write_codex_table(path: &Path) -> Result<Config, ConfigError> {
        let _guard = Self::lock_file(path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut doc = match fs::read_to_string(path) {
            Ok(text) if text.trim().is_empty() => toml_edit::DocumentMut::new(),
            Ok(text) => text.parse().map_err(ConfigError::Edit)?,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                toml_edit::DocumentMut::new()
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let existing = doc
            .get("providers")
            .and_then(|providers| providers.get("codex"))
            .map(|item| {
                (
                    item.get("kind")
                        .and_then(|kind| kind.as_str())
                        .map(str::to_string),
                    item.get("model")
                        .and_then(|model| model.as_str())
                        .filter(|model| !model.is_empty())
                        .map(str::to_string),
                )
            });
        match existing {
            None => {
                doc["providers"]["codex"]["kind"] = toml_edit::value(CODEX_KIND);
                doc["providers"]["codex"]["model"] = toml_edit::value("gpt-6.1-sol");
            }
            Some((kind, model)) => {
                if kind.as_deref() != Some(CODEX_KIND) {
                    doc["providers"]["codex"]["kind"] = toml_edit::value(CODEX_KIND);
                }
                if model.is_none() {
                    doc["providers"]["codex"]["model"] = toml_edit::value("gpt-6.1-sol");
                }
            }
        }
        doc["provider"] = toml_edit::value("codex");
        Self::write_document(path, &doc)
    }

    /// The API key, read from the environment at the moment it is asked for.
    ///
    /// Nothing comes back when the file named no variable, or named one that
    /// is unset or empty, and the caller then sends no `Authorization` header
    /// at all. That is the local-server case, and it is not an error.
    pub fn api_key(&self) -> Option<String> {
        let name = self.api_key_env.as_deref()?;
        let value = std::env::var(name).ok()?;
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    }

    pub(crate) fn search_vendor(&self) -> Option<crate::web::Vendor> {
        crate::web::pick_search(&self.exa_api_key_env, &self.firecrawl_api_key_env, |name| {
            std::env::var(name).ok()
        })
        .map(|(vendor, _key)| vendor)
    }

    /// The URL of a chat completion, which is `base_url` and
    /// `/chat/completions`. A trailing slash on `base_url` does not double up.
    pub fn chat_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if self.is_codex() || (self.is_opencode() && opencode_responses_model(&self.model)) {
            if base.ends_with("/responses") {
                base.to_string()
            } else {
                format!("{base}/responses")
            }
        } else {
            format!("{base}/chat/completions")
        }
    }

    fn selected_kind(&self) -> Option<&str> {
        self.provider
            .as_deref()
            .and_then(|id| self.providers.get(id))
            .and_then(|provider| provider.kind.as_deref())
    }

    pub fn is_codex(&self) -> bool {
        self.selected_kind() == Some(CODEX_KIND)
    }

    pub fn is_opencode(&self) -> bool {
        self.selected_kind() == Some(OPENCODE_KIND)
    }

    pub fn models_url(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }

    pub fn takes_effort(&self) -> bool {
        self.provider.as_deref() == Some("grok")
            || self
                .provider
                .as_deref()
                .and_then(|id| self.providers.get(id))
                .and_then(|provider| provider.effort.as_ref())
                .is_some()
    }

    pub fn request_effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }

    pub fn provider_context_window(&self) -> Option<u64> {
        self.provider
            .as_deref()
            .and_then(|id| self.providers.get(id))
            .and_then(|provider| provider.context_window)
    }

    pub fn set_model(path: &Path, model: &str) -> Result<Config, ConfigError> {
        Self::set_model_options(path, None, model, None)
    }

    pub fn set_provider_model(
        path: &Path,
        provider: &str,
        model: &str,
    ) -> Result<Config, ConfigError> {
        Self::set_model_options(path, Some(provider), model, None)
    }

    pub(crate) fn set_model_options(
        path: &Path,
        provider: Option<&str>,
        model: &str,
        effort: Option<Option<&str>>,
    ) -> Result<Config, ConfigError> {
        let _guard = Self::lock_file(path)?;
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let config = Config::from_toml(&text)?;
        if let Some(id) = provider {
            if !config.providers.contains_key(id) {
                return Err(ConfigError::UnknownProvider { id: id.to_string() });
            }
        }
        let mut doc: toml_edit::DocumentMut = text.parse().map_err(ConfigError::Edit)?;
        if let Some(id) = provider {
            doc["provider"] = toml_edit::value(id);
        }
        match provider.or(config.provider.as_deref()) {
            Some(id) if config.providers.contains_key(id) => {
                doc["providers"][id]["model"] = toml_edit::value(model);
            }
            _ => doc["model"] = toml_edit::value(model),
        }
        if let Some(effort) = effort {
            match effort {
                Some(value) => doc["effort"] = toml_edit::value(value),
                None => {
                    doc.remove("effort");
                }
            }
        }
        Self::write_document(path, &doc)
    }

    pub fn set_effort(path: &Path, effort: Option<&str>) -> Result<Config, ConfigError> {
        let _guard = Self::lock_file(path)?;
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let _ = Config::from_toml(&text)?;
        let mut doc: toml_edit::DocumentMut = text.parse().map_err(ConfigError::Edit)?;
        match effort {
            Some(value) => doc["effort"] = toml_edit::value(value),
            None => {
                doc.remove("effort");
            }
        }
        Self::write_document(path, &doc)
    }

    pub fn set_listen(path: &Path, listen: &str) -> Result<Config, ConfigError> {
        let _guard = Self::lock_file(path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut doc = match fs::read_to_string(path) {
            Ok(text) if text.trim().is_empty() => toml_edit::DocumentMut::new(),
            Ok(text) => text.parse().map_err(ConfigError::Edit)?,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                toml_edit::DocumentMut::new()
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if doc.get("base_url").is_none()
            && doc.get("model").is_none()
            && doc.get("providers").is_none()
        {
            doc["base_url"] = toml_edit::value("https://openrouter.ai/api/v1");
            doc["model"] = toml_edit::value("openai/gpt-4o");
        }
        doc["listen"] = toml_edit::value(listen);
        Self::write_document(path, &doc)
    }

    /// `~/.kyotoagent/config.toml`, when there is a home directory to put it in.
    pub fn default_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        let home = PathBuf::from(home);
        if home.as_os_str().is_empty() {
            None
        } else {
            Some(home.join(".kyotoagent").join(CONFIG_FILE))
        }
    }
}

/// A configuration file that could not be used.
#[derive(Debug)]
pub enum ConfigError {
    LayoutWidth,
    /// The file could not be read or written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file held something that is not a configuration.
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    /// The text was not TOML.
    Parse(toml::de::Error),
    /// The file could not be edited in place.
    Edit(toml_edit::TomlError),
    /// `provider` named a table that is not in the file.
    UnknownProvider {
        id: String,
    },
    /// An id that is not `[a-z0-9-]+`.
    InvalidId {
        id: String,
    },
    /// A required field was missing.
    Missing {
        field: String,
    },
    /// A profile named a tool this agent does not have.
    UnknownTool {
        profile: String,
        tool: String,
    },
    /// A profile field was not the list the file expects.
    ProfileList {
        field: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::LayoutWidth => write!(f, "Layout widths must be at least two columns."),
            ConfigError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            ConfigError::Toml { path, source } => write!(f, "{}: {source}", path.display()),
            ConfigError::Parse(source) => write!(f, "{source}"),
            ConfigError::Edit(source) => write!(f, "{source}"),
            ConfigError::UnknownProvider { id } => write!(f, "no provider named {id}"),
            ConfigError::InvalidId { id } => {
                write!(f, "an id is lowercase letters, digits, and dashes: {id}")
            }
            ConfigError::Missing { field } => {
                write!(f, "the configuration is missing {field}")
            }
            ConfigError::UnknownTool { profile, tool } => {
                write!(f, "profile {profile} names an unknown tool: {tool}")
            }
            ConfigError::ProfileList { field } => {
                write!(f, "{field} is a list of names")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

fn projects_in(
    text: &str,
    definitions: &BTreeMap<String, ProjectFile>,
) -> Result<Vec<Project>, ConfigError> {
    let doc: toml_edit::DocumentMut = text.parse().map_err(ConfigError::Edit)?;
    let Some(table) = doc.get("projects").and_then(|item| item.as_table()) else {
        return Ok(Vec::new());
    };
    let mut projects = Vec::new();
    for (id, item) in table.iter() {
        if !Config::is_provider_id(id) {
            return Err(ConfigError::InvalidId { id: id.to_string() });
        }
        let path = project_str(item, "path").unwrap_or_default();
        if path.is_empty() {
            return Err(ConfigError::Missing {
                field: format!("projects.{id}.path"),
            });
        }
        let name = project_str(item, "name")
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.to_string());
        if definitions
            .get(id)
            .and_then(|project| project.closeout.as_ref())
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ConfigError::Missing {
                field: format!("projects.{id}.closeout"),
            });
        }
        projects.push(Project {
            closeout: definitions
                .get(id)
                .and_then(|project| project.closeout.clone()),
            id: id.to_string(),
            path: PathBuf::from(path),
            name,
            yolo: project_bool(item, "yolo"),
            enhance: project_bool(item, "enhance"),
            show_closeout: project_bool(item, "show_closeout"),
            profile: project_str(item, "profile").filter(|name| !name.trim().is_empty()),
        });
    }
    Ok(projects)
}

fn profiles_in(text: &str) -> Result<Vec<Profile>, ConfigError> {
    let doc: toml_edit::DocumentMut = text.parse().map_err(ConfigError::Edit)?;
    let Some(table) = doc.get("profiles").and_then(|item| item.as_table()) else {
        return Ok(Vec::new());
    };
    let mut profiles = Vec::new();
    for (id, item) in table.iter() {
        let tools = string_list(item, &format!("profiles.{id}.tools"))?;
        let skills = string_list(item, &format!("profiles.{id}.skills"))?;
        if let Some(tools) = &tools {
            for tool in tools {
                if !crate::turn::known_tool_names().contains(&tool.as_str()) {
                    return Err(ConfigError::UnknownTool {
                        profile: id.to_string(),
                        tool: tool.clone(),
                    });
                }
            }
        }
        profiles.push(Profile {
            id: id.to_string(),
            tools,
            skills,
        });
    }
    Ok(profiles)
}

fn string_list(item: &toml_edit::Item, field: &str) -> Result<Option<Vec<String>>, ConfigError> {
    let key = field.rsplit('.').next().unwrap_or(field);
    if let Some(table) = item.as_table() {
        return name_list(table.get(key).map(toml_edit::Item::as_array), field);
    }
    if let Some(table) = item.as_inline_table() {
        return name_list(table.get(key).map(toml_edit::Value::as_array), field);
    }
    Ok(None)
}

fn name_list(
    array: Option<Option<&toml_edit::Array>>,
    field: &str,
) -> Result<Option<Vec<String>>, ConfigError> {
    let Some(array) = array else {
        return Ok(None);
    };
    let Some(array) = array else {
        return Err(ConfigError::ProfileList {
            field: field.to_string(),
        });
    };
    let mut names = Vec::new();
    for value in array.iter() {
        let Some(text) = value.as_str() else {
            return Err(ConfigError::ProfileList {
                field: field.to_string(),
            });
        };
        names.push(text.to_string());
    }
    Ok(Some(names))
}

fn project_bool(item: &toml_edit::Item, key: &str) -> Option<bool> {
    let from_table = item
        .as_table()
        .and_then(|table| table.get(key))
        .and_then(|value| value.as_bool());
    let from_inline = item
        .as_inline_table()
        .and_then(|table| table.get(key))
        .and_then(|value| value.as_bool());
    from_table.or(from_inline)
}

fn project_str(item: &toml_edit::Item, key: &str) -> Option<String> {
    let from_table = item
        .as_table()
        .and_then(|table| table.get(key))
        .and_then(|value| value.as_str());
    let from_inline = item
        .as_inline_table()
        .and_then(|table| table.get(key))
        .and_then(|value| value.as_str());
    from_table.or(from_inline).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test names its own variable, because the environment is one
    /// process-wide thing and the tests in a binary run at the same time.
    fn set_var(name: &str, value: &str) {
        std::env::set_var(name, value);
    }

    fn clear_var(name: &str) {
        std::env::remove_var(name);
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kyotoagent-config-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the temp dir exists");
        dir.join(CONFIG_FILE)
    }

    #[test]
    fn invalid_model_changes_leave_the_configuration_unchanged() {
        let path = temp_path("reject-invalid-model");
        let before = r#"base_url = "http://localhost/v1"
model = "existing"
"#;
        fs::write(&path, before).unwrap();
        Config::load(&path).unwrap();
        assert!(Config::set_model(&path, "").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert!(Config::add_provider(&path, "invalid", "", "", None).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn overlapping_config_edits_preserve_both_changes() {
        let root =
            std::env::temp_dir().join(format!("kyoto-config-overlap-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        fs::write(
            &path,
            r#"base_url = "http://localhost/v1"
model = "test"
[projects.first]
path = "/first"
[projects.second]
path = "/second"
"#,
        )
        .unwrap();
        let (entered, waiting) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (second_entered, second_waiting) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let first_path = &path;
            let first = scope.spawn(move || {
                Config::edit(first_path, |doc| {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                    doc["projects"].as_table_like_mut().unwrap().remove("first");
                })
            });
            waiting.recv().unwrap();
            let second_path = &path;
            let second = scope.spawn(move || {
                Config::edit(second_path, |doc| {
                    doc["projects"]
                        .as_table_like_mut()
                        .unwrap()
                        .remove("second");
                    second_entered.send(()).unwrap();
                })
            });
            let second_finished = second_waiting
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_ok();
            if second_finished {
                second.join().unwrap().unwrap();
                release.send(()).unwrap();
                first.join().unwrap().unwrap();
            } else {
                release.send(()).unwrap();
                first.join().unwrap().unwrap();
                second.join().unwrap().unwrap();
            }
        });
        assert!(Config::load(&path).unwrap().projects.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_openrouter_file_reads_as_it_is_written() {
        let config = Config::from_toml(
            r#"
base_url = "https://openrouter.ai/api/v1"
model = "openai/gpt-4o"
api_key_env = "OPENROUTER_API_KEY"
"#,
        )
        .expect("the file parses");

        assert_eq!(config.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(config.model, "openai/gpt-4o");
        assert_eq!(config.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
        assert_eq!(
            config.chat_url(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
    }

    #[test]
    fn a_local_file_is_the_same_shape_without_a_key() {
        let config = Config::from_toml(
            r#"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5-coder"
"#,
        )
        .expect("the file parses");

        assert_eq!(config.api_key_env, None);
        assert_eq!(config.model, "qwen2.5-coder");
        assert_eq!(
            config.chat_url(),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(config.api_key(), None, "no name, so no key");
    }

    #[test]
    fn a_leftover_max_steps_key_still_loads() {
        let config = Config::from_toml(
            r#"
base_url = "https://openrouter.ai/api/v1"
model = "openai/gpt-4o"
max_steps = 40
"#,
        )
        .expect("a leftover max_steps key still loads");

        assert_eq!(config.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(config.model, "openai/gpt-4o");
    }

    #[test]
    fn a_file_with_only_base_url_and_model_loads() {
        let config = Config::from_toml(
            r#"
base_url = "http://127.0.0.1:8080/v1/"
model = "local"
"#,
        )
        .expect("a file with only the two required fields parses");

        assert_eq!(
            config.chat_url(),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
    }

    #[test]
    fn a_codex_table_without_a_base_url_posts_to_responses() {
        let config = Config::from_toml(
            "provider = \"codex\"\n\n[providers.codex]\nkind = \"codex\"\nmodel = \"gpt-6.1-sol\"\n",
        )
        .expect("the codex table parses");
        assert!(config.is_codex());
        assert!(config.api_key_env.is_none());
        assert_eq!(
            config.chat_url(),
            format!("{CODEX_RESPONSES_ROOT}/responses")
        );
        assert!(!config.chat_url().contains("completions"));
    }

    #[test]
    fn a_codex_table_keeps_an_explicit_base() {
        let config = Config::from_toml(
            "provider = \"codex\"\n\n[providers.codex]\nkind = \"codex\"\nbase_url = \"local\"\nmodel = \"gpt-6.1-sol\"\n",
        )
        .expect("the codex table parses");
        assert_eq!(config.base_url, "local");
        assert_eq!(config.chat_url(), "local/responses");
    }

    #[test]
    fn a_provider_table_without_a_base_url_is_rejected() {
        let error =
            Config::from_toml("provider = \"office\"\n\n[providers.office]\nmodel = \"m\"\n")
                .expect_err("a base url is required");
        assert!(matches!(error, ConfigError::Missing { .. }), "{error}");
    }

    #[test]
    fn the_key_is_read_from_the_environment_and_not_stored() {
        let name = "KYOTOAGENT_TEST_KEY_IN_THE_ENVIRONMENT";
        set_var(name, "sk-from-the-environment");
        let config = Config::from_toml(&format!(
            "base_url = \"https://openrouter.ai/api/v1\"\nmodel = \"m\"\napi_key_env = \"{name}\"\n"
        ))
        .expect("the file parses");

        assert_eq!(config.api_key().as_deref(), Some("sk-from-the-environment"));
        // The name is in the file. The secret never was.
        assert_eq!(config.api_key_env.as_deref(), Some(name));
        assert!(!format!("{config:?}").contains("sk-from-the-environment"));

        clear_var(name);
    }

    #[test]
    fn an_unset_or_empty_variable_is_the_same_as_no_variable() {
        let unset = "KYOTOAGENT_TEST_KEY_THAT_IS_NOT_SET";
        clear_var(unset);
        let empty = "KYOTOAGENT_TEST_KEY_THAT_IS_EMPTY";
        set_var(empty, "");

        for name in [unset, empty] {
            let config = Config::from_toml(&format!(
                "base_url = \"http://127.0.0.1:11434/v1\"\nmodel = \"m\"\napi_key_env = \"{name}\"\n"
            ))
            .expect("the file parses");
            assert_eq!(config.api_key(), None, "{name} gives no key");
        }

        clear_var(empty);
    }

    #[test]
    fn a_file_that_is_not_a_configuration_names_the_path() {
        let error = Config::from_toml("base_url = 7\n").expect_err("a number is not a url");
        assert!(error.to_string().contains("base_url"), "{error}");
    }

    #[test]
    fn a_missing_file_names_its_own_path() {
        let path = PathBuf::from("/kyotoagent/no-such-config.toml");
        let error = Config::load(&path).expect_err("there is no such file");
        assert!(error.to_string().contains("no-such-config.toml"), "{error}");
    }

    #[test]
    fn the_default_path_is_inside_the_kyotoagent_directory() {
        // The path is derived, not invented: it is the one the server reads.
        // HOME is not touched here, so this only asserts the shape when a home
        // directory exists at all.
        if let Some(path) = Config::default_path() {
            assert!(path.ends_with(".kyotoagent/config.toml"), "{path:?}");
        }
    }

    #[test]
    fn a_two_provider_file_uses_the_selected_table() {
        let name = "KYOTOAGENT_TEST_PROVIDER_OFFICE_KEY";
        set_var(name, "sk-office");
        let config = Config::from_toml(&format!(
            r#"
provider = "office"

[providers.office]
base_url = "https://openrouter.ai/api/v1"
model = "openai/gpt-4o"
api_key_env = "{name}"

[providers.local]
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5-coder"
"#
        ))
        .expect("the file parses");

        assert_eq!(config.provider.as_deref(), Some("office"));
        assert_eq!(config.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(config.model, "openai/gpt-4o");
        assert_eq!(config.api_key().as_deref(), Some("sk-office"));
        assert_eq!(
            config.chat_url(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(config.providers.len(), 2);
        assert_eq!(
            config.providers["local"].api_key_env, None,
            "a local table has no key"
        );

        clear_var(name);
    }

    #[test]
    fn use_provider_writes_the_id_and_refuses_a_missing_one() {
        let path = temp_path("use");
        fs::write(
            &path,
            r#"
provider = "office"

[providers.office]
base_url = "https://openrouter.ai/api/v1"
model = "openai/gpt-4o"
api_key_env = "OPENROUTER_API_KEY"

[providers.local]
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5-coder"
"#,
        )
        .expect("the file writes");
        let before = fs::read(&path).expect("the file reads");

        let error = Config::use_provider(&path, "missing").expect_err("a missing id is refused");
        assert!(
            matches!(error, ConfigError::UnknownProvider { ref id } if id == "missing"),
            "{error:?}"
        );
        assert_eq!(
            fs::read(&path).expect("the file still reads"),
            before,
            "a refused use leaves the file unchanged"
        );

        let config = Config::use_provider(&path, "local").expect("local is in the file");
        assert_eq!(config.provider.as_deref(), Some("local"));
        assert_eq!(config.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(config.model, "qwen2.5-coder");
        assert_eq!(config.api_key_env, None);
        let text = fs::read_to_string(&path).expect("the file reads");
        assert!(text.contains("provider = \"local\""), "{text}");
    }

    #[test]
    fn add_provider_writes_the_table_and_a_second_add_replaces_the_url() {
        let path = temp_path("add");
        let first = Config::add_provider(
            &path,
            "office",
            "https://openrouter.ai/api/v1",
            "openai/gpt-4o",
            Some("OPENROUTER_API_KEY"),
        )
        .expect("the first add writes");
        assert_eq!(first.provider.as_deref(), Some("office"));
        assert_eq!(first.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(first.model, "openai/gpt-4o");
        assert_eq!(first.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));

        let second = Config::add_provider(
            &path,
            "office",
            "http://127.0.0.1:8080/v1",
            "local-model",
            None,
        )
        .expect("the second add replaces");
        assert_eq!(second.provider.as_deref(), Some("office"));
        assert_eq!(second.base_url, "http://127.0.0.1:8080/v1");
        assert_eq!(second.model, "local-model");
        assert_eq!(second.api_key_env, None);
        let text = fs::read_to_string(&path).expect("the file reads");
        assert!(text.contains("http://127.0.0.1:8080/v1"), "{text}");
        assert!(
            !text.contains("openrouter.ai"),
            "the old url is gone: {text}"
        );
        assert!(
            !text.contains("OPENROUTER_API_KEY"),
            "the old key name is gone: {text}"
        );
    }

    #[test]
    fn add_provider_refuses_an_id_that_is_not_lowercase() {
        let path = temp_path("bad-id");
        let error = Config::add_provider(&path, "Office", "http://127.0.0.1:1/v1", "m", None)
            .expect_err("uppercase is refused");
        assert!(matches!(error, ConfigError::InvalidId { ref id } if id == "Office"));
        assert!(!path.exists(), "a refused add writes nothing");
    }

    #[test]
    fn add_opencode_selects_zen_and_keeps_the_key_out_of_the_file() {
        let path = temp_path("opencode-add");
        let config = Config::add_opencode(&path, None, None, None).expect("opencode writes");
        assert_eq!(config.provider.as_deref(), Some("opencode"));
        assert!(config.is_opencode());
        assert!(!config.is_codex());
        assert_eq!(config.model, OPENCODE_DEFAULT_MODEL);
        assert_eq!(config.base_url, opencode_zen_root());
        assert!(config.api_key_env.is_none());
        assert_eq!(
            config.chat_url(),
            format!("{}/chat/completions", config.base_url)
        );
        let text = fs::read_to_string(&path).expect("the file reads");
        assert!(text.contains("kind = \"opencode\""), "{text}");
        assert!(!text.contains("api_key"), "{text}");
        let replaced = Config::add_opencode(&path, None, Some("gpt-6.1-sol"), Some("ZEN_KEY"))
            .expect("the second add replaces");
        assert_eq!(replaced.model, "gpt-6.1-sol");
        assert_eq!(replaced.api_key_env.as_deref(), Some("ZEN_KEY"));
        assert!(replaced.chat_url().ends_with("/responses"));
        assert!(opencode_responses_model("grok-4"));
        assert!(opencode_responses_model("muse-spark-1"));
        assert!(!opencode_responses_model(OPENCODE_DEFAULT_MODEL));
        assert!(opencode_picker_keeps("qwen3.8-max"));
        assert!(opencode_picker_keeps("big-pickle"));
        assert!(!opencode_picker_keeps("claude-sonnet-5"));
        assert!(!opencode_picker_keeps("gemini-3-flash"));
        assert!(!opencode_picker_keeps("jev-1.13"));
        assert!(!opencode_picker_keeps("qwen3.6"));
        let text = fs::read_to_string(&path).expect("the file reads");
        assert!(!text.contains(OPENCODE_DEFAULT_MODEL), "{text}");
        assert!(!text.contains("ZEN_KEY_VALUE"));
    }

    #[test]
    fn a_grok_chat_url_stays_on_completions() {
        let host = format!("https:{}api.x.ai/v1", "/".repeat(2));
        let config = Config::from_toml(&format!(
            "provider = \"grok\"\n\n[providers.grok]\nbase_url = \"{host}\"\nmodel = \"grok-4.6\"\n"
        ))
        .expect("the grok table parses");
        assert!(!config.is_opencode());
        assert_eq!(config.chat_url(), format!("{host}/chat/completions"));
    }

    #[test]
    fn grok_client_id_is_read_from_the_file() {
        let config = Config::from_toml(
            r#"
provider = "grok"
grok_client_id = "rotated-client"

[providers.grok]
base_url = "https://api.x.ai/v1"
model = "grok-4.6"
"#,
        )
        .expect("the file parses");
        assert_eq!(config.grok_client_id.as_deref(), Some("rotated-client"));
        assert_eq!(config.provider.as_deref(), Some("grok"));
        assert_eq!(config.api_key_env, None);
    }

    #[test]
    fn effort_is_read_and_grok_takes_it() {
        let config = Config::from_toml(
            r#"
provider = "grok"
effort = "high"

[providers.grok]
base_url = "x"
model = "grok-4.6"
"#,
        )
        .expect("the file parses");
        assert_eq!(config.effort.as_deref(), Some("high"));
        assert!(config.takes_effort());
        assert_eq!(config.request_effort(), Some("high"));
        assert_eq!(config.models_url(), "x/models");
        assert_eq!(config.compact_percent, 85);
        assert_eq!(config.prefire_percent, 70);
        assert_eq!(config.provider_context_window(), None);
    }

    #[test]
    fn the_omitted_title_model_uses_the_provider_default() {
        let grok = Config::from_toml(
            "provider = \"grok\"\n\n[providers.grok]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"grok-4.6\"\n",
        )
        .expect("grok loads");
        assert_eq!(grok.title_model(), Some("grok-4.6"));
        assert_eq!(grok.title_effort(), Some("low"));

        let local = Config::from_toml(
            "provider = \"local\"\n\n[providers.local]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"m\"\n",
        )
        .expect("local loads");
        assert_eq!(local.title_model(), Some("google/gemini-3.8-flash"));

        let named = Config::from_toml(
            "provider = \"local\"\ntitle_model = \"title-fast\"\n\n[providers.local]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"m\"\n",
        )
        .expect("a named title model loads");
        assert_eq!(named.title_model(), Some("title-fast"));

        let skipped = Config::from_toml(
            "provider = \"grok\"\ntitle_model = \"\"\n\n[providers.grok]\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"grok-4.6\"\n",
        )
        .expect("an empty title model loads");
        assert_eq!(skipped.title_model(), None);
    }

    #[test]
    fn a_local_provider_omits_request_effort() {
        let config = Config::from_toml(
            r#"
provider = "local"
effort = "high"

[providers.local]
base_url = "x"
model = "qwen"
"#,
        )
        .expect("the file parses");
        assert_eq!(config.effort.as_deref(), Some("high"));
        assert!(!config.takes_effort());
        assert_eq!(config.request_effort(), Some("high"));
    }

    #[test]
    fn a_provider_table_effort_means_it_takes_effort() {
        let config = Config::from_toml(
            r#"
provider = "office"
effort = "medium"

[providers.office]
base_url = "x"
model = "office-model"
effort = "medium"
"#,
        )
        .expect("the file parses");
        assert!(config.takes_effort());
        assert_eq!(config.request_effort(), Some("medium"));
        assert_eq!(config.providers["office"].effort.as_deref(), Some("medium"));
    }

    #[test]
    fn set_model_and_effort_write_the_file() {
        let path = temp_path("pick");
        fs::write(
            &path,
            r#"
provider = "office"

[providers.office]
base_url = "x"
model = "office-model"

[providers.local]
base_url = "y"
model = "local-model"
"#,
        )
        .expect("the file writes");

        let config = Config::set_model(&path, "office-2").expect("the model writes");
        assert_eq!(config.model, "office-2");
        let text = fs::read_to_string(&path).expect("the file reads");
        assert!(text.contains("model = \"office-2\""), "{text}");

        let config = Config::set_effort(&path, Some("high")).expect("the effort writes");
        assert_eq!(config.effort.as_deref(), Some("high"));
        let config =
            Config::set_provider_model(&path, "local", "local-2").expect("the switch writes");
        assert_eq!(config.provider.as_deref(), Some("local"));
        assert_eq!(config.model, "local-2");
        let config = Config::set_effort(&path, None).expect("the effort clears");
        assert_eq!(config.effort, None);
        let text = fs::read_to_string(&path).expect("the file reads");
        assert!(!text.contains("effort"), "{text}");
    }

    #[test]
    fn listen_is_optional_and_set_listen_writes_it() {
        let config = Config::from_toml(
            r#"
base_url = "http://127.0.0.1:1/v1"
model = "m"
listen = "0.0.0.0:7841"
listen_cert = "certs/custom.crt"
listen_key = "certs/custom.key"
"#,
        )
        .expect("the file parses");
        assert_eq!(config.listen.as_deref(), Some("0.0.0.0:7841"));
        assert_eq!(config.listen_cert.as_deref(), Some("certs/custom.crt"));
        assert_eq!(config.listen_key.as_deref(), Some("certs/custom.key"));

        let path = temp_path("listen");
        let config = Config::set_listen(&path, "127.0.0.1:0").expect("listen writes");
        assert_eq!(config.listen.as_deref(), Some("127.0.0.1:0"));
        let text = fs::read_to_string(&path).expect("the file reads");
        assert!(text.contains("listen = \"127.0.0.1:0\""), "{text}");
        assert!(text.contains("base_url"), "{text}");
    }

    #[test]
    fn projects_keep_file_order_and_name_falls_back_to_the_id() {
        let config = Config::from_toml(
            r#"
base_url = "x"
model = "m"

[projects.kyotoagent]
path = "/work/kyotoagent"

[projects.acpbot]
path = "/work/acpbot"
name = "Bot"
"#,
        )
        .expect("the file parses");
        assert_eq!(config.projects.len(), 2);
        assert_eq!(config.projects[0].id, "kyotoagent");
        assert_eq!(config.projects[0].path, PathBuf::from("/work/kyotoagent"));
        assert_eq!(config.projects[0].label(), "kyotoagent");
        assert_eq!(config.projects[1].id, "acpbot");
        assert_eq!(config.projects[1].path, PathBuf::from("/work/acpbot"));
        assert_eq!(config.projects[1].label(), "Bot");
    }

    #[test]
    fn a_project_id_uses_the_provider_rule_and_path_is_required() {
        let bad_id = Config::from_toml(
            r#"
base_url = "x"
model = "m"

[projects.KyotoAgent]
path = "/work/kyotoagent"
"#,
        )
        .expect_err("the id is refused");
        assert!(matches!(bad_id, ConfigError::InvalidId { ref id } if id == "KyotoAgent"));
        let missing = Config::from_toml(
            r#"
base_url = "x"
model = "m"

[projects.kyotoagent]
name = "Kyoto Agent"
"#,
        )
        .expect_err("path is required");
        assert!(missing.to_string().contains("path"), "{missing}");
        let empty = Config::from_toml(
            r#"
base_url = "x"
model = "m"

[projects.kyotoagent]
path = ""
"#,
        )
        .expect_err("an empty path is refused");
        assert!(matches!(empty, ConfigError::Missing { .. }), "{empty}");
    }

    #[test]
    fn a_workspace_under_a_project_path_uses_that_id() {
        let config = Config::from_toml(
            r#"
base_url = "http://127.0.0.1:1/v1"
model = "m"

[projects.kyotoagent]
path = "/work/kyotoagent"
name = "Kyoto Agent"

[projects.acpbot]
path = "/work/acpbot"
"#,
        )
        .expect("the file parses");
        assert_eq!(config.projects.len(), 2);
        assert_eq!(config.projects[0].id, "kyotoagent");
        assert_eq!(config.projects[0].name, "Kyoto Agent");
        assert_eq!(config.projects[1].name, "acpbot");
        assert_eq!(
            config.project_for("/work/kyotoagent", None).as_deref(),
            Some("kyotoagent")
        );
        assert_eq!(
            config.project_for("/work/kyotoagent/src", None).as_deref(),
            Some("kyotoagent")
        );
        assert_eq!(
            config
                .project_for("/work/kyotoagent-extra", None)
                .as_deref(),
            None
        );
        assert_eq!(
            config
                .project_for(
                    "/home/u/.kyotoagent/worktrees/kyotoagent-91bc",
                    Some("/work/kyotoagent")
                )
                .as_deref(),
            Some("kyotoagent")
        );
        assert_eq!(config.project_for("/tmp/notes", None).as_deref(), None);
    }

    #[test]
    fn a_missing_web_table_still_names_the_default_search_envs() {
        let config =
            Config::from_toml("base_url = \"https://openrouter.ai/api/v1\"\nmodel = \"m\"\n")
                .expect("toml");
        assert_eq!(config.exa_api_key_env, "EXA_API_KEY");
        assert_eq!(config.firecrawl_api_key_env, "FIRECRAWL_API_KEY");
    }

    #[test]
    fn web_search_names_env_vars_and_drops_a_secret_field() {
        let config = Config::from_toml(
            "base_url = \"https://openrouter.ai/api/v1\"\nmodel = \"m\"\n\n[web]\nexa_api_key_env = \"MY_EXA\"\nfirecrawl_api_key_env = \"  MY_FIRE  \"\nexa_api_key = \"supersecret\"\n",
        )
        .expect("toml");
        assert_eq!(config.exa_api_key_env, "MY_EXA");
        assert_eq!(config.firecrawl_api_key_env, "MY_FIRE");
        assert!(
            !format!("{config:?}").contains("supersecret"),
            "the secret stays out of the loaded config"
        );
        let blank = Config::from_toml(
            "base_url = \"https://openrouter.ai/api/v1\"\nmodel = \"m\"\n\n[web]\nexa_api_key_env = \"\"\n",
        )
        .expect("blank");
        assert_eq!(blank.exa_api_key_env, "EXA_API_KEY");
    }

    #[test]
    fn yolo_falls_through_until_a_project_sets_it() {
        let omitted = Config::from_toml(
            r#"
base_url = "x"
model = "m"
"#,
        )
        .expect("the file parses");
        assert!(!omitted.yolo);
        assert!(!omitted.yolo_for("/work/kyotoagent", None));

        let config = Config::from_toml(
            r#"
base_url = "x"
model = "m"
yolo = true

[projects.kyotoagent]
path = "/work/kyotoagent"

[projects.notes]
path = "/work/notes"
yolo = false
"#,
        )
        .expect("the file parses");
        assert!(config.yolo);
        assert_eq!(config.projects[0].yolo, None);
        assert_eq!(config.projects[1].yolo, Some(false));
        assert!(config.yolo_for("/work/kyotoagent", None));
        assert!(config.yolo_for("/tmp/other", None));
        assert!(!config.yolo_for("/work/notes", None));
        assert!(config.yolo_for(
            "/home/u/.kyotoagent/worktrees/kyotoagent-91bc",
            Some("/work/kyotoagent")
        ));

        let project_only = Config::from_toml(
            r#"
base_url = "x"
model = "m"

[projects.kyotoagent]
path = "/work/kyotoagent"
yolo = true
"#,
        )
        .expect("the file parses");
        assert!(!project_only.yolo);
        assert!(project_only.yolo_for("/work/kyotoagent", None));
        assert!(!project_only.yolo_for("/tmp/other", None));
    }

    #[test]
    fn enhance_falls_through_until_a_project_sets_it() {
        let omitted = Config::from_toml(
            r#"
base_url = "x"
model = "m"
"#,
        )
        .expect("the file parses");
        assert!(!omitted.enhance);
        assert!(!omitted.enhance_for("/work/kyotoagent", None));

        let config = Config::from_toml(
            r#"
base_url = "x"
model = "m"
enhance = true

[projects.kyotoagent]
path = "/work/kyotoagent"

[projects.notes]
path = "/work/notes"
enhance = false
"#,
        )
        .expect("the file parses");
        assert!(config.enhance);
        assert_eq!(config.projects[0].enhance, None);
        assert_eq!(config.projects[1].enhance, Some(false));
        assert!(config.enhance_for("/work/kyotoagent", None));
        assert!(config.enhance_for("/tmp/other", None));
        assert!(!config.enhance_for("/work/notes", None));
        assert!(config.enhance_for(
            "/home/u/.kyotoagent/worktrees/kyotoagent-91bc",
            Some("/work/kyotoagent")
        ));

        let project_only = Config::from_toml(
            r#"
base_url = "x"
model = "m"

[projects.kyotoagent]
path = "/work/kyotoagent"
enhance = true
"#,
        )
        .expect("the file parses");
        assert!(!project_only.enhance);
        assert!(project_only.enhance_for("/work/kyotoagent", None));
        assert!(!project_only.enhance_for("/tmp/other", None));
    }

    #[test]
    fn show_closeout_is_on_until_a_key_turns_it_off() {
        let omitted = Config::from_toml(
            r#"
base_url = "x"
model = "m"
"#,
        )
        .expect("the file parses");
        assert!(omitted.show_closeout);
        assert!(omitted.show_closeout_for("/work/kyotoagent", None));

        let config = Config::from_toml(
            r#"
base_url = "x"
model = "m"
show_closeout = false

[projects.kyotoagent]
path = "/work/kyotoagent"

[projects.notes]
path = "/work/notes"
show_closeout = true
"#,
        )
        .expect("the file parses");
        assert!(!config.show_closeout);
        assert_eq!(config.projects[0].show_closeout, None);
        assert_eq!(config.projects[1].show_closeout, Some(true));
        assert!(!config.show_closeout_for("/work/kyotoagent", None));
        assert!(!config.show_closeout_for("/tmp/other", None));
        assert!(config.show_closeout_for("/work/notes", None));
        assert!(!config.show_closeout_for(
            "/home/u/.kyotoagent/worktrees/kyotoagent-91bc",
            Some("/work/kyotoagent")
        ));

        let project_only = Config::from_toml(
            r#"
base_url = "x"
model = "m"

[projects.kyotoagent]
path = "/work/kyotoagent"
show_closeout = false
"#,
        )
        .expect("the file parses");
        assert!(project_only.show_closeout);
        assert!(!project_only.show_closeout_for("/work/kyotoagent", None));
        assert!(project_only.show_closeout_for("/tmp/other", None));
    }

    #[test]
    fn an_unknown_tool_name_fails_and_names_the_profile() {
        let error = Config::from_toml(
            r#"
base_url = "x"
model = "m"

[profiles.planning]
tools = ["read_file", "explode"]
"#,
        )
        .expect_err("an unknown tool fails load");
        let text = error.to_string();
        assert!(text.contains("planning"), "{text}");
        assert!(text.contains("explode"), "{text}");
    }

    #[test]
    fn an_unknown_skill_name_loads() {
        let config = Config::from_toml(
            r#"
base_url = "x"
model = "m"

[profiles.planning]
tools = ["read_file", "use_skill"]
skills = ["not-a-real-skill"]
"#,
        )
        .expect("an unknown skill still loads");
        let profile = config.profile("planning").expect("planning");
        assert_eq!(
            profile.skills.as_deref(),
            Some(["not-a-real-skill".to_string()].as_slice())
        );
    }

    #[test]
    fn a_missing_tools_key_still_allows_every_tool() {
        let config = Config::from_toml(
            r#"
base_url = "x"
model = "m"
profile = "open"

[profiles.open]
skills = ["skill-a"]
"#,
        )
        .expect("the file parses");
        assert!(config.profile("open").expect("open").tools.is_none());
        assert!(config.tool_allowed(Some("open"), "run"));
        assert!(config.tool_allowed(Some("open"), "write_file"));
        assert_eq!(
            config.skill_allowance(Some("open")).as_deref(),
            Some(["skill-a".to_string()].as_slice())
        );
        assert!(config.tool_allowed(None, "run"));
        assert!(config.skill_allowance(None).is_none());
    }

    #[test]
    fn a_project_profile_wins_over_the_top_level_profile() {
        let config = Config::from_toml(
            r#"
base_url = "x"
model = "m"
profile = "planning"

[projects.kyotoagent]
path = "/work/kyotoagent"
profile = "review"

[projects.notes]
path = "/work/notes"

[profiles.planning]
tools = ["read_file", "todo", "use_skill"]
skills = ["skill-a", "skill-b"]

[profiles.review]
tools = ["read_file", "grep", "list_dir", "web_fetch", "ask", "finish"]
"#,
        )
        .expect("the file parses");
        assert_eq!(
            config.profile_for("/work/kyotoagent", None).as_deref(),
            Some("review")
        );
        assert_eq!(
            config.profile_for("/tmp/other", None).as_deref(),
            Some("planning")
        );
        assert_eq!(
            config.profile_for("/work/notes", None).as_deref(),
            Some("planning")
        );
        assert_eq!(
            config
                .profile_for(
                    "/home/u/.kyotoagent/worktrees/kyotoagent-91bc",
                    Some("/work/kyotoagent")
                )
                .as_deref(),
            Some("review")
        );
        let names: Vec<&str> = config
            .profiles
            .iter()
            .map(|profile| profile.id.as_str())
            .collect();
        assert_eq!(names, vec!["planning", "review"]);
        assert!(config.profile("review").expect("review").skills.is_none());
        assert_eq!(
            config.profile("review").expect("review").tools.as_deref(),
            Some(
                [
                    "read_file".to_string(),
                    "grep".to_string(),
                    "list_dir".to_string(),
                    "web_fetch".to_string(),
                    "ask".to_string(),
                    "finish".to_string(),
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn an_empty_skills_list_allows_no_skills() {
        let config = Config::from_toml(
            r#"
base_url = "x"
model = "m"

[profiles.quiet]
skills = []
"#,
        )
        .expect("the file parses");
        assert_eq!(
            config.skill_allowance(Some("quiet")).as_deref(),
            Some([].as_slice())
        );
        assert!(config.tool_allowed(Some("quiet"), "run"));
    }
    #[test]
    fn provider_title_defaults_replace_legacy_login_values_and_preserve_overrides() {
        for (id, kind, expected) in [
            ("grok", "", "grok-4.6"),
            ("codex", "codex", "gpt-6-luna"),
            ("office-codex", "codex", "gpt-6-luna"),
            ("opencode", "opencode", "deepseek-v4.1-flash"),
            ("office-zen", "opencode", "deepseek-v4.1-flash"),
            ("openrouter", "", DEFAULT_TITLE_MODEL),
            ("local", "", DEFAULT_TITLE_MODEL),
        ] {
            for (title, wanted) in [
                (None, Some(expected)),
                (Some(DEFAULT_TITLE_MODEL), Some(expected)),
                (Some("custom-title"), Some("custom-title")),
                (Some(""), None),
            ] {
                let title = title
                    .map(|value| format!("title_model = {value:?}\n"))
                    .unwrap_or_default();
                let config = Config::from_toml(&format!(
                    "provider = {id:?}\neffort = \"high\"\n{title}\n[providers.{id}]\nkind = {kind:?}\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"foreground\"\n"
                )).unwrap();
                assert_eq!(config.title_model(), wanted, "provider {id}, title {title}");
                assert_eq!(config.title_effort(), (id == "grok").then_some("low"));
                assert_eq!(config.request_effort(), Some("high"));
                assert_eq!(config.model, "foreground");
            }
        }
    }
    #[test]
    fn project_closeout_falls_back_only_when_repository_files_are_absent() {
        let root =
            std::env::temp_dir().join(format!("kyotoagent-config-closeout-{}", std::process::id()));
        fs::create_dir_all(root.join(".agents")).unwrap();
        fs::create_dir_all(root.join(".kyotoagent")).unwrap();
        let config = Config::from_toml(&format!(
            r#"
base_url = "x"
model = "m"
[projects.repo]
path = {root:?}
closeout = "fallback.yaml"
"#,
            root = root.to_string_lossy()
        ))
        .unwrap();
        fs::write(root.join("fallback.yaml"), "version: 1\nretry:\n  maxFailedAttemptsPerItem: 2\nitems:\n  - id: fallback\n    kind: command\n    run: true\n    hint: Run the fallback check\n    paths: ['src/**']\n").unwrap();
        let file = config.closeout_for(&root, None).unwrap().unwrap();
        assert_eq!(file.items[0].id, "fallback");
        assert_eq!(file.items[0].paths, ["src/**"]);
        assert_eq!(file.max_failures, 2);
        assert!(config
            .closeout_for(Path::new("/unrelated"), None)
            .unwrap()
            .is_none());
        let linked = config
            .closeout_for(
                Path::new("/outside/worktree"),
                Some(&root.to_string_lossy()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(linked, file);
        fs::write(
            root.join(".agents/closeout.yaml"),
            "version: 1\nitems: []\n",
        )
        .unwrap();
        assert!(config
            .closeout_for(&root, None)
            .unwrap()
            .unwrap()
            .items
            .is_empty());
        fs::write(
            root.join(".kyotoagent/closeout.yaml"),
            "version: 1\nitems:\n  - id: repo\n    kind: command\n    run: true\n    hint: repo\n",
        )
        .unwrap();
        assert_eq!(
            config.closeout_for(&root, None).unwrap().unwrap().items[0].id,
            "repo"
        );
        fs::write(
            root.join(".kyotoagent/closeout.yaml"),
            "version: 2\nitems: []\n",
        )
        .unwrap();
        assert!(config.closeout_for(&root, None).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_closeout_imports_resolve_inside_the_selected_workspace() {
        let root =
            std::env::temp_dir().join(format!("kyotoagent-config-imports-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("checks.yaml"), "specVersion: '0.1'\nitems:\n  - id: test\n    kind: command\n    gate: beforePR\n    exec: [echo, checked]\n    timeoutSeconds: 5\n").unwrap();
        fs::write(
            root.join("fallback.yaml"),
            "specVersion: '0.1'\nimports:\n  - path: checks.yaml\n    as: quality\n",
        )
        .unwrap();
        let config = Config::from_toml(&format!(
            "base_url = 'x'\nmodel = 'm'\n[projects.repo]\npath = {:?}\ncloseout = 'fallback.yaml'\n",
            root.to_string_lossy()
        ))
        .unwrap();
        let file = config.closeout_for(&root, None).unwrap().unwrap();
        assert_eq!(file.items[0].id, "quality/test");
        assert_eq!(file.executions["quality/test"].argv, ["echo", "checked"]);
        std::fs::remove_file(root.join("checks.yaml")).unwrap();
        assert!(config.closeout_for(&root, None).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn external_project_policy_imports_and_skills_resolve_beside_the_yaml() {
        let root =
            std::env::temp_dir().join(format!("kyotoagent-external-policy-{}", std::process::id()));
        let project = root.join("project");
        let shared = root.join("shared");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(shared.join("skills/review")).unwrap();
        fs::write(shared.join("skills/review/SKILL.md"), "Review changes").unwrap();
        fs::write(shared.join("review.yaml"), "specVersion: '0.1'\nitems:\n  - id: review\n    kind: review\n    gate: beforePR\n    skill: skills/review/SKILL.md\n    independence:\n      differentSession: true\n      differentModel: true\n    failOn: P1\n").unwrap();
        fs::write(
            shared.join("closeout.yaml"),
            "specVersion: '0.1'\nimports:\n  - path: review.yaml\n    as: quality\n",
        )
        .unwrap();
        let config = Config::from_toml(&format!(
            "base_url = 'x'\nmodel = 'm'\n[projects.repo]\npath = {:?}\ncloseout = {:?}\n",
            project.to_string_lossy(),
            shared.join("closeout.yaml").to_string_lossy()
        ))
        .unwrap();
        let file = config.closeout_for(&project, None).unwrap().unwrap();
        assert_eq!(file.items[0].id, "quality/review");
        assert_eq!(
            Path::new(&file.reviews["quality/review"].skill),
            shared.join("skills/review/SKILL.md")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_closeout_requires_a_yaml_path() {
        let template = "base_url = 'x'\nmodel = 'm'\n[projects.repo]\npath = '/repo'\ncloseout = '/shared/closeout.yaml'\n";
        assert!(Config::from_toml(template).is_ok());
        assert!(Config::from_toml(&template.replace("'/shared/closeout.yaml'", "''")).is_err());
        assert!(Config::from_toml(
            "base_url = 'x'\nmodel = 'm'\n[projects.repo]\npath = '/repo'\n[projects.repo.closeout]\nspecVersion = '0.1'\n"
        )
        .is_err());
        let config = Config::from_toml(template).unwrap();
        assert!(config.closeout_for(Path::new("/repo"), None).is_err());
    }
}
