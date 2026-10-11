//! The one request the agent makes: a non-streaming chat completion.
//!
//! [`ChatClient::complete`] posts to `{base_url}/chat/completions` and waits
//! for the whole message. The body carries `model`, `stream: false`,
//! `tool_choice: "auto"`, `messages`, and `tools`, and the reply is read out of
//! `choices[0].message`:
//!
//! ```json
//! { "choices": [ { "message": { "role": "assistant", "content": null,
//!   "tool_calls": [ { "id": "call_1", "type": "function",
//!     "function": { "name": "read_file", "arguments": "{\"path\":\"README.md\"}" } } ] } } ] }
//! ```
//!
//! Two headers matter. `Authorization: Bearer` goes out only when the
//! configuration named an environment variable and that variable held
//! something, so OpenRouter is authenticated and a local server is not asked
//! for a key. Everything else the server sends is provider-specific and is
//! ignored: the model id, the token counts, and the reason fields all vary
//! between OpenRouter, Ollama, and llama.cpp, and none of them is the answer.
//!
//! Nothing here writes to a session. A turn loop owns the transcript; this
//! module takes messages in and hands back a [`Reply`], and every failure is
//! an [`ChatError`] the caller can turn into a one-sentence result.

use std::fmt;
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::{self, AuthClient, AuthError, CodexAuth};
use crate::config::Config;

mod catalog;
mod retry;
pub use retry::RetryStatus;

pub use catalog::*;

#[cfg(test)]
pub(crate) mod tests;

/// How long one completion gets before the request is abandoned.
pub const REQUEST_TIMEOUT_SECS: u64 = 120;

pub const TITLE_TIMEOUT_SECS: u64 = 15;

pub const TITLE_SYSTEM: &str =
    "Write a 3 to 8 word title for this coding session. Reply with the title only.";

/// One message in the transcript, as the server spells it. The role is the
/// tag, so a message is `{"role": "assistant", "tool_calls": [...]}` on the
/// wire and not a wrapper around it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    /// Instructions that hold for the whole turn.
    System { content: String },
    /// What the user asked.
    User { content: UserContent },
    /// What the model said. `tool_calls` is what the loop runs, and `content`
    /// is whatever prose came with it, which is often nothing.
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    /// What a tool returned, tied to the call that asked for it. The id is
    /// echoed exactly as the model spelled it, in the `tool_call_id` the
    /// completion API uses.
    Tool {
        #[serde(rename = "tool_call_id", alias = "toolCallId")]
        tool_call_id: String,
        content: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Parts(Vec<UserPart>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

impl From<String> for UserContent {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<&str> for UserContent {
    fn from(text: &str) -> Self {
        Self::Text(text.to_string())
    }
}

impl std::ops::Deref for UserContent {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for UserContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl UserContent {
    pub fn with_images(text: String, images: &[crate::attachment::ImageAttachment]) -> Self {
        if images.is_empty() {
            return Self::Text(text);
        }
        let mut parts = vec![UserPart::Text { text }];
        parts.extend(images.iter().map(|image| UserPart::ImageUrl {
            image_url: ImageUrl {
                url: image.data_url(),
            },
        }));
        Self::Parts(parts)
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Parts(parts) => parts
                .iter()
                .find_map(|part| match part {
                    UserPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .unwrap_or(""),
        }
    }

    pub fn estimated_bytes(&self) -> u64 {
        let images = match self {
            Self::Text(_) => 0,
            Self::Parts(parts) => parts
                .iter()
                .filter(|part| matches!(part, UserPart::ImageUrl { .. }))
                .count(),
        };
        self.as_str().len() as u64 + images as u64 * 65536
    }

    fn responses_parts(&self) -> Vec<Value> {
        match self {
            Self::Text(text) => vec![serde_json::json!({ "type": "input_text", "text": text })],
            Self::Parts(parts) => parts
                .iter()
                .map(|part| match part {
                    UserPart::Text { text } => {
                        serde_json::json!({ "type": "input_text", "text": text })
                    }
                    UserPart::ImageUrl { image_url } => {
                        serde_json::json!({ "type": "input_image", "image_url": image_url.url })
                    }
                })
                .collect(),
        }
    }
}

impl Message {
    pub fn tool_images(images: &[crate::attachment::ImageAttachment]) -> Option<Message> {
        if images.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        for image in images {
            parts.push(UserPart::Text {
                text: format!("Image read by a tool: {}", image.name),
            });
            parts.push(UserPart::ImageUrl {
                image_url: ImageUrl {
                    url: image.data_url(),
                },
            });
        }
        Some(Message::User {
            content: UserContent::Parts(parts),
        })
    }

    /// The assistant message the model just produced, ready to be appended to
    /// the transcript before its tool calls run. Echoing the message back keeps
    /// the call ids paired with the answers.
    pub fn assistant_reply(reply: &Reply) -> Message {
        Message::Assistant {
            content: reply.content.clone(),
            tool_calls: reply.tool_calls.clone(),
        }
    }

    /// The result of one tool call. `tool_call_id` is the id the model gave, so
    /// the server knows which call this answers.
    pub fn tool_result(tool_call_id: &str, content: &str) -> Message {
        Message::Tool {
            tool_call_id: tool_call_id.to_string(),
            content: crate::compact::cap_dump(content),
        }
    }
}

/// A tool the model may call, in the function shape the server expects:
/// `{"type": "function", "function": {"name", "description", "parameters"}}`.
/// `parameters` is a JSON Schema object and is passed through untouched.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl Tool {
    /// A tool with a JSON Schema for its arguments.
    pub fn new(name: &str, description: &str, parameters: Value) -> Tool {
        Tool {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        }
    }
}

impl Serialize for Tool {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Function<'a> {
            name: &'a str,
            description: &'a str,
            parameters: &'a Value,
        }
        #[derive(Serialize)]
        struct Wrapper<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            function: Function<'a>,
        }
        Wrapper {
            kind: "function",
            function: Function {
                name: &self.name,
                description: &self.description,
                parameters: &self.parameters,
            },
        }
        .serialize(serializer)
    }
}

/// One call the model wants made. The fields are flat here and nested on the
/// wire, because `id`, `name`, and `arguments` are what the loop needs and the
/// nesting is the server's business.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolCall {
    /// The server's id for this call. The result message has to echo it.
    pub id: String,
    /// Always `function`. Kept as a string so a server that adds a kind later
    /// does not make this crate fail to parse a reply.
    pub kind: String,
    pub name: String,
    /// The arguments, as the JSON string the server sends them in. Parsing them
    /// is the loop's business, not this module's.
    pub arguments: String,
}

fn function_kind() -> String {
    "function".to_string()
}

#[derive(Serialize, Deserialize)]
struct WireToolCall {
    id: String,
    #[serde(rename = "type", default = "function_kind")]
    kind: String,
    function: WireFunction,
}

#[derive(Serialize, Deserialize)]
struct WireFunction {
    name: String,
    #[serde(default)]
    arguments: String,
}

impl Serialize for ToolCall {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WireToolCall {
            id: self.id.clone(),
            kind: self.kind.clone(),
            function: WireFunction {
                name: self.name.clone(),
                arguments: self.arguments.clone(),
            },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ToolCall {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<ToolCall, D::Error> {
        let wire = WireToolCall::deserialize(deserializer)?;
        Ok(ToolCall {
            id: wire.id,
            kind: wire.kind,
            name: wire.function.name,
            arguments: wire.function.arguments,
        })
    }
}

/// What the model said in reply to one completion.
///
/// `content` is the prose, which is the whole answer when the model finished
/// and often nothing when it called a tool. `tool_calls` is the work.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip)]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip)]
    pub completion_tokens: Option<u64>,
}

fn validate_tool_calls(calls: &[ToolCall]) -> Result<(), ChatError> {
    let mut seen = std::collections::HashSet::new();
    for call in calls {
        if call.id.trim().is_empty() || !seen.insert(call.id.clone()) {
            return Err(ChatError::Status {
                status: 502,
                body: "the model repeated a tool call id".to_string(),
            });
        }
    }
    Ok(())
}

impl Reply {
    /// The prose, or an empty string when the model only called a tool.
    pub fn text(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }

    /// Whether the model is waiting on tools rather than finishing.
    pub fn wants_tools(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    /// Never a stream: the whole message is waited for.
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Tool]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

#[derive(Serialize)]
struct CompactRequest<'a> {
    model: &'a str,
    stream: bool,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

#[derive(Serialize)]
struct QuietRequest<'a> {
    model: &'a str,
    stream: bool,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

#[derive(Deserialize)]
struct WireResponse {
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: Reply,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// A completion request that did not come back with a message. Each variant
/// reads as one sentence, because that sentence is what the turn puts in its
/// result card.
#[derive(Debug)]
pub enum ChatError {
    /// The server could not be reached, or the request timed out.
    Transport(reqwest::Error),
    /// The server answered with something other than a 2xx.
    Status {
        status: u16,
        body: String,
    },
    /// The answer was not a completion this crate can read.
    Decode(serde_json::Error),
    /// The answer was a completion with no choice in it.
    NoChoice,
    NeedLogin,
    NeedKey,
    Idle {
        seconds: u64,
    },
}

impl ChatError {
    pub fn is_context_overflow(&self) -> bool {
        matches!(self, Self::Status { status: 400 | 413, body } if ["input_too_large", "context_length_exceeded", "context_window_exceeded"].iter().any(|code| body.contains(code)))
    }
}

impl fmt::Display for ChatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChatError::Transport(source) => {
                write!(f, "the model server could not be reached: {source}")
            }
            ChatError::Status { status, body } => {
                let detail = body.trim();
                if detail.is_empty() {
                    write!(f, "the model server answered {status}")
                } else {
                    write!(f, "the model server answered {status}: {detail}")
                }
            }
            ChatError::Decode(source) => {
                write!(f, "the model server's answer could not be read: {source}")
            }
            ChatError::NoChoice => write!(f, "the model server sent no choices"),
            ChatError::NeedLogin => write!(
                f,
                "Open Providers in Kyoto Agent to sign in or enter an API key."
            ),
            ChatError::NeedKey => write!(f, "OpenCode API key is missing"),
            ChatError::Idle { seconds } if *seconds == 0 => {
                write!(f, "the model stream timed out")
            }
            ChatError::Idle { seconds } => {
                write!(f, "the model stream timed out after {seconds} seconds")
            }
        }
    }
}

impl std::error::Error for ChatError {}

/// A chat client for one server and one model.
///
/// The API key is read from the environment when the client is made, so a
/// process that exports the variable later still needs a new client, and the
/// key itself is never held anywhere but the client's copy of it.
#[derive(Clone, Debug)]
pub struct ChatClient {
    http: reqwest::Client,
    chat_url: String,
    models_url: String,
    model: String,
    reasoning_effort: Option<String>,
    title_reasoning_effort: Option<String>,
    api_key: Option<String>,
    base_url: String,
    grok: Option<GrokSession>,
    codex: Option<CodexSession>,
    opencode: Option<OpenCodeSession>,
    stream_idle: Duration,
    cooldown: Arc<Mutex<Option<tokio::time::Instant>>>,
}

#[derive(Clone, Debug)]
struct GrokSession {
    path: PathBuf,
    client: AuthClient,
}

#[derive(Clone, Debug)]
struct CodexSession {
    path: PathBuf,
    client: CodexAuth,
}

#[derive(Clone, Debug)]
struct OpenCodeSession {
    path: Option<PathBuf>,
    env_name: Option<String>,
}

impl ChatClient {
    /// A client for the server and model in `config`, with the key read from
    /// the environment it names. A server with no key configured is not an
    /// error: the client simply sends no `Authorization` header.
    pub fn new(config: &Config) -> Result<ChatClient, ChatError> {
        ChatClient::in_root(config, None)
    }

    pub fn in_root(config: &Config, root: Option<&Path>) -> Result<ChatClient, ChatError> {
        if config.is_codex() {
            let codex = codex_session(config, root, None)?;
            return ChatClient::build(config, None, codex, None, root);
        }
        let opencode = opencode_session(config, root)?;
        let grok = if opencode.is_some() {
            None
        } else {
            grok_session(config, root, None)?
        };
        ChatClient::build(config, grok, None, opencode, root)
    }

    pub fn in_root_with_auth(
        config: &Config,
        root: Option<&Path>,
        auth: AuthClient,
    ) -> Result<ChatClient, ChatError> {
        let opencode = opencode_session(config, root)?;
        let grok = if opencode.is_some() {
            None
        } else {
            grok_session(config, root, Some(auth))?
        };
        ChatClient::build(config, grok, None, opencode, root)
    }

    pub fn in_root_with_codex(
        config: &Config,
        root: Option<&Path>,
        auth: CodexAuth,
    ) -> Result<ChatClient, ChatError> {
        let codex = codex_session(config, root, Some(auth))?;
        ChatClient::build(config, None, codex, None, root)
    }

    fn build(
        config: &Config,
        grok: Option<GrokSession>,
        codex: Option<CodexSession>,
        opencode: Option<OpenCodeSession>,
        root: Option<&Path>,
    ) -> Result<ChatClient, ChatError> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(ChatError::Transport)?;
        let oauth = grok.is_some() || codex.is_some() || opencode.is_some();
        let api_key = if oauth {
            None
        } else {
            auth::stored_provider_key(config, root).map_err(|_| ChatError::NeedKey)?
        };
        let account = codex
            .as_ref()
            .map(|session| format!("oauth:{}", session.path.display()))
            .or_else(|| {
                grok.as_ref()
                    .map(|session| format!("oauth:{}", session.path.display()))
            })
            .or_else(|| {
                opencode.as_ref().map(|session| match &session.env_name {
                    Some(name) => format!("opencode-env:{name}"),
                    None => format!(
                        "opencode:{}",
                        session
                            .path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_default()
                    ),
                })
            })
            .unwrap_or_else(|| format!("key:{}", api_key.clone().unwrap_or_default()));
        let cooldown = retry::shared(&config.chat_url(), account);
        Ok(ChatClient {
            http,
            chat_url: config.chat_url(),
            models_url: config.models_url(),
            model: config.model.clone(),
            reasoning_effort: config.request_effort().map(str::to_string),
            title_reasoning_effort: config.title_effort().map(str::to_string),
            api_key: if oauth { None } else { api_key },
            base_url: config.base_url.clone(),
            grok,
            codex,
            opencode,
            stream_idle: Duration::from_secs(REQUEST_TIMEOUT_SECS),
            cooldown,
        })
    }

    /// The URL this client posts to, `{base_url}/chat/completions`.
    pub fn url(&self) -> &str {
        &self.chat_url
    }

    /// The model this client asks for.
    pub fn model(&self) -> &str {
        &self.model
    }

    pub async fn catalog(&self) -> Result<Vec<ModelRow>, ChatError> {
        let token = self.bearer().await?;
        let openrouter = reqwest::Url::parse(&self.base_url).ok().is_some_and(|url| {
            matches!(url.host_str(), Some("openrouter.ai" | "www.openrouter.ai"))
        });
        if openrouter && token.as_deref().is_none_or(|token| token.trim().is_empty()) {
            return Err(ChatError::NeedLogin);
        }
        let (status, text) = self.get_models(token.as_deref()).await?;
        let (status, text) = if status == 401 {
            if let Some(refreshed) = self.refresh_on_401(token.as_deref()).await? {
                self.get_models(Some(refreshed.as_str())).await?
            } else {
                (status, text)
            }
        } else {
            (status, text)
        };
        if !(200..300).contains(&status) {
            return Err(ChatError::Status { status, body: text });
        }
        let mut rows = if self.codex.is_some() {
            catalog::parse_codex_catalog(&text)
        } else {
            parse_catalog(&text)
        }
        .ok_or(ChatError::NoChoice)?;
        if self.opencode.is_some() {
            rows.retain(|row| crate::config::opencode_picker_keeps(&row.id));
        }
        if rows.is_empty() {
            return Err(ChatError::NoChoice);
        }
        Ok(rows)
    }

    async fn get_models(&self, token: Option<&str>) -> Result<(u16, String), ChatError> {
        let mut request = self
            .http
            .get(&self.models_url)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS));
        if self.codex.is_some() {
            request = request.query(&[("client_version", auth::CODEX_CLIENT_VERSION)]);
        }
        request = self.with_auth(request, token);
        let response = request.send().await.map_err(ChatError::Transport)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(ChatError::Transport)?;
        Ok((status, text))
    }

    /// One completion. The request asks for a stream. Chunks append
    /// `reasoning_content` (or `reasoning`) onto `thoughts` as they arrive, and
    /// the returned [`Reply`] is the assembled prose and tool calls. A server
    /// that answers with one JSON object is read the same way as a non-stream
    /// body. This call has no timeout: a reasoning stream can run longer than
    /// [`REQUEST_TIMEOUT_SECS`].
    pub async fn complete(&self, messages: &[Message], tools: &[Tool]) -> Result<Reply, ChatError> {
        self.complete_showing(messages, tools, None).await
    }

    pub async fn complete_showing(
        &self,
        messages: &[Message],
        tools: &[Tool],
        thoughts: Option<&Arc<Mutex<String>>>,
    ) -> Result<Reply, ChatError> {
        self.complete_with_status(messages, tools, thoughts, None)
            .await
    }

    pub async fn complete_with_status(
        &self,
        messages: &[Message],
        tools: &[Tool],
        thoughts: Option<&Arc<Mutex<String>>>,
        status: Option<&RetryStatus>,
    ) -> Result<Reply, ChatError> {
        let guard = retry::StatusGuard(status);
        let thought_length = thoughts.map(|sink| sink.lock().unwrap().len());
        for attempt in 0..=retry::MAX_RETRIES {
            let result = self
                .complete_attempt(messages, tools, thoughts, status)
                .await;
            let transient = match &result {
                Err(ChatError::Status { status, body }) => retry::transient(*status, body),
                _ => false,
            };
            if !transient || attempt == retry::MAX_RETRIES {
                return result;
            }
            if let (Some(sink), Some(length)) = (thoughts, thought_length) {
                sink.lock().unwrap().truncate(length);
            }
            let delay = Duration::from_secs(1 << attempt);
            guard.set(delay);
            tokio::time::sleep(delay).await;
            guard.clear();
        }
        unreachable!()
    }

    async fn complete_attempt(
        &self,
        messages: &[Message],
        tools: &[Tool],
        thoughts: Option<&Arc<Mutex<String>>>,
        status: Option<&RetryStatus>,
    ) -> Result<Reply, ChatError> {
        let url = self.request_url(&self.model);
        if self.posts_responses(&self.model) {
            let body = responses_body(
                &self.model,
                self.reasoning_effort.as_deref(),
                messages,
                tools,
                self.codex.is_some(),
            );
            let token = self.bearer().await?;
            let response = self
                .send_with_retry(&url, &body, token.as_deref(), None, status, true)
                .await?;
            let response = if response.status().as_u16() == 401 {
                let text = response.text().await.map_err(ChatError::Transport)?;
                if let Some(refreshed) = self.refresh_on_401(token.as_deref()).await? {
                    self.send_with_retry(&url, &body, Some(refreshed.as_str()), None, status, true)
                        .await?
                } else {
                    return Err(ChatError::Status {
                        status: 401,
                        body: text,
                    });
                }
            } else {
                response
            };
            return read_completion(response, thoughts, self.stream_idle, true).await;
        }
        let (tool_choice, tools) = if tools.is_empty() {
            (None, None)
        } else {
            (Some("auto"), Some(tools))
        };
        let body = Request {
            model: &self.model,
            stream: true,
            tool_choice,
            messages,
            tools,
            reasoning_effort: self.reasoning_effort.as_deref(),
        };
        let token = self.bearer().await?;
        let response = self
            .send_with_retry(&url, &body, token.as_deref(), None, status, true)
            .await?;
        let response = if response.status().as_u16() == 401 {
            let text = response.text().await.map_err(ChatError::Transport)?;
            if let Some(refreshed) = self.refresh_on_401(token.as_deref()).await? {
                self.send_with_retry(&url, &body, Some(refreshed.as_str()), None, status, true)
                    .await?
            } else {
                return Err(ChatError::Status {
                    status: 401,
                    body: text,
                });
            }
        } else {
            response
        };
        read_completion(response, thoughts, self.stream_idle, false).await
    }

    pub async fn title(&self, model: &str, ask: &str) -> Result<Reply, ChatError> {
        let messages = [
            Message::System {
                content: TITLE_SYSTEM.to_string(),
            },
            Message::User {
                content: ask.to_string().into(),
            },
        ];
        let url = self.request_url(model);
        if self.posts_responses(model) {
            let body = responses_body(
                model,
                self.title_reasoning_effort.as_deref(),
                &messages,
                &[],
                self.codex.is_some(),
            );
            return self.finish_responses(&url, &body, TITLE_TIMEOUT_SECS).await;
        }
        let body = QuietRequest {
            model,
            stream: false,
            messages: &messages,
            reasoning_effort: self.title_reasoning_effort.as_deref(),
        };
        let token = self.bearer().await?;
        let (status, text) = self
            .send_json(&url, &body, token.as_deref(), TITLE_TIMEOUT_SECS)
            .await?;
        let (status, text) = if status == 401 {
            if let Some(refreshed) = self.refresh_on_401(token.as_deref()).await? {
                self.send_json(&url, &body, Some(refreshed.as_str()), TITLE_TIMEOUT_SECS)
                    .await?
            } else {
                (status, text)
            }
        } else {
            (status, text)
        };
        if !(200..300).contains(&status) {
            return Err(ChatError::Status { status, body: text });
        }
        decode_message(&text)
    }

    pub async fn rewrite(&self, model: &str, messages: &[Message]) -> Result<Reply, ChatError> {
        let url = self.request_url(model);
        if self.posts_responses(model) {
            let body = responses_body(model, None, messages, &[], self.codex.is_some());
            return self
                .finish_responses(&url, &body, REQUEST_TIMEOUT_SECS)
                .await;
        }
        let body = QuietRequest {
            model,
            stream: false,
            messages,
            reasoning_effort: None,
        };
        let token = self.bearer().await?;
        let (status, text) = self
            .send_json(&url, &body, token.as_deref(), REQUEST_TIMEOUT_SECS)
            .await?;
        let (status, text) = if status == 401 {
            if let Some(refreshed) = self.refresh_on_401(token.as_deref()).await? {
                self.send_json(&url, &body, Some(refreshed.as_str()), REQUEST_TIMEOUT_SECS)
                    .await?
            } else {
                (status, text)
            }
        } else {
            (status, text)
        };
        if !(200..300).contains(&status) {
            return Err(ChatError::Status { status, body: text });
        }
        decode_message(&text)
    }

    pub async fn compact(&self, messages: &[Message]) -> Result<Reply, ChatError> {
        let url = self.request_url(&self.model);
        if self.posts_responses(&self.model) {
            let body = responses_body(
                &self.model,
                self.reasoning_effort.as_deref(),
                messages,
                &[],
                self.codex.is_some(),
            );
            return self
                .finish_responses(&url, &body, REQUEST_TIMEOUT_SECS)
                .await;
        }
        let body = CompactRequest {
            model: &self.model,
            stream: false,
            messages,
            reasoning_effort: self.reasoning_effort.as_deref(),
        };
        let token = self.bearer().await?;
        let (status, text) = self
            .send_json(&url, &body, token.as_deref(), REQUEST_TIMEOUT_SECS)
            .await?;
        let (status, text) = if status == 401 {
            if let Some(refreshed) = self.refresh_on_401(token.as_deref()).await? {
                self.send_json(&url, &body, Some(refreshed.as_str()), REQUEST_TIMEOUT_SECS)
                    .await?
            } else {
                (status, text)
            }
        } else {
            (status, text)
        };
        if !(200..300).contains(&status) {
            return Err(ChatError::Status { status, body: text });
        }
        decode_message(&text)
    }

    pub async fn model_length(&self, model: &str) -> Option<u64> {
        if let Ok(rows) = self.catalog().await {
            if let Some(row) = rows.iter().find(|row| row.matches(model)) {
                if let Some(length) = row.context_length {
                    return Some(length);
                }
            }
        }
        let token = self.bearer().await.ok()?;
        let url = format!("{}/{model}", self.models_url.trim_end_matches('/'));
        let mut request = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS));
        request = self.with_auth(request, token.as_deref());
        let response = request.send().await.ok()?;
        let status = response.status().as_u16();
        let text = response.text().await.ok()?;
        if !(200..300).contains(&status) {
            return None;
        }
        parse_model(&text).and_then(|row| row.context_length)
    }

    fn posts_responses(&self, model: &str) -> bool {
        self.codex.is_some()
            || (self.opencode.is_some() && crate::config::opencode_responses_model(model))
    }

    fn request_url(&self, model: &str) -> String {
        if self.opencode.is_none() {
            return self.chat_url.clone();
        }
        let base = self.base_url.trim_end_matches('/');
        if crate::config::opencode_responses_model(model) {
            format!("{base}/responses")
        } else {
            format!("{base}/chat/completions")
        }
    }

    async fn finish_responses(
        &self,
        url: &str,
        body: &impl Serialize,
        timeout_secs: u64,
    ) -> Result<Reply, ChatError> {
        let token = self.bearer().await?;
        let response = self
            .send_with_retry(
                url,
                body,
                token.as_deref(),
                Some(timeout_secs),
                None,
                timeout_secs != TITLE_TIMEOUT_SECS,
            )
            .await?;
        let response = if response.status().as_u16() == 401 {
            let text = response.text().await.map_err(ChatError::Transport)?;
            if let Some(refreshed) = self.refresh_on_401(token.as_deref()).await? {
                self.send_with_retry(
                    url,
                    body,
                    Some(refreshed.as_str()),
                    Some(timeout_secs),
                    None,
                    timeout_secs != TITLE_TIMEOUT_SECS,
                )
                .await?
            } else {
                return Err(ChatError::Status {
                    status: 401,
                    body: text,
                });
            }
        } else {
            response
        };
        read_completion(response, None, self.stream_idle, true).await
    }

    async fn bearer(&self) -> Result<Option<String>, ChatError> {
        if let Some(codex) = &self.codex {
            let token = auth::codex_access(&codex.client, &codex.path)
                .await
                .map_err(need_login)?;
            return Ok(Some(token));
        }
        if let Some(opencode) = &self.opencode {
            if let Some(name) = &opencode.env_name {
                return match std::env::var(name) {
                    Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
                    _ => Err(ChatError::NeedKey),
                };
            }
            let Some(path) = &opencode.path else {
                return Err(ChatError::NeedKey);
            };
            let key = auth::load_opencode_key(path).map_err(|_| ChatError::NeedKey)?;
            return Ok(Some(key));
        }
        if let Some(grok) = &self.grok {
            let token = auth::access_token(&grok.client, &grok.path)
                .await
                .map_err(need_login)?;
            return Ok(Some(token));
        }
        Ok(self.api_key.clone())
    }

    async fn refresh_on_401(&self, rejected: Option<&str>) -> Result<Option<String>, ChatError> {
        let Some(rejected) = rejected else {
            return Ok(None);
        };
        if let Some(codex) = &self.codex {
            let token = auth::codex_access_after_401(&codex.client, &codex.path, rejected)
                .await
                .map_err(need_login)?;
            return Ok(Some(token));
        }
        let Some(grok) = &self.grok else {
            return Ok(None);
        };
        let token = auth::access_token_after_401(&grok.client, &grok.path, rejected)
            .await
            .map_err(need_login)?;
        Ok(Some(token))
    }

    fn with_auth(
        &self,
        mut request: reqwest::RequestBuilder,
        token: Option<&str>,
    ) -> reqwest::RequestBuilder {
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let Some(codex) = &self.codex else {
            return request;
        };
        request = request
            .header("originator", auth::CODEX_ORIGINATOR)
            .header("version", auth::CODEX_CLIENT_VERSION);
        if let Ok(tokens) = auth::load_codex(&codex.path) {
            if !tokens.account_id.is_empty() {
                request = request.header("chatgpt-account-id", tokens.account_id);
            }
        }
        request
    }

    async fn send_with_retry<B: Serialize>(
        &self,
        url: &str,
        body: &B,
        token: Option<&str>,
        timeout: Option<u64>,
        status: Option<&RetryStatus>,
        retry_allowed: bool,
    ) -> Result<reqwest::Response, ChatError> {
        let guard = retry::StatusGuard(status);
        let mut waited = Duration::ZERO;
        let mut attempts = 0;
        loop {
            loop {
                let deadline = *self.cooldown.lock().unwrap();
                let Some(delay) =
                    deadline.and_then(|d| d.checked_duration_since(tokio::time::Instant::now()))
                else {
                    break;
                };
                if !retry_allowed {
                    return Err(ChatError::Status {
                        status: 429,
                        body: "Provider busy. Skipping the background title request.".into(),
                    });
                }
                if delay > retry::BUDGET.saturating_sub(waited) {
                    return Err(ChatError::Status { status: 429, body: "Provider busy. The retry delay exceeds the automatic retry budget; try again later.".into() });
                }
                guard.set(delay);
                let before = tokio::time::Instant::now();
                tokio::time::sleep(delay.min(Duration::from_secs(1))).await;
                waited = waited.saturating_add(before.elapsed());
            }
            if waited > Duration::ZERO {
                let stagger = retry::jitter();
                if stagger > retry::BUDGET.saturating_sub(waited) {
                    return Err(ChatError::Status {
                        status: 429,
                        body: "Provider busy. Automatic retry wait budget exhausted.".into(),
                    });
                }
                tokio::time::sleep(stagger).await;
                waited = waited.saturating_add(stagger);
                if self
                    .cooldown
                    .lock()
                    .unwrap()
                    .is_some_and(|d| d > tokio::time::Instant::now())
                {
                    continue;
                }
            }
            guard.clear();
            let mut request = self.http.post(url).json(body);
            if let Some(seconds) = timeout {
                request = request.timeout(Duration::from_secs(seconds));
            }
            request = self.with_auth(request, token);
            let response = request.send().await.map_err(ChatError::Transport)?;
            if response.status().as_u16() != 429 {
                return Ok(response);
            }
            let headers = response.headers().clone();
            let text = response.text().await.map_err(ChatError::Transport)?;
            let delay = retry::delay(&headers, &text, attempts);
            if retry::permanent(&text) {
                return Err(ChatError::Status {
                    status: 429,
                    body: retry::message(&text),
                });
            }
            let scheduled = delay.saturating_add(retry::jitter());
            let retained_delay = scheduled.min(Duration::from_secs(86400 * 365));
            if let Some(deadline) = tokio::time::Instant::now().checked_add(retained_delay) {
                let mut cooldown = self.cooldown.lock().unwrap();
                *cooldown = Some(cooldown.map_or(deadline, |old| old.max(deadline)));
            }
            if !retry_allowed
                || attempts >= retry::MAX_RETRIES
                || scheduled > retry::BUDGET.saturating_sub(waited)
            {
                return Err(ChatError::Status {
                    status: 429,
                    body: format!(
                        "Provider busy: {} Try again later. Retry delay: {}s.",
                        retry::message(&text),
                        delay.as_secs()
                    ),
                });
            }
            attempts += 1;
        }
    }
    async fn send_json<B: Serialize>(
        &self,
        url: &str,
        body: &B,
        token: Option<&str>,
        timeout_secs: u64,
    ) -> Result<(u16, String), ChatError> {
        let response = self
            .send_with_retry(
                url,
                body,
                token,
                Some(timeout_secs),
                None,
                timeout_secs != TITLE_TIMEOUT_SECS,
            )
            .await?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(ChatError::Transport)?;
        Ok((status, text))
    }
}

fn decode_message(text: &str) -> Result<Reply, ChatError> {
    let wire: WireResponse = serde_json::from_str(text).map_err(ChatError::Decode)?;
    let tokens = wire.usage.as_ref().and_then(|usage| usage.prompt_tokens);
    let completion_tokens = wire.usage.and_then(|usage| usage.completion_tokens);
    let choice = wire.choices.into_iter().next().ok_or(ChatError::NoChoice)?;
    if !successful_finish(choice.finish_reason.as_deref()) {
        return Err(ChatError::Status {
            status: 502,
            body: format!(
                "the model generation ended with {}",
                choice
                    .finish_reason
                    .as_deref()
                    .unwrap_or("no finish reason")
            ),
        });
    }
    let mut reply = choice.message;
    validate_tool_calls(&reply.tool_calls)?;
    reply.prompt_tokens = tokens;
    reply.completion_tokens = completion_tokens;
    Ok(reply)
}

async fn read_completion(
    mut response: reqwest::Response,
    thoughts: Option<&Arc<Mutex<String>>>,
    idle: Duration,
    responses: bool,
) -> Result<Reply, ChatError> {
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.text().await.map_err(ChatError::Transport)?;
        return Err(ChatError::Status { status, body });
    }
    let mut buf = Vec::new();
    let mut mode = BodyMode::Unknown;
    let mut partial = Partial {
        responses,
        ..Partial::default()
    };
    loop {
        if partial.done {
            break;
        }
        let chunk = match tokio::time::timeout(idle, response.chunk()).await {
            Ok(Ok(chunk)) => chunk,
            Ok(Err(error)) => return Err(ChatError::Transport(error)),
            Err(_elapsed) => {
                return Err(ChatError::Idle {
                    seconds: idle.as_secs(),
                })
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        buf.extend_from_slice(&chunk);
        if mode == BodyMode::Unknown {
            match first_byte(&buf) {
                None => continue,
                Some(b'{') => mode = BodyMode::Json,
                Some(_) => mode = BodyMode::Sse,
            }
        }
        if mode == BodyMode::Sse {
            absorb_sse(&mut buf, &mut partial, thoughts)?;
        }
    }
    match mode {
        BodyMode::Json | BodyMode::Unknown => {
            let text = String::from_utf8(buf).map_err(|_| {
                ChatError::Decode(serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the completion was not utf-8",
                )))
            })?;
            if responses {
                decode_responses(&text)
            } else {
                decode_message(&text)
            }
        }
        BodyMode::Sse => {
            if !partial.done {
                return Err(ChatError::NoChoice);
            }
            partial.finish()
        }
    }
}

fn responses_body(
    model: &str,
    effort: Option<&str>,
    messages: &[Message],
    tools: &[Tool],
    stream: bool,
) -> Value {
    let mut instructions = String::new();
    let mut input = Vec::new();
    for message in messages {
        match message {
            Message::System { content } => {
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(content);
            }
            Message::User { content } => {
                input.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": content.responses_parts(),
                }));
            }
            Message::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(text) = content.as_deref().filter(|text| !text.is_empty()) {
                    input.push(serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                for call in tool_calls {
                    input.push(serde_json::json!({
                        "type": "function_call",
                        "name": call.name,
                        "arguments": call.arguments,
                        "call_id": call.id,
                    }));
                }
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                input.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": content,
                }));
            }
        }
    }
    let mut body = serde_json::json!({
        "model": model,
        "stream": stream,
        "store": false,
        "parallel_tool_calls": true,
        "input": input,
    });
    if !instructions.is_empty() {
        body["instructions"] = Value::String(instructions);
    }
    if !tools.is_empty() {
        let mapped: Vec<Value> = tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                    "strict": false,
                })
            })
            .collect();
        body["tools"] = Value::Array(mapped);
        body["tool_choice"] = Value::String("auto".to_string());
    }
    if let Some(effort) = effort {
        body["reasoning"] = serde_json::json!({ "effort": effort });
    }
    body
}

fn decode_responses(text: &str) -> Result<Reply, ChatError> {
    let value: Value = serde_json::from_str(text).map_err(ChatError::Decode)?;
    decode_response_value(&value)
}

fn successful_finish(reason: Option<&str>) -> bool {
    matches!(reason, None | Some("stop" | "tool_calls"))
}

fn decode_response_value(value: &Value) -> Result<Reply, ChatError> {
    if let Some(status) = value.get("status").and_then(Value::as_str) {
        if status != "completed" {
            return Err(ChatError::Status {
                status: 502,
                body: format!("the model generation ended with {status}"),
            });
        }
    }
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return Err(ChatError::NoChoice);
    };
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for item in output {
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "message" => push_response_text(&mut content, item.get("content")),
            "output_text" => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    content.push_str(text);
                }
            }
            "function_call" => tool_calls.push(ToolCall {
                id: item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                kind: function_kind(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                arguments: response_arguments(item.get("arguments")),
            }),
            _ => {}
        }
    }
    if content.is_empty() && tool_calls.is_empty() {
        return Err(ChatError::NoChoice);
    }
    validate_tool_calls(&tool_calls)?;
    let prompt_tokens = value.get("usage").and_then(|usage| {
        usage
            .get("input_tokens")
            .or_else(|| usage.get("prompt_tokens"))
            .and_then(Value::as_u64)
    });
    Ok(Reply {
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        tool_calls,
        prompt_tokens,
        completion_tokens: value
            .get("usage")
            .and_then(|usage| {
                usage
                    .get("output_tokens")
                    .or_else(|| usage.get("completion_tokens"))
            })
            .and_then(Value::as_u64),
    })
}

fn push_response_text(content: &mut String, value: Option<&Value>) {
    match value {
        Some(Value::String(text)) => content.push_str(text),
        Some(Value::Array(parts)) => {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    content.push_str(text);
                }
            }
        }
        _ => {}
    }
}

fn response_arguments(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyMode {
    Unknown,
    Json,
    Sse,
}

fn first_byte(buf: &[u8]) -> Option<u8> {
    buf.iter().copied().find(|byte| !byte.is_ascii_whitespace())
}

#[derive(Default)]
struct Partial {
    responses: bool,
    response_output: std::collections::BTreeMap<usize, Value>,
    content: String,
    calls: Vec<ToolCall>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    finish_reason: Option<String>,
    saw: bool,
    done: bool,
}

impl Partial {
    fn finish(self) -> Result<Reply, ChatError> {
        if !self.saw || (self.responses && !self.done) {
            return Err(ChatError::NoChoice);
        }
        if !self.responses && !self.done {
            return Err(ChatError::NoChoice);
        }
        if !self.responses && !successful_finish(self.finish_reason.as_deref()) {
            return Err(ChatError::Status {
                status: 502,
                body: format!(
                    "the model generation ended with {}",
                    self.finish_reason.as_deref().unwrap_or("no finish reason")
                ),
            });
        }
        let mut tool_calls = self.calls;
        tool_calls.retain(|call| !call.id.is_empty() || !call.name.is_empty());
        for call in &mut tool_calls {
            if call.kind.is_empty() {
                call.kind = function_kind();
            }
        }
        validate_tool_calls(&tool_calls)?;
        Ok(Reply {
            content: if self.content.is_empty() {
                None
            } else {
                Some(self.content)
            },
            tool_calls,
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
        })
    }
}

fn absorb_sse(
    buf: &mut Vec<u8>,
    partial: &mut Partial,
    thoughts: Option<&Arc<Mutex<String>>>,
) -> Result<(), ChatError> {
    while !partial.done {
        let Some(at) = buf.iter().position(|byte| *byte == b'\n') else {
            break;
        };
        let line = buf.drain(..=at).collect::<Vec<_>>();
        let line = String::from_utf8(line).map_err(|_| {
            ChatError::Decode(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the stream was not utf-8",
            )))
        })?;
        let line = line.trim_end_matches(['\n', '\r']);
        apply_sse_line(line, partial, thoughts)?;
    }
    Ok(())
}

fn apply_sse_line(
    line: &str,
    partial: &mut Partial,
    thoughts: Option<&Arc<Mutex<String>>>,
) -> Result<(), ChatError> {
    if partial.done {
        return Ok(());
    }
    let Some(payload) = line.trim_start().strip_prefix("data:") else {
        return Ok(());
    };
    let payload = payload.strip_prefix(' ').unwrap_or(payload).trim();
    if payload.is_empty() {
        return Ok(());
    }
    if partial.responses {
        return apply_response_event(payload, partial, thoughts);
    }
    if payload == "[DONE]" {
        partial.done = true;
        return Ok(());
    }
    let value: Value = serde_json::from_str(payload).map_err(ChatError::Decode)?;
    if let Some(message) = value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .filter(|_| {
            value.get("error").is_some()
                || value.get("type").and_then(Value::as_str) == Some("error")
        })
    {
        return Err(ChatError::Status {
            status: 502,
            body: message.to_string(),
        });
    }
    let chunk: StreamChunk = serde_json::from_value(value).map_err(ChatError::Decode)?;
    if let Some(usage) = chunk.usage {
        if let Some(tokens) = usage.prompt_tokens {
            partial.prompt_tokens = Some(tokens);
        }
        if let Some(tokens) = usage.completion_tokens {
            partial.completion_tokens = Some(tokens);
        }
    }
    let Some(choice) = chunk.choices.into_iter().next() else {
        return Ok(());
    };
    partial.saw = true;
    if let Some(reason) = choice.finish_reason {
        partial.finish_reason = Some(reason);
    }
    if choice.error.is_some() {
        return Err(ChatError::Status {
            status: 502,
            body: "the model stream failed".to_string(),
        });
    }
    let Some(delta) = choice.delta else {
        return Ok(());
    };
    if let Some(piece) = delta.reasoning_content.or(delta.reasoning) {
        if !piece.is_empty() {
            if let Some(sink) = thoughts {
                sink.lock()
                    .expect("the thoughts buffer is not poisoned")
                    .push_str(&piece);
            }
        }
    }
    if let Some(Value::String(text)) = delta.content {
        partial.content.push_str(&text);
    }
    for tool in delta.tool_calls {
        let index = tool.index.unwrap_or(partial.calls.len());
        while partial.calls.len() <= index {
            partial.calls.push(ToolCall::default());
        }
        let slot = &mut partial.calls[index];
        if let Some(id) = tool.id.as_deref() {
            assign_piece(&mut slot.id, id);
        }
        if let Some(kind) = tool.kind.as_deref() {
            assign_piece(&mut slot.kind, kind);
        }
        if let Some(function) = tool.function {
            if let Some(name) = function.name.as_deref() {
                assign_piece(&mut slot.name, name);
            }
            if let Some(arguments) = function.arguments.as_deref() {
                slot.arguments.push_str(arguments);
            }
        }
    }
    Ok(())
}

fn apply_response_event(
    payload: &str,
    partial: &mut Partial,
    thoughts: Option<&Arc<Mutex<String>>>,
) -> Result<(), ChatError> {
    let event: Value = serde_json::from_str(payload).map_err(ChatError::Decode)?;
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "response.completed" => {
            let mut response = event
                .get("response")
                .filter(|response| response.is_object())
                .ok_or(ChatError::NoChoice)?
                .clone();
            if response
                .get("output")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
            {
                response["output"] = Value::Array(
                    std::mem::take(&mut partial.response_output)
                        .into_values()
                        .collect(),
                );
            }
            let reply = decode_response_value(&response)?;
            partial.content = reply.content.unwrap_or_default();
            partial.calls = reply.tool_calls;
            partial.prompt_tokens = reply.prompt_tokens;
            partial.completion_tokens = reply.completion_tokens;
            partial.saw = true;
            partial.done = true;
        }
        "response.output_item.done" => {
            let item = event.get("item").ok_or(ChatError::NoChoice)?.clone();
            let index = event
                .get("output_index")
                .and_then(Value::as_u64)
                .map(|index| index as usize)
                .unwrap_or(partial.response_output.len());
            partial.response_output.insert(index, item);
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let (Some(sink), Some(delta)) =
                (thoughts, event.get("delta").and_then(Value::as_str))
            {
                sink.lock()
                    .expect("the thoughts buffer is not poisoned")
                    .push_str(delta);
            }
        }
        "response.failed" | "response.incomplete" | "error" => {
            let error = event
                .pointer("/response/error")
                .or_else(|| event.get("error"))
                .unwrap_or(&event);
            let code = error.get("code").and_then(Value::as_str);
            let message = error
                .get("message")
                .or_else(|| event.pointer("/response/incomplete_details/reason"))
                .or_else(|| event.get("message"))
                .and_then(Value::as_str);
            let status = match (kind, code) {
                ("response.incomplete", _) => 400,
                (_, Some("rate_limit_exceeded")) => 429,
                (_, Some("server_error" | "internal_server_error")) => 502,
                (_, Some(_)) => 400,
                _ => 502,
            };
            return Err(ChatError::Status {
                status,
                body: match (code, message) {
                    (Some(code), Some(message)) => format!("{code}: {message}"),
                    (Some(code), None) => code.to_string(),
                    (None, Some(message)) => message.to_string(),
                    (None, None) => format!("{kind}: {event}"),
                },
            });
        }
        _ => {}
    }
    Ok(())
}

fn assign_piece(slot: &mut String, next: &str) {
    if next.is_empty() {
        return;
    }
    if slot.is_empty() || next.starts_with(slot.as_str()) {
        *slot = next.to_string();
    } else if !slot.ends_with(next) {
        slot.push_str(next);
    }
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Option<StreamDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct StreamDelta {
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    tool_calls: Vec<StreamTool>,
}

#[derive(Deserialize)]
struct StreamTool {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    function: Option<StreamFn>,
}

#[derive(Deserialize)]
struct StreamFn {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

pub fn request_streams(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(Value::as_bool))
        .unwrap_or(false)
}

pub fn completion_as_sse(body: &str) -> String {
    let trimmed = body.trim_start();
    if trimmed.starts_with("data:") {
        let mut out = body.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        return out;
    }
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return format!("data: {body}\n\ndata: [DONE]\n\n");
    };
    if value.get("output").is_some() {
        return format!(
            "data: {}\n\n",
            serde_json::json!({ "type": "response.completed", "response": value })
        );
    }
    let Some(message) = value.pointer("/choices/0/message").cloned() else {
        let line = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string());
        return format!("data: {line}\n\ndata: [DONE]\n\n");
    };
    let mut delta = serde_json::Map::new();
    if let Some(content) = message.get("content") {
        if !content.is_null() {
            delta.insert("content".to_string(), content.clone());
        }
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        let indexed: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(index, call)| {
                let mut call = call.clone();
                if let Some(obj) = call.as_object_mut() {
                    obj.entry("index".to_string()).or_insert(Value::from(index));
                }
                call
            })
            .collect();
        delta.insert("tool_calls".to_string(), Value::Array(indexed));
    }
    let mut chunk = serde_json::json!({
        "choices": [{ "index": 0, "delta": delta }]
    });
    if let Some(usage) = value.get("usage") {
        chunk["usage"] = usage.clone();
    }
    let line = serde_json::to_string(&chunk).unwrap_or_else(|_| "{}".to_string());
    format!("data: {line}\n\ndata: [DONE]\n\n")
}

pub fn answer_completion(stream: &mut TcpStream, status: u16, body: &str, request: &str) {
    let (content_type, payload) = if status == 200 && request_streams(request) {
        ("text/event-stream", completion_as_sse(body))
    } else {
        ("application/json", body.to_string())
    };
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
    let _ = stream.flush();
}

fn grok_session(
    config: &Config,
    root: Option<&Path>,
    auth: Option<AuthClient>,
) -> Result<Option<GrokSession>, ChatError> {
    if config.provider.as_deref() != Some(auth::GROK_PROVIDER) {
        return Ok(None);
    }
    let path = root
        .map(|root| root.join(auth::AUTH_FILE))
        .or_else(auth::default_path)
        .unwrap_or_else(|| PathBuf::from(auth::AUTH_FILE));
    let client = match auth {
        Some(client) => client,
        None => AuthClient::new(
            config
                .grok_client_id
                .as_deref()
                .unwrap_or(auth::DEFAULT_CLIENT_ID),
        ),
    };
    Ok(Some(GrokSession { path, client }))
}

fn opencode_session(
    config: &Config,
    root: Option<&Path>,
) -> Result<Option<OpenCodeSession>, ChatError> {
    if !config.is_opencode() {
        return Ok(None);
    }
    if let Some(name) = config
        .api_key_env
        .clone()
        .filter(|_| config.api_key().is_some())
    {
        return Ok(Some(OpenCodeSession {
            path: None,
            env_name: Some(name),
        }));
    }
    let path = root
        .map(|root| root.join(auth::OPENCODE_AUTH_FILE))
        .or_else(auth::default_opencode_path)
        .unwrap_or_else(|| PathBuf::from(auth::OPENCODE_AUTH_FILE));
    let saved = root
        .map(|root| auth::provider_key_path(root, config.provider.as_deref()))
        .or_else(|| {
            Config::default_path().and_then(|path| {
                path.parent()
                    .map(|root| auth::provider_key_path(root, config.provider.as_deref()))
            })
        });
    let path = saved.filter(|path| path.exists()).unwrap_or(path);
    Ok(Some(OpenCodeSession {
        path: Some(path),
        env_name: None,
    }))
}

fn codex_session(
    config: &Config,
    root: Option<&Path>,
    auth: Option<CodexAuth>,
) -> Result<Option<CodexSession>, ChatError> {
    if !config.is_codex() {
        return Ok(None);
    }
    let path = root
        .map(|root| root.join(auth::CODEX_AUTH_FILE))
        .or_else(auth::default_codex_path)
        .unwrap_or_else(|| PathBuf::from(auth::CODEX_AUTH_FILE));
    let client = auth.unwrap_or_else(|| CodexAuth::at(auth::CODEX_ISSUER));
    Ok(Some(CodexSession { path, client }))
}

fn need_login(error: AuthError) -> ChatError {
    match error {
        AuthError::Transport(source) => ChatError::Transport(source),
        _ => ChatError::NeedLogin,
    }
}
