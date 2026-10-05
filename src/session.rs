//! A session directory on disk: `meta.json` beside an append-only
//! `events.jsonl`.
//!
//! ```text
//! sessions/91bc7a1d/
//!   meta.json      the session's own facts, rewritten whenever one changes
//!   events.jsonl   one JSON object per line, only ever added to
//! ```
//!
//! The directory is passed in rather than guessed at. The default
//! `~/.kyotoagent` root is the server's business, and a test wants a temporary
//! directory, so nothing here reads a global.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::events::Event;
use crate::screen::Status;

/// The file the session's facts live in.
pub const META_FILE: &str = "meta.json";
/// The file the transcript lives in.
pub const EVENTS_FILE: &str = "events.jsonl";

/// A problem with a session directory, with the path that caused it.
#[derive(Debug)]
pub enum SessionError {
    /// A file could not be read or written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A file held something that is not a session document.
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// The directory has no `meta.json` in it.
    MissingMeta { path: PathBuf },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            SessionError::Json { path, source } => write!(f, "{}: {source}", path.display()),
            SessionError::MissingMeta { path } => write!(
                f,
                "{}: no {META_FILE}, so this is not a session directory",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SessionError {}

/// What a session is, and what it has been allowed to do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    #[serde(default, rename = "taskId", skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// The session id, as the directory is named.
    pub id: String,
    /// The working directory every relative path in the log is against.
    pub workspace: String,
    /// The model the session talks to.
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<SessionModel>,
    pub created_at: String,
    pub updated_at: String,
    pub status: Status,
    /// What one `allow_session` answer remembered. Every entry is exact: a
    /// stored path is that path and no other, and a stored argv is that command
    /// and no other. A session's allows are its own.
    #[serde(default)]
    pub allow: AllowList,
    #[serde(default, rename = "pullUrl", skip_serializing_if = "Option::is_none")]
    pub pull_url: Option<String>,
    #[serde(
        default,
        rename = "contextLength",
        skip_serializing_if = "Option::is_none"
    )]
    pub context_length: Option<u64>,
    #[serde(
        default,
        rename = "promptTokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub yolo: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enhance: bool,
    #[serde(default = "default_show_closeout")]
    pub show_closeout: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "title_unset")]
    pub title: Option<String>,
    #[serde(
        default,
        rename = "requestedWorkspace",
        skip_serializing_if = "Option::is_none"
    )]
    pub requested_workspace: Option<String>,
    #[serde(default, rename = "parentId", skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closeout_reviewer: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<crate::goal::Goal>,
}

fn default_show_closeout() -> bool {
    true
}

pub(crate) fn title_unset(title: &Option<String>) -> bool {
    title.as_deref().map(str::trim).unwrap_or("").is_empty()
}

pub fn title_ask(text: &str) -> String {
    text.chars().take(500).collect()
}

pub fn tidy_title(reply: &str) -> String {
    let line = reply.lines().next().unwrap_or("");
    let text = line
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`'))
        .trim();
    text.chars().take(60).collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionModel {
    pub model: String,
    pub effort: Option<String>,
    pub provider: Option<String>,
}

impl SessionMeta {
    /// A new session's facts, with nothing allowed yet and a fresh timestamp on
    /// both ends.
    pub fn new(id: &str, workspace: &Path, model: &str, at: &str) -> SessionMeta {
        SessionMeta {
            task_id: None,
            id: id.to_string(),
            workspace: workspace.display().to_string(),
            model: model.to_string(),
            effort: None,
            model_override: None,
            created_at: at.to_string(),
            updated_at: at.to_string(),
            status: Status::Idle,
            allow: AllowList::default(),
            pull_url: None,
            context_length: None,
            prompt_tokens: None,
            yolo: false,
            enhance: false,
            show_closeout: true,
            profile: None,
            title: None,
            requested_workspace: None,
            parent_id: None,
            closeout_reviewer: false,
            description: None,
            isolation: None,
            goal: None,
        }
    }
}

pub(crate) fn new_task_id() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .expect("task identity randomness");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The exact things one session has been allowed, remembered on `meta.json`.
///
/// An entry is a whole decision, never a prefix. `/w/README.md` allows that
/// file and not `/w/README.md.bak` or `/w/`, and `["cargo", "test"]` allows
/// that command and not `["cargo", "test", "--all"]`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowList {
    /// Absolute paths this session may write without asking again.
    #[serde(default)]
    pub write_paths: Vec<String>,
    /// Absolute paths outside the workspace this session may read without
    /// asking again.
    #[serde(default)]
    pub outside_read_paths: Vec<String>,
    /// Argv lists this session may run without asking again.
    #[serde(default)]
    pub argv: Vec<Vec<String>>,
    #[serde(default)]
    pub fetch_origins: Vec<String>,
    #[serde(default)]
    pub web_search: bool,
}

impl AllowList {
    /// Whether this exact path may be written.
    pub fn allows_write(&self, path: &str) -> bool {
        self.write_paths.iter().any(|allowed| allowed == path)
    }

    /// Whether this exact path outside the workspace may be read.
    pub fn allows_outside_read(&self, path: &str) -> bool {
        self.outside_read_paths
            .iter()
            .any(|allowed| allowed == path)
    }

    /// Whether this exact argv may be run.
    pub fn allows_argv(&self, argv: &[String]) -> bool {
        self.argv.iter().any(|allowed| allowed == argv)
    }

    /// Remember an `allow_session` answer, keeping each list free of repeats
    /// and in the order the answers came.
    pub fn remember(
        &mut self,
        write_path: Option<&str>,
        outside_read: Option<&str>,
        argv: Option<&[String]>,
    ) {
        if let Some(path) = write_path {
            if !self.allows_write(path) {
                self.write_paths.push(path.to_string());
            }
        }
        if let Some(path) = outside_read {
            if !self.allows_outside_read(path) {
                self.outside_read_paths.push(path.to_string());
            }
        }
        if let Some(argv) = argv {
            if !self.allows_argv(argv) {
                self.argv.push(argv.to_vec());
            }
        }
    }

    pub fn allows_fetch(&self, origin: &str) -> bool {
        self.fetch_origins.iter().any(|allowed| allowed == origin)
    }

    pub fn remember_fetch(&mut self, origin: &str) {
        if !self.allows_fetch(origin) {
            self.fetch_origins.push(origin.to_string());
        }
    }

    pub fn allows_search(&self) -> bool {
        self.web_search
    }

    pub fn remember_search(&mut self) {
        self.web_search = true;
    }
}

pub fn github_origin() -> String {
    format!("{}{}{}{}", "https:", "/", "/", "github.com")
}

pub fn github_pull_url(text: &str) -> Option<String> {
    let marker = "/pull/";
    let mut best = None;
    let mut from = 0;
    while let Some(rel) = text[from..].find(marker) {
        let at = from + rel;
        if let Some(url) = take_github_pull(text, at) {
            best = Some(url);
        }
        from = at + marker.len();
    }
    best
}

fn take_github_pull(text: &str, pull_at: usize) -> Option<String> {
    let https = concat!("https:", "/", "/");
    let http = concat!("http:", "/", "/");
    let before = &text[..pull_at];
    let start = before.rfind(https).or_else(|| before.rfind(http))?;
    let scheme_len = if text[start..].starts_with(https) {
        https.len()
    } else {
        http.len()
    };
    let host_path = &text[start + scheme_len..pull_at];
    if !host_path.starts_with("github.com/") {
        return None;
    }
    let repo = &host_path["github.com/".len()..];
    if repo.is_empty() || !repo.contains('/') || repo.starts_with('/') || repo.ends_with('/') {
        return None;
    }
    let after = &text[pull_at + "/pull/".len()..];
    let n = after.chars().take_while(|c| c.is_ascii_digit()).count();
    if n == 0 {
        return None;
    }
    Some(text[start..pull_at + "/pull/".len() + n].to_string())
}

pub fn pull_number(url: &str) -> Option<&str> {
    let rest = url.split("/pull/").nth(1)?;
    let digits = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

pub fn pull_mark(url: &str) -> Option<String> {
    pull_number(url).map(|n| format!("pr {n}"))
}

pub fn opened_line(url: &str) -> String {
    format!("Opened {url}")
}

pub fn url_from_pr_view(text: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) {
        if let Some(url) = value.get("url").and_then(|v| v.as_str()) {
            return github_pull_url(url);
        }
    }
    github_pull_url(text)
}

fn meta_mutex(dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut map = locks.lock().expect("session meta locks");
    map.entry(dir.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// A session directory. Cheap to make: it holds no open file, so the turn loop
/// can keep one and the log stays append-only across processes.
#[derive(Clone, Debug)]
pub struct Session {
    dir: PathBuf,
    event_cache: Arc<Mutex<EventCache>>,
}

#[derive(Debug, Default)]
struct EventCache {
    stamp: Option<LogStamp>,
    generation: u64,
    events: Arc<Vec<Event>>,
}

#[derive(Debug, PartialEq, Eq)]
struct LogStamp {
    len: u64,
    identity: (u64, u64),
    modified: Option<std::time::SystemTime>,
    changed: (i64, i64),
}

impl LogStamp {
    fn read(file: &File) -> std::io::Result<Self> {
        let meta = file.metadata()?;
        Ok(Self {
            len: meta.len(),
            identity: (meta.dev(), meta.ino()),
            modified: meta.modified().ok(),
            changed: (meta.ctime(), meta.ctime_nsec()),
        })
    }
}

impl Session {
    /// The session in `dir`, whether or not anything is written there yet.
    pub fn at(dir: &Path) -> Session {
        Session {
            dir: dir.to_path_buf(),
            event_cache: Arc::new(Mutex::new(EventCache::default())),
        }
    }

    /// The directory itself.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `meta.json` in this session.
    pub fn meta_path(&self) -> PathBuf {
        self.dir.join(META_FILE)
    }

    /// `events.jsonl` in this session.
    pub fn events_path(&self) -> PathBuf {
        self.dir.join(EVENTS_FILE)
    }

    /// Write `meta.json` and an empty log, creating the directory if it is not
    /// there yet. An existing log is left alone: a session is never restarted
    /// over its own transcript.
    pub fn create(&self, meta: &SessionMeta) -> Result<(), SessionError> {
        fs::create_dir_all(&self.dir).map_err(|source| SessionError::Io {
            path: self.dir.clone(),
            source,
        })?;
        self.write_meta(meta)?;
        if !self.events_path().exists() {
            File::create(self.events_path()).map_err(|source| SessionError::Io {
                path: self.events_path(),
                source,
            })?;
        }
        Ok(())
    }

    /// The session's facts.
    pub fn meta(&self) -> Result<SessionMeta, SessionError> {
        let mutex = meta_mutex(&self.dir);
        let _guard = mutex.lock().expect("session meta");
        self.read_meta()
    }

    fn read_meta(&self) -> Result<SessionMeta, SessionError> {
        let path = self.meta_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionError::MissingMeta { path })
            }
            Err(source) => return Err(SessionError::Io { path, source }),
        };
        serde_json::from_str(&text).map_err(|source| SessionError::Json { path, source })
    }

    /// Replace `meta.json`. This is the one file a session rewrites, because
    /// its status and its session allows change as the turn goes on.
    pub fn write_meta(&self, meta: &SessionMeta) -> Result<(), SessionError> {
        let mutex = meta_mutex(&self.dir);
        let _guard = mutex.lock().expect("session meta");
        self.write_meta_locked(meta)
    }

    pub fn update(
        &self,
        change: impl FnOnce(&mut SessionMeta) -> bool,
    ) -> Result<(), SessionError> {
        let mutex = meta_mutex(&self.dir);
        let _guard = mutex.lock().expect("session meta");
        let mut meta = self.read_meta()?;
        if change(&mut meta) {
            self.write_meta_locked(&meta)?;
        }
        Ok(())
    }

    fn write_meta_locked(&self, meta: &SessionMeta) -> Result<(), SessionError> {
        let path = self.meta_path();
        let mut text = serde_json::to_string_pretty(meta).map_err(|source| SessionError::Json {
            path: path.clone(),
            source,
        })?;
        text.push('\n');
        let tmp = self.dir.join(format!(".{META_FILE}.tmp"));
        fs::write(&tmp, text).map_err(|source| SessionError::Io {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, &path).map_err(|source| SessionError::Io { path, source })
    }

    pub fn set_pull_url(&self, url: &str) -> Result<(), SessionError> {
        self.update(|meta| {
            if meta.pull_url.as_deref() == Some(url) {
                return false;
            }
            meta.pull_url = Some(url.to_string());
            true
        })
    }

    pub fn set_model_effort(&self, model: &str, effort: Option<&str>) -> Result<(), SessionError> {
        self.update(|meta| {
            let effort = effort.map(str::to_string);
            if meta.model == model && meta.effort == effort && meta.model_override.is_none() {
                return false;
            }
            if meta.model != model {
                meta.context_length = None;
            }
            meta.model = model.to_string();
            meta.effort = effort;
            meta.model_override = None;
            true
        })
    }

    pub fn set_session_model(&self, selection: SessionModel) -> Result<(), SessionError> {
        self.update(|meta| {
            if meta.model_override.as_ref() == Some(&selection) {
                return false;
            }
            meta.context_length = None;
            meta.model = selection.model.clone();
            meta.effort = selection.effort.clone();
            meta.model_override = Some(selection.clone());
            true
        })
    }

    pub fn set_yolo(&self, yolo: bool) -> Result<(), SessionError> {
        self.update(|meta| {
            if meta.yolo == yolo {
                return false;
            }
            meta.yolo = yolo;
            true
        })
    }

    pub fn set_enhance(&self, enhance: bool) -> Result<(), SessionError> {
        self.update(|meta| {
            if meta.enhance == enhance {
                return false;
            }
            meta.enhance = enhance;
            true
        })
    }

    pub fn set_show_closeout(&self, show: bool) -> Result<(), SessionError> {
        self.update(|meta| {
            if meta.show_closeout == show {
                return false;
            }
            meta.show_closeout = show;
            true
        })
    }

    pub fn set_profile(&self, profile: Option<&str>) -> Result<(), SessionError> {
        let profile = profile
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        self.update(|meta| {
            if meta.profile == profile {
                return false;
            }
            meta.profile = profile;
            true
        })
    }

    /// The current time, stamped on `updated_at` as well as kept in the
    /// caller's own event.
    pub fn touch(&self, meta: &mut SessionMeta, at: &str) -> Result<(), SessionError> {
        meta.updated_at = at.to_string();
        self.write_meta(meta)
    }

    /// Add one line to the log. The write is appended and flushed, so a turn
    /// that ends here leaves a log another process can read.
    pub fn append(&self, event: &Event) -> Result<(), SessionError> {
        let path = self.events_path();
        let mut line = serde_json::to_string(event).map_err(|source| SessionError::Json {
            path: path.clone(),
            source,
        })?;
        line.push('\n');
        let mut cache = self
            .event_cache
            .lock()
            .expect("the event cache is not poisoned");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| SessionError::Io {
                path: path.clone(),
                source,
            })?;
        file.lock().map_err(|source| SessionError::Io {
            path: path.clone(),
            source,
        })?;
        let before = LogStamp::read(&file).map_err(|source| SessionError::Io {
            path: path.clone(),
            source,
        })?;
        file.write_all(line.as_bytes())
            .and_then(|()| file.flush())
            .map_err(|source| SessionError::Io { path, source })?;
        let after = LogStamp::read(&file).map_err(|source| SessionError::Io {
            path: self.events_path(),
            source,
        })?;
        if cache.stamp.as_ref() == Some(&before) && after.len == before.len + line.len() as u64 {
            Arc::make_mut(&mut cache.events).push(event.clone());
            cache.stamp = Some(after);
        } else {
            cache.stamp = None;
        }
        cache.generation = cache.generation.wrapping_add(1);
        Ok(())
    }

    pub(crate) fn recover_event_log(&self) -> Result<(), SessionError> {
        let path = self.events_path();
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(SessionError::Io {
                    path: path.clone(),
                    source,
                })
            }
        };
        file.lock().map_err(|source| SessionError::Io {
            path: path.clone(),
            source,
        })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| SessionError::Io {
                path: path.clone(),
                source,
            })?;
        let mut offset = 0;
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if line.iter().all(u8::is_ascii_whitespace) {
                offset += line.len();
                continue;
            }
            match serde_json::from_slice::<Event>(line) {
                Ok(_) => offset += line.len(),
                Err(source) if source.is_eof() && !line.ends_with(b"\n") => {
                    let archive = self.dir.join(format!(
                        "events.interrupted-{}.jsonl",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos()
                    ));
                    let mut backup = OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .mode(0o600)
                        .open(&archive)
                        .map_err(|source| SessionError::Io {
                            path: archive.clone(),
                            source,
                        })?;
                    backup
                        .write_all(line)
                        .and_then(|()| backup.sync_all())
                        .map_err(|source| SessionError::Io {
                            path: archive,
                            source,
                        })?;
                    file.set_len(offset as u64)
                        .and_then(|()| file.sync_all())
                        .map_err(|source| SessionError::Io {
                            path: path.clone(),
                            source,
                        })?;
                    return Ok(());
                }
                Err(source) => {
                    return Err(SessionError::Json {
                        path: path.clone(),
                        source,
                    })
                }
            }
        }
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            file.write_all(b"\n")
                .and_then(|()| file.sync_all())
                .map_err(|source| SessionError::Io {
                    path: path.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    /// Every event in the log, in the order it was appended. A blank line is
    /// skipped, so a log that ends with a stray newline is not a broken one.
    pub fn events(&self) -> Result<Vec<Event>, SessionError> {
        Ok(self.event_snapshot()?.1.as_ref().clone())
    }

    pub(crate) fn event_snapshot(&self) -> Result<(u64, Arc<Vec<Event>>), SessionError> {
        let mut cache = self
            .event_cache
            .lock()
            .expect("the event cache is not poisoned");
        let path = self.events_path();
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                cache.stamp = None;
                cache.events = Arc::new(Vec::new());
                cache.generation = cache.generation.wrapping_add(1);
                return Ok((cache.generation, Arc::clone(&cache.events)));
            }
            Err(source) => return Err(SessionError::Io { path, source }),
        };
        file.lock_shared().map_err(|source| SessionError::Io {
            path: path.clone(),
            source,
        })?;
        let stamp = LogStamp::read(&file).map_err(|source| SessionError::Io {
            path: path.clone(),
            source,
        })?;
        if cache.stamp.as_ref() == Some(&stamp) {
            return Ok((cache.generation, Arc::clone(&cache.events)));
        }
        let mut events = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|source| SessionError::Io {
                path: path.clone(),
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let event = serde_json::from_str(&line).map_err(|source| SessionError::Json {
                path: path.clone(),
                source,
            })?;
            events.push(event);
        }
        cache.events = Arc::new(events);
        cache.stamp = Some(stamp);
        cache.generation = cache.generation.wrapping_add(1);
        Ok((cache.generation, Arc::clone(&cache.events)))
    }

    /// The id the next event gets: one past the number of lines already in the
    /// log, so ids stay `e1`, `e2`, and so on.
    pub fn next_event_id(&self) -> Result<String, SessionError> {
        Ok(format!("e{}", self.event_snapshot()?.1.len() + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventKind, PermissionBody};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kyotoagent-session-{}-{}",
            std::process::id(),
            name
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_new_session_has_a_meta_and_an_empty_log() {
        let dir = temp_dir("new");
        let session = Session::at(&dir);
        session
            .create(&SessionMeta::new(
                "91bc",
                Path::new("/w"),
                "gpt",
                "2026-09-29T00:00:00.000Z",
            ))
            .expect("the session is created");

        assert_eq!(session.meta().expect("meta reads back").id, "91bc");
        assert_eq!(session.meta().expect("meta reads back").effort, None);
        assert!(session.events().expect("the log reads").is_empty());
        assert_eq!(session.next_event_id().expect("an id"), "e1");

        session
            .set_model_effort("grok-4.6", Some("high"))
            .expect("the pair writes");
        let meta = session.meta().expect("meta reads back");
        assert_eq!(meta.model, "grok-4.6");
        assert_eq!(meta.effort.as_deref(), Some("high"));
        assert!(
            !fs::read_to_string(session.meta_path())
                .expect("meta text")
                .contains("\"title\""),
            "an empty title is left out of meta.json"
        );

        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_title_keeps_the_first_line_without_quotes_and_stops_at_sixty() {
        assert_eq!(tidy_title("\"Name the binary\"\nmore"), "Name the binary");
        assert_eq!(tidy_title("  'Name the binary'  "), "Name the binary");
        assert_eq!(tidy_title(""), "");
        let long = "a".repeat(80);
        assert_eq!(tidy_title(&long).chars().count(), 60);
        assert_eq!(title_ask(&"b".repeat(800)).chars().count(), 500);
    }

    #[test]
    fn an_old_meta_without_show_closeout_shows_the_checks() {
        let dir = temp_dir("old-meta");
        fs::create_dir_all(&dir).expect("the directory exists");
        fs::write(
            dir.join(META_FILE),
            r#"{"id":"91bc","workspace":"/w","model":"m","created_at":"t","updated_at":"t","status":"idle"}"#,
        )
        .expect("the old meta writes");
        let meta = Session::at(&dir).meta().expect("the old meta loads");
        assert!(meta.show_closeout);
        assert!(!meta.yolo);
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_session_allow_is_exact_and_stays_in_the_session() {
        let mut allow = AllowList::default();
        allow.remember(Some("/w/README.md"), None, None);

        assert!(allow.allows_write("/w/README.md"));
        assert!(
            !allow.allows_write("/w/README.md.bak"),
            "not a prefix match"
        );
        assert!(!allow.allows_write("/w/other.md"), "not a whole directory");
        // A second session starts empty: nothing here is shared.
        assert!(!AllowList::default().allows_write("/w/README.md"));
        assert!(allow.fetch_origins.is_empty());
        let stored: AllowList =
            serde_json::from_str(r#"{"writePaths":[],"outsideReadPaths":[],"argv":[]}"#)
                .expect("an older allow list still loads");
        assert!(stored.fetch_origins.is_empty());
        assert!(!stored.allows_search());
        assert!(!allow.allows_search());
        allow.remember_search();
        assert!(allow.allows_search());
        allow.remember_fetch("https://docs.example");
        assert!(allow.allows_fetch("https://docs.example"));
        assert!(!allow.allows_fetch("https://other.example"));
        allow.remember_fetch("https://docs.example");
        assert_eq!(allow.fetch_origins.len(), 1);
    }

    #[test]
    fn an_old_meta_loads_with_enhance_off() {
        let meta: SessionMeta = serde_json::from_str(
            r#"{"id":"91bc","workspace":"/w","model":"gpt","created_at":"2026-09-29T00:00:00.000Z","updated_at":"2026-09-29T00:00:00.000Z","status":"idle","yolo":true}"#,
        )
        .expect("an old meta loads");
        assert!(!meta.enhance);
        assert!(meta.yolo);
    }

    #[test]
    fn a_stored_argv_has_to_match_word_for_word() {
        let mut allow = AllowList::default();
        allow.remember(None, None, Some(&["cargo".to_string(), "test".to_string()]));

        assert!(allow.allows_argv(&["cargo".to_string(), "test".to_string()]));
        assert!(!allow.allows_argv(&[
            "cargo".to_string(),
            "test".to_string(),
            "--all".to_string()
        ]));
        assert!(!allow.allows_argv(&["cargo".to_string()]));
    }

    #[test]
    fn appending_twice_keeps_both_events() {
        let dir = temp_dir("append");
        let session = Session::at(&dir);
        session
            .create(&SessionMeta::new(
                "91bc",
                Path::new("/w"),
                "gpt",
                "2026-09-29T00:00:00.000Z",
            ))
            .expect("the session is created");

        for id in ["e1", "e2"] {
            let event = Event::new(id, "2026-09-29T00:00:00.000Z", "t1", EventKind::UserAsk)
                .with_body(&crate::events::AskBody {
                    images: Vec::new(),
                    text: "hello".into(),
                    context: String::new(),
                    skill: String::new(),
                    silent: false,
                })
                .expect("a body serializes");
            session.append(&event).expect("the event is appended");
        }

        let events = session.events().expect("the log reads");
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].id, "e2");
        assert_eq!(session.next_event_id().expect("an id"), "e3");

        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn recovery_preserves_a_complete_final_event_without_a_newline() {
        let dir = temp_dir("complete-tail");
        fs::create_dir_all(&dir).unwrap();
        let session = Session::at(&dir);
        let event = Event::new("e1", "2026-09-29T00:00:00.000Z", "t1", EventKind::Result);
        fs::write(session.events_path(), serde_json::to_vec(&event).unwrap()).unwrap();
        session.recover_event_log().unwrap();
        session
            .append(&Event::new(
                "e2",
                "2026-09-29T00:00:00.000Z",
                "t1",
                EventKind::Result,
            ))
            .unwrap();
        let events = session.events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], event);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recovery_leaves_malformed_complete_lines_unchanged() {
        let dir = temp_dir("invalid-complete-line");
        fs::create_dir_all(&dir).unwrap();
        let session = Session::at(&dir);
        let bytes = b"{\"id\":\"e1\"\n";
        fs::write(session.events_path(), bytes).unwrap();
        assert!(session.recover_event_log().is_err());
        assert_eq!(fs::read(session.events_path()).unwrap(), bytes);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recovery_rejects_invalid_json_without_a_newline() {
        let dir = temp_dir("invalid-json-tail");
        fs::create_dir_all(&dir).unwrap();
        let session = Session::at(&dir);
        let bytes = b"{broken}";
        fs::write(session.events_path(), bytes).unwrap();
        assert!(session.recover_event_log().is_err());
        assert_eq!(fs::read(session.events_path()).unwrap(), bytes);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_permission_body_survives_the_log() {
        let dir = temp_dir("body");
        let session = Session::at(&dir);
        session
            .create(&SessionMeta::new(
                "91bc",
                Path::new("/w"),
                "gpt",
                "2026-09-29T00:00:00.000Z",
            ))
            .expect("the session is created");
        let event = Event::new(
            "e1",
            "2026-09-29T00:00:00.000Z",
            "t1",
            EventKind::Permission,
        )
        .with_body(&PermissionBody::write(
            "Replace README.md",
            "/w/README.md",
            &["@@ -1 +1,2 @@"],
        ))
        .expect("a body serializes");
        session.append(&event).expect("the event is appended");

        let back = session.events().expect("the log reads").remove(0);
        assert_eq!(
            back.body_as::<PermissionBody>().expect("the body parses"),
            PermissionBody::write("Replace README.md", "/w/README.md", &["@@ -1 +1,2 @@"])
        );

        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_directory_without_meta_is_not_a_session() {
        let dir = temp_dir("no-meta");
        fs::create_dir_all(&dir).expect("the directory exists");
        assert!(matches!(
            Session::at(&dir).meta(),
            Err(SessionError::MissingMeta { .. })
        ));
        fs::remove_dir_all(&dir).expect("clean up");
    }

    fn sample_pull(n: u32) -> String {
        format!("{}/pmdroid/pagent/pull/{n}", github_origin())
    }

    #[test]
    fn a_github_pull_url_is_read_from_stdout() {
        let url = sample_pull(14);
        let stdout = format!("Creating pull request\n{url}\n");
        assert_eq!(github_pull_url(&stdout).as_deref(), Some(url.as_str()));
        assert_eq!(pull_number(&url), Some("14"));
        assert_eq!(pull_mark(&url).as_deref(), Some("pr 14"));
        assert_eq!(opened_line(&url), format!("Opened {url}"));
        assert!(github_pull_url("no pull here").is_none());
        assert!(github_pull_url("github.com/pmdroid/pagent/pull/14").is_none());
    }

    #[test]
    fn a_github_pull_url_is_read_from_stderr_and_the_last_one_wins() {
        let first = sample_pull(14);
        let second = sample_pull(22);
        let stderr = format!("warning\n{first}\n{second}\n");
        assert_eq!(github_pull_url(&stderr).as_deref(), Some(second.as_str()));
        let json = format!("{{\"url\":\"{second}\"}}");
        assert_eq!(url_from_pr_view(&json).as_deref(), Some(second.as_str()));
    }

    #[test]
    fn set_pull_url_replaces_the_stored_url() {
        let dir = temp_dir("pull");
        let session = Session::at(&dir);
        session
            .create(&SessionMeta::new(
                "91bc",
                Path::new("/w"),
                "gpt",
                "2026-09-29T00:00:00.000Z",
            ))
            .expect("the session is created");
        assert_eq!(session.meta().expect("meta").pull_url, None);
        let first = sample_pull(14);
        session.set_pull_url(&first).expect("stored");
        assert_eq!(
            session.meta().expect("meta").pull_url.as_deref(),
            Some(first.as_str())
        );
        let second = sample_pull(22);
        session.set_pull_url(&second).expect("replaced");
        assert_eq!(
            session.meta().expect("meta").pull_url.as_deref(),
            Some(second.as_str())
        );
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn an_old_meta_loads_with_no_profile_and_a_new_one_omits_the_field() {
        let dir = temp_dir("old-profile");
        fs::create_dir_all(&dir).expect("the directory exists");
        fs::write(
            dir.join(META_FILE),
            r#"{"id":"91bc","workspace":"/w","model":"gpt","created_at":"2026-09-29T00:00:00.000Z","updated_at":"2026-09-29T00:00:00.000Z","status":"idle"}"#,
        )
        .expect("the old meta writes");
        let session = Session::at(&dir);
        let loaded = session.meta().expect("the old meta loads");
        assert_eq!(loaded.profile, None);
        assert_eq!(loaded.id, "91bc");
        let fresh = temp_dir("fresh-profile");
        let created = Session::at(&fresh);
        created
            .create(&SessionMeta::new(
                "91bc",
                Path::new("/w"),
                "gpt",
                "2026-09-29T00:00:00.000Z",
            ))
            .expect("created");
        let text = fs::read_to_string(created.meta_path()).expect("meta text");
        assert!(!text.contains("\"profile\""), "{text}");
        created.set_profile(Some("review")).expect("stored");
        let stored = fs::read_to_string(created.meta_path()).expect("meta text");
        assert!(stored.contains("\"profile\": \"review\""), "{stored}");
        created.set_profile(Some("")).expect("cleared");
        let cleared = fs::read_to_string(created.meta_path()).expect("meta text");
        assert!(!cleared.contains("\"profile\""), "{cleared}");
        assert_eq!(created.meta().expect("meta").profile, None);
        fs::remove_dir_all(&dir).expect("clean up");
        fs::remove_dir_all(&fresh).expect("clean up");
    }
}
