//! The four tools a turn can call: `read_file`, `list_dir`, `write_file`, and
//! `run`.
//!
//! Every path a tool is given is resolved first: relative to the workspace,
//! then through every symlink, so what the tool touches is the file itself and
//! not a link that points somewhere else. A path the caller meant to be inside
//! the workspace has to land inside it. A path that resolves out is refused
//! rather than followed, which is the whole answer to a symlink dropped in a
//! repository to see whether the agent would read `/etc/shadow` through it.
//!
//! Reading and listing inside the workspace are the quiet tools. They run and
//! the turn keeps going, because a file the agent looked at is not something
//! the user has to answer for. Everything else stops at the [`Gate`]: a write,
//! a read or a listing outside the workspace, and every command. What the gate
//! returns decides what happens next, and a `deny` is an answer, not a failure:
//! it comes back as the tool result and the workspace is as it was.
//!
//! A write is atomic. The bytes go to a temporary file in the destination
//! directory and are renamed over the target, so a reader either sees the file
//! as it was or the file as it will be, and a write that dies halfway leaves no
//! half file behind.
//!
//! Nothing here talks to a model. A turn calls these functions, writes what
//! they return into the log as tool events, and hands the summary back as the
//! tool result.

use std::ffi::{CString, OsStr};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, PermissionsExt},
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command as TokioCommand;

pub type OutputSink = Arc<dyn Fn(bool, &[u8]) + Send + Sync>;

use crate::events::PermissionBody;
use crate::permit::{self, Gate, GateError, Verdict};
use crate::schedule::{Clock, Schedules};
use crate::session::{Session, SessionError};
use crate::task::{Tasks, MAX_TASK_TIMEOUT_SEC};

mod file_read;
mod image_generation;
mod preparation;

pub use file_read::ReadOptions;
pub use image_generation::ImageGeneration;

#[derive(Debug)]
struct Approval {
    body: PermissionBody,
    verdict: Verdict,
}

/// How much of a file one read returns.
pub const READ_LIMIT: usize = 32 * 1024;
pub const GREP_LIMIT: usize = 200;
/// How many names one listing returns.
pub const LIST_LIMIT: usize = 500;
/// The largest file one write may propose.
pub const WRITE_LIMIT: usize = 256 * 1024;
/// How much of a command's output comes back to the caller, for each stream.
pub const OUTPUT_LIMIT: usize = 64 * 1024;
/// How long a command runs when the caller names no timeout.
pub const DEFAULT_TIMEOUT_SEC: u64 = 120;
/// The longest timeout a caller may ask for.
pub const MAX_TIMEOUT_SEC: u64 = 600;
/// How long the pipes of a finished command are given to close on their own. A
/// command that leaves a grandchild holding its output should not hold the
/// turn open forever.
const PIPE_GRACE_SEC: u64 = 5;

/// A tool that could not do its job.
#[derive(Debug)]
pub enum ToolError {
    /// A path the caller meant to be inside the workspace resolved outside it.
    Outside {
        path: PathBuf,
    },
    /// A file was written, read, or listed where a file cannot be.
    NotAFile {
        path: PathBuf,
    },
    /// The write has no file name to replace.
    NoFileName {
        path: PathBuf,
    },
    /// The directory a write would land in is not there.
    NoDirectory {
        path: PathBuf,
    },
    /// The write is larger than a write may be.
    TooLarge {
        path: PathBuf,
        bytes: usize,
        limit: usize,
    },
    EmptyOldString,
    NoMatch {
        path: PathBuf,
    },
    ManyMatches {
        path: PathBuf,
        count: usize,
    },
    BadLine {
        line: u64,
    },
    BadLimit {
        limit: u64,
    },
    BadPattern {
        message: String,
    },
    /// There is nothing to run.
    NoCommand,
    /// A timeout of zero is no timeout to wait for, and anything past
    /// [`MAX_TIMEOUT_SEC`] is a turn nobody is waiting for.
    BadTimeout {
        secs: u64,
    },
    /// A background command asked for a timeout outside 1 to 86400 seconds.
    BadTaskTimeout {
        secs: u64,
    },
    /// A file could not be read or written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A command could not be started at all.
    Spawn {
        program: String,
        source: std::io::Error,
    },
    /// The log could not be written, or the card could not be answered.
    Gate(GateError),
    Web(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::Outside { path } => {
                write!(f, "{} resolves outside the workspace", path.display())
            }
            ToolError::NotAFile { path } => write!(f, "{}: not a file", path.display()),
            ToolError::NoFileName { path } => write!(f, "{}: no file name", path.display()),
            ToolError::NoDirectory { path } => {
                write!(f, "{}: no such directory", path.display())
            }
            ToolError::TooLarge { path, bytes, limit } => write!(
                f,
                "{}: {bytes} bytes is over the {limit} byte limit",
                path.display()
            ),
            ToolError::EmptyOldString => write!(f, "old_string is empty"),
            ToolError::NoMatch { path } => {
                write!(f, "{}: old_string was not found", path.display())
            }
            ToolError::ManyMatches { path, count } => {
                write!(f, "{}: old_string appears {count} times", path.display())
            }
            ToolError::BadLine { line } => {
                write!(f, "line {line} is not a line number: use 1 or more")
            }
            ToolError::BadLimit { limit } => {
                write!(f, "limit {limit} is not a line count: use 1 or more")
            }
            ToolError::BadPattern { message } => write!(f, "grep: {message}"),
            ToolError::NoCommand => write!(f, "no command to run"),
            ToolError::BadTimeout { secs } => write!(
                f,
                "{secs} seconds is not a timeout: use 1 to {MAX_TIMEOUT_SEC}"
            ),
            ToolError::BadTaskTimeout { secs } => write!(
                f,
                "{secs} seconds is not a timeout: use 1 to {MAX_TASK_TIMEOUT_SEC}"
            ),
            ToolError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            ToolError::Spawn { program, source } => {
                write!(f, "could not start {program}: {source}")
            }
            ToolError::Gate(source) => write!(f, "{source}"),
            ToolError::Web(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<GateError> for ToolError {
    fn from(source: GateError) -> ToolError {
        ToolError::Gate(source)
    }
}

impl From<crate::web::Error> for ToolError {
    fn from(source: crate::web::Error) -> ToolError {
        ToolError::Web(source.to_string())
    }
}

impl From<SessionError> for ToolError {
    fn from(source: SessionError) -> ToolError {
        match source {
            SessionError::Io { path, source } => ToolError::Io { path, source },
            other => ToolError::Gate(GateError::Session(other)),
        }
    }
}

/// The tools of one session, held against its workspace.
///
/// Cheap to make and cheap to clone the gate out of, because a turn holds one
/// and the server's answer route holds another, and both have to be stopping on
/// the same card.
#[derive(Clone, Debug)]
pub struct Tools {
    /// The workspace, resolved. Every relative path is against it and every
    /// resolved path is compared with it.
    workspace: PathBuf,
    gate: Gate,
    tasks: Tasks,
    schedules: Schedules,
    approval: Option<Arc<Mutex<Option<Approval>>>>,
}

impl Tools {
    /// The tools for `session`, against the workspace its meta names.
    pub fn at(session: &Session) -> Result<Tools, ToolError> {
        Self::with_clock(session, Clock::system())
    }

    pub fn with_clock(session: &Session, clock: Clock) -> Result<Tools, ToolError> {
        let meta = session.meta()?;
        let workspace = resolve(Path::new(&meta.workspace)).map_err(|source| ToolError::Io {
            path: PathBuf::from(&meta.workspace),
            source,
        })?;
        Ok(Tools {
            workspace,
            gate: Gate::at(session),
            tasks: Tasks::at(session)?,
            schedules: Schedules::at(session, clock),
            approval: None,
        })
    }

    pub fn tasks(&self) -> &Tasks {
        &self.tasks
    }

    pub fn schedules(&self) -> &Schedules {
        &self.schedules
    }

    /// The workspace these tools are held to.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The gate every write and every command stops at.
    pub fn gate(&self) -> &Gate {
        &self.gate
    }

    /// The session the log is written to.
    pub fn session(&self) -> &Session {
        self.gate.session()
    }

    pub fn attach_artifact(
        &self,
        turn_id: &str,
        path: &str,
    ) -> Result<Option<crate::proof::ProofFile>, ToolError> {
        let target = self.target(path)?;
        let name = display(&target.absolute);
        if !target.inside && !self.allowed_outside_read(&name)? {
            let verdict = self.ask(
                turn_id,
                &PermissionBody {
                    action: format!("Attach artifact from {name}"),
                    path: Some(name.clone()),
                    ..PermissionBody::default()
                },
            )?;
            if !verdict.allowed() {
                return Ok(None);
            }
            if self.remember_session(&verdict) {
                self.gate.remember_outside_read(&name)?;
            }
        }
        let current = self.target(path)?;
        if current.absolute != target.absolute {
            return Err(ToolError::Outside {
                path: current.absolute,
            });
        }
        let filename = target
            .absolute
            .file_name()
            .ok_or_else(|| ToolError::NoFileName {
                path: target.absolute.clone(),
            })?;
        let parent = write_parent(&target.absolute)?;
        let file =
            open_at(&parent, filename, libc::O_RDONLY | libc::O_NONBLOCK, 0).map_err(|source| {
                ToolError::Io {
                    path: target.absolute.clone(),
                    source,
                }
            })?;
        crate::proof::store_file(self.session(), &filename.to_string_lossy(), file)
            .map(Some)
            .map_err(|source| ToolError::Io {
                path: target.absolute,
                source,
            })
    }

    /// Read up to [`READ_LIMIT`] bytes of a file from `offset`.
    ///
    /// Inside the workspace this runs and the turn keeps going. Outside it, the
    /// card names the absolute path and the read waits for the answer. When
    /// there is more file than this returns, the result carries the offset the
    /// next read starts at.
    pub fn read_file(
        &self,
        turn_id: &str,
        path: &str,
        offset: Option<u64>,
        line: Option<u64>,
        limit: Option<u64>,
    ) -> Result<ReadFile, ToolError> {
        self.read_file_with_options(
            turn_id,
            path,
            ReadOptions {
                offset,
                line,
                limit,
                ..ReadOptions::default()
            },
        )
    }

    pub fn read_file_with_options(
        &self,
        turn_id: &str,
        path: &str,
        options: ReadOptions,
    ) -> Result<ReadFile, ToolError> {
        let target = self.target(path)?;
        let name = display(&target.absolute);
        if !target.inside && !self.allowed_outside_read(&name)? {
            let verdict = self.ask(
                turn_id,
                &PermissionBody {
                    action: format!("Read {name}"),
                    path: Some(name.clone()),
                    ..PermissionBody::default()
                },
            )?;
            if !verdict.allowed() {
                return Ok(ReadFile::denied(&name));
            }
            if self.remember_session(&verdict) {
                self.gate.remember_outside_read(&name)?;
            }
        }

        let mut file = open(&target.absolute)?;
        if let Some(result) = file_read::read_document(&mut file, &target.absolute, &options)? {
            return Ok(result);
        }
        let ReadOptions {
            offset,
            line,
            limit,
            ..
        } = options;
        if let Some(line) = line.or_else(|| offset.is_none().then_some(1)) {
            let (text, next_line) = read_line_slice(&target.absolute, line, limit)?;
            return Ok(ReadFile {
                path: name,
                text,
                next_offset: None,
                next_line,
                denied: false,
                images: Vec::new(),
            });
        }

        let offset = offset.unwrap_or(0);
        let total = file
            .metadata()
            .map(|meta| meta.len())
            .map_err(|source| ToolError::Io {
                path: target.absolute.clone(),
                source,
            })?;
        if offset > total {
            return Err(ToolError::Io {
                path: target.absolute.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("offset {offset} is past the end of {name}"),
                ),
            });
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| ToolError::Io {
                path: target.absolute.clone(),
                source,
            })?;
        // One byte past the limit is how the tool knows there is more file.
        let mut bytes = vec![0u8; READ_LIMIT + 1];
        let read = fill(&mut file, &mut bytes).map_err(|source| ToolError::Io {
            path: target.absolute.clone(),
            source,
        })?;
        let more = read > READ_LIMIT;
        bytes.truncate(read.min(READ_LIMIT));
        let (text, consumed) = file_read::decode_text(&bytes, more, &target.absolute)?;
        Ok(ReadFile {
            path: name,
            text,
            next_offset: if more {
                Some(offset + consumed as u64)
            } else {
                None
            },
            next_line: None,
            denied: false,
            images: Vec::new(),
        })
    }

    pub fn read_workspace_file(&self, path: &str) -> Result<WorkspaceFile, ToolError> {
        let target = self.target(path)?;
        if !target.inside {
            return Err(ToolError::Outside {
                path: target.absolute,
            });
        }
        let mut file = open(&target.absolute)?;
        let mut bytes = vec![0u8; READ_LIMIT + 1];
        let read = fill(&mut file, &mut bytes).map_err(|source| ToolError::Io {
            path: target.absolute.clone(),
            source,
        })?;
        let truncated = read > READ_LIMIT;
        bytes.truncate(read.min(READ_LIMIT));
        let text = String::from_utf8(bytes).map_err(|_| ToolError::Io {
            path: target.absolute.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, "the file is not text"),
        })?;
        Ok(WorkspaceFile {
            path: path.to_string(),
            text,
            truncated,
        })
    }

    /// One level of names under a directory, at most [`LIST_LIMIT`] of them.
    ///
    /// A listing does not descend and does not follow anything: a name that is
    /// a link says so, rather than quietly reading through it.
    pub fn list_dir(&self, turn_id: &str, path: &str) -> Result<ListDir, ToolError> {
        let target = self.target(path)?;
        let name = display(&target.absolute);
        if !target.inside && !self.allowed_outside_read(&name)? {
            let verdict = self.ask(
                turn_id,
                &PermissionBody {
                    action: format!("List {name}"),
                    path: Some(name.clone()),
                    ..PermissionBody::default()
                },
            )?;
            if !verdict.allowed() {
                return Ok(ListDir::denied(&name));
            }
            if self.remember_session(&verdict) {
                self.gate.remember_outside_read(&name)?;
            }
        }

        let reader = fs::read_dir(&target.absolute).map_err(|source| ToolError::Io {
            path: target.absolute.clone(),
            source,
        })?;
        let mut entries: Vec<DirEntry> = Vec::new();
        let mut more = false;
        for entry in reader {
            let entry = entry.map_err(|source| ToolError::Io {
                path: target.absolute.clone(),
                source,
            })?;
            if entries.len() == LIST_LIMIT {
                more = true;
                break;
            }
            entries.push(DirEntry::of(&entry).map_err(|source| ToolError::Io {
                path: entry.path(),
                source,
            })?);
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(ListDir {
            path: name,
            entries,
            truncated: more,
            denied: false,
        })
    }

    /// Replace a file with `contents`, at most [`WRITE_LIMIT`] of them.
    ///
    /// The whole file is proposed every time: a write is a replace, so there
    /// is no patch to get wrong. The card carries the diff against what is on
    /// disk, and the answer is checked against a digest of those bytes. If the
    /// file changed while the card was up, the answer was about a file that is
    /// no longer there, and the write asks again with the diff the file has
    /// now.
    pub fn write_file(
        &self,
        turn_id: &str,
        path: &str,
        contents: &str,
    ) -> Result<WriteFile, ToolError> {
        self.write_binary(turn_id, path, contents.as_bytes(), WRITE_LIMIT)
    }

    fn write_binary(
        &self,
        turn_id: &str,
        path: &str,
        new: &[u8],
        limit: usize,
    ) -> Result<WriteFile, ToolError> {
        let target = self.target(path)?;
        let name = display(&target.absolute);
        if new.len() > limit {
            return Err(ToolError::TooLarge {
                path: target.absolute.clone(),
                bytes: new.len(),
                limit,
            });
        }
        let parent = write_parent(&target.absolute)?;
        let file_name = target
            .absolute
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| ToolError::NoFileName {
                path: target.absolute.clone(),
            })?;

        // The card is asked again when the file moved under it, so this is a
        // loop rather than a straight line. `remember` is what an `allow_session`
        // answer is owed, and it is kept back until the write that was allowed
        // has actually happened: an answer about a file that has since changed
        // is not an answer about this one.
        let mut remember = false;
        loop {
            let (old, created) = write_bytes(&parent, &target.absolute)?;
            if !self.allowed_write(&name)? {
                let action = if created {
                    format!("Create {file_name}")
                } else {
                    format!("Replace {file_name}")
                };
                let verdict = self.ask(
                    turn_id,
                    &permit::write_permission(&action, &name, &old, new),
                )?;
                if !verdict.allowed() {
                    return Ok(WriteFile {
                        path: name,
                        bytes: new.len(),
                        created: false,
                        replaced: false,
                        denied: true,
                    });
                }
                remember |= self.remember_session(&verdict);
            }
            // The answer was about the bytes that were on disk. Anything else
            // now means somebody wrote the file while the card was up, so that
            // answer is spent and the write asks again with the diff the file
            // has now.
            let current = self.target(path)?;
            let current_parent = write_parent(&current.absolute)?;
            let before = parent.metadata().map_err(|source| ToolError::Io {
                path: target.absolute.clone(),
                source,
            })?;
            let after = current_parent.metadata().map_err(|source| ToolError::Io {
                path: current.absolute.clone(),
                source,
            })?;
            if current.absolute != target.absolute
                || before.dev() != after.dev()
                || before.ino() != after.ino()
            {
                return Err(ToolError::Io {
                    path: target.absolute.clone(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "the write target changed while awaiting permission",
                    ),
                });
            }
            if permit::fingerprint(&write_bytes(&parent, &target.absolute)?.0)
                != permit::fingerprint(&old)
            {
                // The answer was about bytes that are no longer there, so it is
                // spent, and an `allow_session` it carried is spent with it.
                remember = false;
                continue;
            }
            write_atomically(&parent, &target.absolute, new)?;
            if remember {
                self.gate.remember_write(&name)?;
            }
            return Ok(WriteFile {
                path: name,
                bytes: new.len(),
                created,
                replaced: !created,
                denied: false,
            });
        }
    }

    pub fn search_replace(
        &self,
        turn_id: &str,
        path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
    ) -> Result<WriteFile, ToolError> {
        let contents = self.replacement(path, old_string, new_string, replace_all)?;
        self.write_file(turn_id, path, &contents)
    }

    fn replacement(
        &self,
        path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
    ) -> Result<String, ToolError> {
        if old_string.is_empty() {
            return Err(ToolError::EmptyOldString);
        }
        let target = self.target(path)?;
        if !target.inside {
            return Err(ToolError::Outside {
                path: target.absolute,
            });
        }
        if !exists(&target.absolute) {
            return Err(ToolError::Io {
                path: target.absolute,
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            });
        }
        let old = self.current_bytes(&target.absolute)?;
        let text = String::from_utf8(old).map_err(|_| ToolError::Io {
            path: target.absolute.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, "the file is not text"),
        })?;
        let contents = match replace_once(&text, old_string, new_string, replace_all) {
            Ok(contents) => contents,
            Err(ReplaceMiss::None) => {
                return Err(ToolError::NoMatch {
                    path: target.absolute,
                })
            }
            Err(ReplaceMiss::Many(count)) => {
                return Err(ToolError::ManyMatches {
                    path: target.absolute,
                    count,
                })
            }
        };
        Ok(contents)
    }

    pub fn grep(
        &self,
        pattern: &str,
        path: Option<&str>,
        glob: Option<&str>,
        head_limit: Option<usize>,
    ) -> Result<Grep, ToolError> {
        if pattern.is_empty() {
            return Err(ToolError::BadPattern {
                message: "pattern is empty".into(),
            });
        }
        let limit = match head_limit {
            Some(0) | None => GREP_LIMIT,
            Some(limit) => limit,
        };
        let root = match path {
            None | Some("") | Some(".") => self.workspace.clone(),
            Some(path) => {
                let target = self.target(path)?;
                if !target.inside {
                    return Err(ToolError::Outside {
                        path: target.absolute,
                    });
                }
                target.absolute
            }
        };
        let cap = limit.saturating_add(1);
        let hits = match grep_rg(&self.workspace, &root, pattern, glob, cap)? {
            Some(hits) => hits,
            None => grep_walk(&self.workspace, &root, pattern, glob, cap)?,
        };
        Ok(Grep::from_hits(hits, limit))
    }

    /// Run a command in the workspace.
    ///
    /// The argv is the whole command: no shell, no word splitting, and nothing
    /// the caller cannot see. The working directory is the workspace and the
    /// environment is this process's own, so a command finds the same tools and
    /// the same tokens the server has. The timeout defaults to
    /// [`DEFAULT_TIMEOUT_SEC`] and may be raised to [`MAX_TIMEOUT_SEC`]; a
    /// timeout that is not the default is part of what the card names, because
    /// it changes what the command may do. At the timeout the process is
    /// killed. Stopping a turn is not this function's job.
    pub fn run(
        &self,
        turn_id: &str,
        argv: &[String],
        timeout_sec: Option<u64>,
    ) -> Result<RunOutput, ToolError> {
        if !self.run_allowed(turn_id, argv, timeout_sec)? {
            return Ok(RunOutput::denied(argv));
        }
        let timeout = timeout_sec.unwrap_or(DEFAULT_TIMEOUT_SEC);
        let output = self.execute(argv, timeout)?;
        self.note_pull(argv, &output);
        Ok(output)
    }

    pub fn is_gh_pr_create(argv: &[String]) -> bool {
        let Some(program) = argv
            .first()
            .and_then(|program| Path::new(program).file_name())
            .and_then(OsStr::to_str)
        else {
            return false;
        };
        if program == "gh" {
            return argv.windows(2).any(|pair| pair == ["pr", "create"]);
        }
        if matches!(program, "sh" | "bash" | "zsh" | "dash") {
            if let Some(command) = argv
                .windows(2)
                .find(|pair| {
                    pair[0].starts_with('-') && !pair[0].starts_with("--") && pair[0].contains('c')
                })
                .map(|pair| &pair[1])
            {
                let words: Vec<_> = command
                    .split(|ch: char| ch.is_whitespace() || ";|&()\"'".contains(ch))
                    .filter(|word| !word.is_empty())
                    .collect();
                return words.windows(3).any(|words| {
                    Path::new(words[0]).file_name() == Some(OsStr::new("gh"))
                        && words[1..] == ["pr", "create"]
                });
            }
        }
        false
    }

    fn note_pull(&self, argv: &[String], output: &RunOutput) {
        if !Self::is_gh_pr_create(argv) {
            return;
        }
        let Some(url) = crate::session::github_pull_url(&output.stdout)
            .or_else(|| crate::session::github_pull_url(&output.stderr))
        else {
            return;
        };
        let _ = self.session().set_pull_url(&url);
    }

    /// Whether this argv may run, asking the gate if it has to.
    ///
    /// This is the gate half of [`Tools::run`], split out so the turn loop can
    /// ask on a blocking thread and then run the command as a cancellable
    /// future. A denied command is `Ok(false)`: the tool result is the denial
    /// and the command never starts.
    pub fn run_allowed(
        &self,
        turn_id: &str,
        argv: &[String],
        timeout_sec: Option<u64>,
    ) -> Result<bool, ToolError> {
        let Some(program) = argv.first() else {
            return Err(ToolError::NoCommand);
        };
        let timeout = timeout_sec.unwrap_or(DEFAULT_TIMEOUT_SEC);
        if timeout == 0 || timeout > MAX_TIMEOUT_SEC {
            return Err(ToolError::BadTimeout { secs: timeout });
        }
        if !self.allowed_argv(argv)? {
            let body = PermissionBody {
                action: format!("Run {program}"),
                argv: Some(argv.to_vec()),
                timeout_sec: match timeout_sec {
                    // A card should not name a number nobody changed.
                    Some(secs) if secs != DEFAULT_TIMEOUT_SEC => Some(secs),
                    _ => None,
                },
                ..PermissionBody::default()
            };
            let verdict = self.ask(turn_id, &body)?;
            if !verdict.allowed() {
                return Ok(false);
            }
            if self.remember_session(&verdict) {
                self.gate.remember_argv(argv)?;
            }
        }
        Ok(true)
    }

    pub fn start_task_allowed(
        &self,
        turn_id: &str,
        argv: &[String],
        timeout_sec: Option<u64>,
    ) -> Result<bool, ToolError> {
        if argv.is_empty() {
            return Err(ToolError::NoCommand);
        }
        if let Some(timeout) = timeout_sec {
            if timeout == 0 || timeout > MAX_TASK_TIMEOUT_SEC {
                return Err(ToolError::BadTaskTimeout { secs: timeout });
            }
        }
        if !self.allowed_argv(argv)? {
            let body = PermissionBody {
                action: format!("Start {}", argv.join(" ")),
                argv: Some(argv.to_vec()),
                timeout_sec,
                ..PermissionBody::default()
            };
            let verdict = self.ask(turn_id, &body)?;
            if !verdict.allowed() {
                return Ok(false);
            }
            if self.remember_session(&verdict) {
                self.gate.remember_argv(argv)?;
            }
        }
        Ok(true)
    }

    pub fn run_closeout_allowed(
        &self,
        turn_id: &str,
        id: &str,
        argv: &[String],
    ) -> Result<bool, ToolError> {
        if !self.allowed_argv(argv)? {
            let body = PermissionBody {
                action: format!("Run closeout {id}"),
                argv: Some(argv.to_vec()),
                ..PermissionBody::default()
            };
            let verdict = self.ask(turn_id, &body)?;
            if !verdict.allowed() {
                return Ok(false);
            }
            if self.remember_session(&verdict) {
                self.gate.remember_argv(argv)?;
            }
        }
        Ok(true)
    }

    /// Run a command as a future the turn loop can cancel.
    ///
    /// The command runs in the workspace with this process's environment, and
    /// its output is capped at [`OUTPUT_LIMIT`] per stream. The timeout kills
    /// it. When `cancel` fires first, the process gets SIGTERM and then SIGKILL
    /// two seconds later if it has not exited: a cancelled command is stopped,
    /// not left running beside a turn that has ended.
    pub async fn execute_cancellable(
        &self,
        argv: &[String],
        timeout_sec: Option<u64>,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<RunOutput, ToolError> {
        self.execute_streaming(argv, timeout_sec, cancel, None)
            .await
    }

    pub async fn execute_streaming(
        &self,
        argv: &[String],
        timeout_sec: Option<u64>,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
        output: Option<OutputSink>,
    ) -> Result<RunOutput, ToolError> {
        if *cancel.borrow() {
            return Ok(RunOutput {
                argv: argv.to_vec(),
                exit: None,
                stdout: String::new(),
                stderr: "Cancelled before starting.".to_string(),
                timed_out: false,
                truncated: false,
                denied: false,
            });
        }
        let program = argv[0].clone();
        let timeout = timeout_sec.unwrap_or(DEFAULT_TIMEOUT_SEC);
        let wait_error = || ToolError::Spawn {
            program: program.clone(),
            source: std::io::Error::other("the command waiter did not report"),
        };

        let mut child = TokioCommand::new(&program)
            .process_group(0)
            .args(&argv[1..])
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| ToolError::Spawn {
                program: program.clone(),
                source,
            })?;

        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .map(|pipe| tokio::spawn(drain_async(pipe, output.clone(), false)));
        let stderr = child
            .stderr
            .take()
            .map(|pipe| tokio::spawn(drain_async(pipe, output.clone(), true)));

        // The child is waited for on its own task, so the select below can
        // race its completion against the cancel signal and the timeout
        // without borrowing the child.
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let status = child.wait().await.ok();
            let _ = done_tx.send(status);
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
        let mut timed_out = false;

        let status = tokio::select! {
            result = &mut done_rx => result.ok().flatten(),
            _ = cancel.changed() => {
                signal_group(pid, libc::SIGTERM);
                let result = tokio::time::timeout(Duration::from_secs(2), &mut done_rx).await;
                signal_group(pid, libc::SIGKILL);
                match result {
                    Ok(result) => result.ok().flatten(),
                    Err(_) => tokio::time::timeout(Duration::from_secs(2), &mut done_rx)
                        .await.ok().and_then(Result::ok).flatten(),
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                timed_out = true;
                signal_group(pid, libc::SIGKILL);
                tokio::time::timeout(Duration::from_secs(2), &mut done_rx)
                    .await.ok().and_then(Result::ok).flatten()
            }
        };
        let stdout = collect_async(stdout, pid).await;
        let stderr = collect_async(stderr, pid).await;
        let status = status.ok_or_else(wait_error)?;
        let output = RunOutput {
            argv: argv.to_vec(),
            exit: status.code(),
            stdout: stdout.text,
            stderr: stderr.text,
            timed_out,
            truncated: stdout.total > OUTPUT_LIMIT || stderr.total > OUTPUT_LIMIT,
            denied: false,
        };
        self.note_pull(argv, &output);
        Ok(output)
    }

    /// Start the command and wait for it, with the timeout on the clock.
    fn execute(&self, argv: &[String], timeout: u64) -> Result<RunOutput, ToolError> {
        let program = argv[0].clone();
        let mut child = Command::new(&program)
            .args(&argv[1..])
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| ToolError::Spawn {
                program: program.clone(),
                source,
            })?;

        // The pipes are drained on their own threads, so a command that fills
        // one of them cannot stop being waited for.
        let stdout = child.stdout.take().map(drain);
        let stderr = child.stderr.take().map(drain);
        let deadline = Instant::now() + Duration::from_secs(timeout);
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    timed_out = true;
                    let _ = child.kill();
                    break child
                        .wait()
                        .map_err(|source| ToolError::Spawn { program, source })?;
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(source) => {
                    let _ = child.kill();
                    return Err(ToolError::Spawn { program, source });
                }
            }
        };
        let stdout = stdout.map(collect).unwrap_or_default();
        let stderr = stderr.map(collect).unwrap_or_default();
        Ok(RunOutput {
            argv: argv.to_vec(),
            exit: status.code(),
            stdout: stdout.text,
            stderr: stderr.text,
            timed_out,
            truncated: stdout.total > OUTPUT_LIMIT || stderr.total > OUTPUT_LIMIT,
            denied: false,
        })
    }

    /// Ask the gate, with the tool's own error on the other side.
    fn ask(&self, turn_id: &str, body: &PermissionBody) -> Result<Verdict, ToolError> {
        if let Some(approval) = &self.approval {
            if let Some(approval) = approval.lock().expect("tool approval").take() {
                if approval.body == *body {
                    return Ok(approval.verdict);
                }
            }
        }
        Ok(self.gate.ask(turn_id, body)?)
    }

    /// Whether the answer was `allow_session`, which is the only answer that
    /// changes what the next call may do.
    fn remember_session(&self, verdict: &Verdict) -> bool {
        verdict.decision == crate::events::Decision::AllowSession
    }

    pub fn web_fetch(
        &self,
        turn_id: &str,
        url: &str,
        max_bytes: Option<u64>,
    ) -> Result<Fetched, ToolError> {
        self.web_fetch_with(turn_id, url, max_bytes, &crate::web::production())
    }

    pub(crate) fn web_fetch_with(
        &self,
        turn_id: &str,
        url: &str,
        max_bytes: Option<u64>,
        cfg: &crate::web::Config,
    ) -> Result<Fetched, ToolError> {
        let max_bytes = crate::web::cap(max_bytes)?;
        let parsed = crate::web::parse(url)?;
        crate::web::ensure_public(&parsed, cfg)?;
        let origin = crate::web::origin(&parsed)?;
        if !self.allowed_fetch(&origin)? {
            let verdict = self.ask(
                turn_id,
                &PermissionBody {
                    action: format!("Fetch {url}"),
                    path: Some(url.to_string()),
                    ..PermissionBody::default()
                },
            )?;
            if !verdict.allowed() {
                return Ok(Fetched::denied(url));
            }
            if self.remember_session(&verdict) {
                self.gate.remember_fetch(&origin)?;
            }
        }
        let page = crate::web::fetch(&parsed, max_bytes, cfg)?;
        Ok(Fetched {
            url: page.final_url,
            text: page.text,
            denied: false,
        })
    }

    pub fn web_search(
        &self,
        turn_id: &str,
        query: &str,
        num_results: Option<u64>,
        exa_env: &str,
        firecrawl_env: &str,
    ) -> Result<SearchOutcome, ToolError> {
        self.web_search_with(
            turn_id,
            query,
            num_results,
            exa_env,
            firecrawl_env,
            &crate::web::SearchEndpoints::production(),
        )
    }

    pub(crate) fn web_search_with(
        &self,
        turn_id: &str,
        query: &str,
        num_results: Option<u64>,
        exa_env: &str,
        firecrawl_env: &str,
        endpoints: &crate::web::SearchEndpoints,
    ) -> Result<SearchOutcome, ToolError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(ToolError::Web("web_search needs a query.".into()));
        }
        if !self.allowed_search()? {
            let verdict = self.ask(
                turn_id,
                &PermissionBody {
                    action: "Search the web".to_string(),
                    path: Some(query.to_string()),
                    ..PermissionBody::default()
                },
            )?;
            if !verdict.allowed() {
                return Ok(SearchOutcome::denied(query));
            }
            if self.remember_session(&verdict) {
                self.gate.remember_search()?;
            }
        }
        let Some((vendor, key)) =
            crate::web::pick_search(exa_env, firecrawl_env, |name| std::env::var(name).ok())
        else {
            return Err(ToolError::Web("No search key is set.".into()));
        };
        let endpoint = match vendor {
            crate::web::Vendor::Exa => endpoints.exa.clone(),
            crate::web::Vendor::Firecrawl => endpoints.firecrawl.clone(),
        };
        let text = crate::web::search(
            vendor,
            &key,
            &endpoint,
            endpoints.allow_loopback,
            query,
            crate::web::clamp_results(num_results),
        )?;
        Ok(SearchOutcome {
            text,
            denied: false,
        })
    }

    fn allowed_fetch(&self, origin: &str) -> Result<bool, ToolError> {
        Ok(self.gate.meta()?.allow.allows_fetch(origin))
    }

    fn allowed_search(&self) -> Result<bool, ToolError> {
        Ok(self.gate.meta()?.allow.allows_search())
    }

    fn allowed_write(&self, path: &str) -> Result<bool, ToolError> {
        Ok(self.gate.meta()?.allow.allows_write(path))
    }

    fn allowed_outside_read(&self, path: &str) -> Result<bool, ToolError> {
        Ok(self.gate.meta()?.allow.allows_outside_read(path))
    }

    fn allowed_argv(&self, argv: &[String]) -> Result<bool, ToolError> {
        Ok(self.gate.meta()?.allow.allows_argv(argv))
    }

    /// The bytes of a file as they are now, and empty for a file that is not
    /// there yet.
    fn current_bytes(&self, path: &Path) -> Result<Vec<u8>, ToolError> {
        match fs::metadata(path) {
            Ok(meta) if meta.is_dir() => Err(ToolError::NotAFile {
                path: path.to_path_buf(),
            }),
            Ok(_) => fs::read(path).map_err(|source| ToolError::Io {
                path: path.to_path_buf(),
                source,
            }),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(source) => Err(ToolError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Where a path argument landed, and whether the caller meant it to stay
    /// inside the workspace.
    ///
    /// A relative path, or an absolute one that names something in the
    /// workspace, has to end up in the workspace. That is what refuses a
    /// symlink pointing out of it. An absolute path somewhere else is a
    /// deliberate reach, and the gate asks about it by that absolute path.
    fn target(&self, path: &str) -> Result<Target, ToolError> {
        let asked = Path::new(path);
        let joined = if asked.is_absolute() {
            asked.to_path_buf()
        } else {
            self.workspace.join(asked)
        };
        let asked_inside = !asked.is_absolute() || joined.starts_with(&self.workspace);
        let absolute = resolve(&joined).map_err(|source| ToolError::Io {
            path: joined.clone(),
            source,
        })?;
        let inside = absolute.starts_with(&self.workspace);
        if asked_inside && !inside {
            return Err(ToolError::Outside { path: absolute });
        }
        Ok(Target { absolute, inside })
    }
}

/// A path that resolved, and whether it landed inside the workspace.
struct Target {
    absolute: PathBuf,
    inside: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkspaceFile {
    pub path: String,
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fetched {
    pub url: String,
    pub text: String,
    pub denied: bool,
}

impl Fetched {
    fn denied(url: &str) -> Fetched {
        Fetched {
            url: url.to_string(),
            text: format!("Not allowed, so {url} was not fetched."),
            denied: true,
        }
    }

    pub fn summary(&self) -> String {
        self.text.clone()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchOutcome {
    pub text: String,
    pub denied: bool,
}

impl SearchOutcome {
    fn denied(query: &str) -> SearchOutcome {
        SearchOutcome {
            text: format!("Not allowed, so {query} was not searched."),
            denied: true,
        }
    }

    pub fn summary(&self) -> String {
        self.text.clone()
    }
}

/// What [`Tools::read_file`] returns.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReadFile {
    /// The absolute path that was read.
    pub path: String,
    /// The bytes, as text. A file that is not text reads as far as `from_utf8`
    /// can, which is a lossy answer and not a wrong one.
    pub text: String,
    /// The offset the next read starts at, when there is more file.
    pub next_offset: Option<u64>,
    pub next_line: Option<u64>,
    /// Whether the user said no. The bytes are empty and nothing was read.
    pub denied: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::attachment::ImageAttachment>,
}

impl ReadFile {
    fn denied(path: &str) -> ReadFile {
        ReadFile {
            path: path.to_string(),
            text: String::new(),
            next_offset: None,
            next_line: None,
            denied: true,
            images: Vec::new(),
        }
    }

    /// The tool result the model reads.
    pub fn summary(&self) -> String {
        if self.denied {
            return format!("Not allowed, so {} was not read.", self.path);
        }
        if let Some(next) = self.next_line {
            return format!(
                "{}\n\nRead {} again with line {next} for the rest.",
                self.text, self.path
            );
        }
        match self.next_offset {
            Some(next) => format!(
                "{}\n\nThat is the first {READ_LIMIT} bytes. Read {} again with offset {next} \
                 for the rest.",
                self.text, self.path
            ),
            None => self.text.clone(),
        }
    }
}

/// One name under a listed directory.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    /// The size, for a file. Zero for anything else.
    pub bytes: u64,
}

/// What a name is. A link is a link: a listing says so rather than reading
/// through it and reporting whatever it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Link,
    Other,
}

/// What [`Tools::list_dir`] returns.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ListDir {
    /// The absolute path that was listed.
    pub path: String,
    pub entries: Vec<DirEntry>,
    /// Whether there were more names than [`LIST_LIMIT`].
    pub truncated: bool,
    pub denied: bool,
}

impl ListDir {
    fn denied(path: &str) -> ListDir {
        ListDir {
            path: path.to_string(),
            entries: Vec::new(),
            truncated: false,
            denied: true,
        }
    }

    /// The tool result the model reads: one name per line.
    pub fn summary(&self) -> String {
        if self.denied {
            return format!("Not allowed, so {} was not listed.", self.path);
        }
        let mut out = self
            .entries
            .iter()
            .map(|entry| match entry.kind {
                EntryKind::File => format!("{} ({} bytes)", entry.name, entry.bytes),
                EntryKind::Dir => format!("{}/", entry.name),
                EntryKind::Link => format!("{} (link)", entry.name),
                EntryKind::Other => entry.name.clone(),
            })
            .collect::<Vec<String>>()
            .join("\n");
        if self.truncated {
            out.push_str(&format!(
                "\n\nThat is the first {LIST_LIMIT} names. This tool reads one level and no \
                 deeper."
            ));
        }
        out
    }
}

/// What [`Tools::write_file`] returns.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WriteFile {
    /// The absolute path that was written.
    pub path: String,
    /// How many bytes went in.
    pub bytes: usize,
    /// The file was not there before.
    pub created: bool,
    /// The file was there and is now something else.
    pub replaced: bool,
    /// Whether the user said no. The file is as it was.
    pub denied: bool,
}

impl WriteFile {
    /// The tool result the model reads.
    pub fn summary(&self) -> String {
        if self.denied {
            return format!("Not allowed, so {} was not written.", self.path);
        }
        let verb = if self.created { "Created" } else { "Replaced" };
        format!("{verb} {} with {} bytes.", self.path, self.bytes)
    }
}

/// What [`Tools::run`] returns.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunOutput {
    /// The command, as it was run.
    pub argv: Vec<String>,
    /// Its exit code, or nothing when a signal ended it.
    pub exit: Option<i32>,
    /// The end of its standard output, at most [`OUTPUT_LIMIT`].
    pub stdout: String,
    /// The end of its standard error, at most [`OUTPUT_LIMIT`].
    pub stderr: String,
    /// Whether the timeout killed it.
    pub timed_out: bool,
    /// Whether either stream was longer than came back.
    pub truncated: bool,
    /// Whether the user said no. The command never started.
    pub denied: bool,
}

impl RunOutput {
    /// A command the user said no to. The command never started.
    pub fn denied(argv: &[String]) -> RunOutput {
        RunOutput {
            argv: argv.to_vec(),
            exit: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            truncated: false,
            denied: true,
        }
    }

    /// The tool result the model reads: the exit, and whatever it said.
    pub fn summary(&self) -> String {
        if self.denied {
            return format!("Not allowed, so {} did not run.", self.argv.join(" "));
        }
        let end = if self.timed_out {
            "timed out and was killed"
        } else {
            match self.exit {
                Some(0) => "exited 0",
                Some(code) => return format!("exited {code}\n\n{}", self.output()),
                None => "was killed by a signal",
            }
        };
        format!("{end}\n\n{}", self.output())
    }

    /// Both streams, whichever ones have something in them.
    fn output(&self) -> String {
        let mut out = String::new();
        if !self.stdout.is_empty() {
            out.push_str(&self.stdout);
        }
        if !self.stderr.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&self.stderr);
        }
        if self.truncated {
            out.push_str("\n(output was cut to the end of each stream)");
        }
        if out.is_empty() {
            out.push_str("(no output)");
        }
        out
    }
}

impl DirEntry {
    /// One name, with what it is. `file_type` does not follow a link, which is
    /// what keeps a listing from reading through one.
    fn of(entry: &fs::DirEntry) -> std::io::Result<DirEntry> {
        let kind = entry.file_type()?;
        let kind = if kind.is_symlink() {
            EntryKind::Link
        } else if kind.is_dir() {
            EntryKind::Dir
        } else if kind.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        // A link's own size is the length of its target, which is not a useful
        // number, so only a real file reports one.
        let bytes = if kind == EntryKind::File {
            entry.metadata().map(|meta| meta.len()).unwrap_or(0)
        } else {
            0
        };
        Ok(DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            bytes,
        })
    }
}

/// Resolve a path all the way: absolute, no `.` or `..`, and through every
/// symlink on the way.
///
/// A path that does not exist yet still has to resolve, because a write names a
/// file it is about to create. So the longest part of it that does exist is
/// resolved and the rest is put back on the end. That is enough to catch a
/// directory that is a link out of the workspace, which is where a write would
/// land if it were allowed to.
pub(crate) fn resolve(path: &Path) -> std::io::Result<PathBuf> {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        match fs::canonicalize(&current) {
            Ok(real) => {
                let mut out = real;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return Ok(out);
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = current.file_name().map(|name| name.to_os_string()) else {
                    return Err(source);
                };
                let Some(parent) = current.parent().map(Path::to_path_buf) else {
                    return Err(source);
                };
                tail.push(name);
                current = parent;
            }
            Err(source) => return Err(source),
        }
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// The next number a temporary file gets, so two writes in one process cannot
/// pick the same temporary name.
fn next_temp() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn open(path: &Path) -> Result<File, ToolError> {
    File::open(path).map_err(|source| ToolError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Fill a buffer as far as the file goes, and say how far it got.
fn fill(file: &mut File, bytes: &mut [u8]) -> std::io::Result<usize> {
    let mut read = 0;
    while read < bytes.len() {
        match file.read(&mut bytes[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(source) if source.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(source),
        }
    }
    Ok(read)
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

/// Write a file by writing a temporary one beside it and renaming it over the
/// target.
///
/// A rename within a directory is one step, so a reader sees the old file or
/// the new one, and a write that fails leaves the old file and a temporary file
/// to clean up rather than half a file.
fn write_atomically(parent: &File, path: &Path, bytes: &[u8]) -> Result<(), ToolError> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| ToolError::NoFileName {
            path: path.to_path_buf(),
        })?;
    let temp = format!(".{name}.kyotoagent-{}-{}", std::process::id(), next_temp());
    let mut created = false;
    let mut write = || -> std::io::Result<()> {
        let permissions = match open_at(
            parent,
            path.file_name().unwrap(),
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        ) {
            Ok(file) => Some(file.metadata()?.permissions()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => return Err(source),
        };
        let mut file = open_at(
            parent,
            OsStr::new(&temp),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            permissions
                .as_ref()
                .map_or(0o666, |permissions| permissions.mode() & 0o7777),
        )?;
        created = true;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        let temp = CString::new(temp.as_bytes()).unwrap();
        let name = CString::new(path.file_name().unwrap().as_bytes()).unwrap();
        let renamed = unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                temp.as_ptr(),
                parent.as_raw_fd(),
                name.as_ptr(),
            )
        };
        if renamed < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    };
    if let Err(source) = write() {
        if created {
            let temp = CString::new(temp.as_bytes()).unwrap();
            unsafe { libc::unlinkat(parent.as_raw_fd(), temp.as_ptr(), 0) };
        }
        return Err(ToolError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn write_parent(path: &Path) -> Result<File, ToolError> {
    let parent = path.parent().ok_or_else(|| ToolError::NoDirectory {
        path: path.to_path_buf(),
    })?;
    if !parent.is_dir() {
        return Err(ToolError::NoDirectory {
            path: parent.to_path_buf(),
        });
    }
    let open = || -> std::io::Result<File> {
        let mut directory = File::open("/")?;
        for part in parent.components() {
            match part {
                std::path::Component::RootDir => {}
                std::path::Component::Normal(name) => {
                    directory = open_at(&directory, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                }
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "the write directory is not absolute",
                    ))
                }
            }
        }
        Ok(directory)
    };
    open().map_err(|source| ToolError::Io {
        path: parent.to_path_buf(),
        source,
    })
}

fn open_at(parent: &File, name: &OsStr, flags: i32, mode: u32) -> std::io::Result<File> {
    let name = CString::new(name.as_bytes())
        .map_err(|source| std::io::Error::new(std::io::ErrorKind::InvalidInput, source))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn write_bytes(parent: &File, path: &Path) -> Result<(Vec<u8>, bool), ToolError> {
    let name = path.file_name().ok_or_else(|| ToolError::NoFileName {
        path: path.to_path_buf(),
    })?;
    match open_at(parent, name, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(mut file) => {
            if file
                .metadata()
                .map_err(|source| ToolError::Io {
                    path: path.to_path_buf(),
                    source,
                })?
                .is_dir()
            {
                return Err(ToolError::NotAFile {
                    path: path.to_path_buf(),
                });
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|source| ToolError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
            Ok((bytes, false))
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok((Vec::new(), true)),
        Err(source) => Err(ToolError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Send a signal to a process by pid. A pid that is gone is not an error: the
/// process may have exited between the cancel and the signal.
fn signal_group(pid: Option<u32>, sig: i32) {
    if let Some(pid) = pid {
        unsafe { libc::kill(-(pid as i32), sig) };
    }
}
async fn collect_async(
    handle: Option<tokio::task::JoinHandle<Streamed>>,
    pid: Option<u32>,
) -> Streamed {
    let Some(mut handle) = handle else {
        return Streamed::default();
    };
    match tokio::time::timeout(Duration::from_secs(2), &mut handle).await {
        Ok(result) => result.unwrap_or_default(),
        Err(_) => {
            signal_group(pid, libc::SIGKILL);
            match tokio::time::timeout(Duration::from_secs(2), &mut handle).await {
                Ok(result) => result.unwrap_or_default(),
                Err(_) => {
                    handle.abort();
                    Streamed::default()
                }
            }
        }
    }
}

/// Read one of a command's streams to the end, keeping the last [`OUTPUT_LIMIT`]
/// bytes. The async half of [`drain`], for the cancellable command.
async fn drain_async<R: AsyncRead + Unpin>(
    mut pipe: R,
    output: Option<OutputSink>,
    stderr: bool,
) -> Streamed {
    let mut tail = Tail::new();
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tail.push(&chunk[..n]);
                if let Some(output) = &output {
                    output(stderr, &chunk[..n]);
                }
            }
        }
    }
    Streamed {
        text: String::from_utf8_lossy(&tail.bytes).into_owned(),
        total: tail.total,
    }
}

/// Read one of a command's streams on its own thread, keeping the end of it.
struct Tail {
    bytes: Vec<u8>,
    /// How much the stream held, kept or not.
    total: usize,
}

impl Tail {
    fn new() -> Tail {
        Tail {
            bytes: Vec::new(),
            total: 0,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len();
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > OUTPUT_LIMIT {
            let excess = self.bytes.len() - OUTPUT_LIMIT;
            self.bytes.drain(..excess);
        }
    }
}

/// What a drain thread sends back.
#[derive(Default)]
struct Streamed {
    text: String,
    total: usize,
}

fn drain<R: Read + Send + 'static>(mut pipe: R) -> mpsc::Receiver<Streamed> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut tail = Tail::new();
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => tail.push(&chunk[..n]),
            }
        }
        let _ = sender.send(Streamed {
            // The end of a stream is where a failure is, so the cap keeps the
            // end rather than the beginning.
            text: String::from_utf8_lossy(&tail.bytes).into_owned(),
            total: tail.total,
        });
    });
    receiver
}

/// Wait for a drain thread, without waiting forever for one that will not
/// finish.
fn collect(receiver: mpsc::Receiver<Streamed>) -> Streamed {
    receiver
        .recv_timeout(Duration::from_secs(PIPE_GRACE_SEC))
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Hit {
    path: String,
    line: u64,
    text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Grep {
    pub text: String,
    pub truncated: bool,
}

impl Grep {
    fn from_hits(mut hits: Vec<Hit>, limit: usize) -> Grep {
        hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        let truncated = hits.len() > limit;
        hits.truncate(limit);
        let mut text = hits
            .iter()
            .map(|hit| format!("{}:{}:{}", hit.path, hit.line, hit.text))
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            text = "No matches.".to_string();
        } else if truncated {
            text.push_str(&format!("\n\nShowing the first {limit} matches."));
        }
        Grep { text, truncated }
    }

    pub fn summary(&self) -> String {
        self.text.clone()
    }
}

enum ReplaceMiss {
    None,
    Many(usize),
}

fn replace_once(
    text: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, ReplaceMiss> {
    let count = match_count(text, old);
    if count == 0 {
        return Err(ReplaceMiss::None);
    }
    if !replace_all && count != 1 {
        return Err(ReplaceMiss::Many(count));
    }
    Ok(text.replace(old, new))
}

fn match_count(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while let Some(found) = hay[start..].find(needle) {
        count += 1;
        start += found + needle.len();
    }
    count
}

fn read_line_slice(
    path: &Path,
    line: u64,
    limit: Option<u64>,
) -> Result<(String, Option<u64>), ToolError> {
    if line == 0 {
        return Err(ToolError::BadLine { line });
    }
    if limit == Some(0) {
        return Err(ToolError::BadLimit { limit: 0 });
    }
    let file = open(path)?;
    let mut reader = BufReader::new(file);
    let mut buf = String::new();
    let mut current = 0u64;
    loop {
        buf.clear();
        let read = reader.read_line(&mut buf).map_err(|source| ToolError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            return Err(ToolError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("line {line} is past the end of {}", display(path)),
                ),
            });
        }
        file_read::decode_text(buf.as_bytes(), false, path)?;
        current += 1;
        if current == line {
            break;
        }
    }
    let max_lines = limit.unwrap_or(200);
    let mut out = String::new();
    let mut produced = 0u64;
    let mut number = line;
    loop {
        file_read::decode_text(buf.as_bytes(), false, path)?;
        let content = buf.trim_end_matches(['\n', '\r']);
        let piece = format!("{number}→{content}");
        let extra = if out.is_empty() {
            piece.len()
        } else {
            piece.len() + 1
        };
        if out.len() + extra > READ_LIMIT {
            if out.is_empty() {
                let marker = format!("{number}→");
                let budget = READ_LIMIT.saturating_sub(marker.len());
                let kept = take_bytes(content, budget);
                return Ok((format!("{marker}{kept}"), Some(number + 1)));
            }
            return Ok((out, Some(number)));
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&piece);
        produced += 1;
        number += 1;
        if produced == max_lines {
            buf.clear();
            let read = reader.read_line(&mut buf).map_err(|source| ToolError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            if read == 0 {
                return Ok((out, None));
            }
            return Ok((out, Some(number)));
        }
        buf.clear();
        let read = reader.read_line(&mut buf).map_err(|source| ToolError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            return Ok((out, None));
        }
    }
}

fn take_bytes(text: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if out.len() + ch.len_utf8() > max {
            break;
        }
        out.push(ch);
    }
    out
}

fn rg_ready() -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| {
        Command::new("rg")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

fn grep_rg(
    workspace: &Path,
    root: &Path,
    pattern: &str,
    glob: Option<&str>,
    limit: usize,
) -> Result<Option<Vec<Hit>>, ToolError> {
    if !rg_ready() {
        return Ok(None);
    }
    let mut cmd = Command::new("rg");
    cmd.current_dir(workspace)
        .arg("-n")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--no-ignore-parent")
        .arg("--no-ignore-global")
        .arg("--max-count")
        .arg(limit.to_string())
        .arg("-e")
        .arg(pattern);
    if let Some(glob) = glob.filter(|glob| !glob.is_empty()) {
        cmd.arg("-g").arg(glob);
    }
    let relative = root.strip_prefix(workspace).unwrap_or(root);
    let relative = if relative.as_os_str().is_empty() {
        Path::new(".")
    } else {
        relative
    };
    cmd.arg("--").arg(relative);
    let output = cmd.output().map_err(|source| ToolError::Spawn {
        program: "rg".into(),
        source,
    })?;
    match output.status.code() {
        Some(0) | Some(1) => {}
        _ => {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let message = if message.is_empty() {
                "rg failed".to_string()
            } else {
                message
            };
            return Err(ToolError::BadPattern { message });
        }
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut hits = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(hit) = parse_rg_line(line) {
            hits.push(hit);
        }
    }
    Ok(Some(hits))
}

fn parse_rg_line(line: &str) -> Option<Hit> {
    let (path, rest) = line.split_once(':')?;
    let (number, text) = rest.split_once(':')?;
    let line = number.parse().ok()?;
    let path = path.strip_prefix("./").unwrap_or(path).to_string();
    Some(Hit {
        path,
        line,
        text: text.to_string(),
    })
}

fn grep_walk(
    workspace: &Path,
    root: &Path,
    pattern: &str,
    glob: Option<&str>,
    limit: usize,
) -> Result<Vec<Hit>, ToolError> {
    if !literal_pattern(pattern) {
        return Err(ToolError::BadPattern {
            message: "rg is not installed, and this pattern is not a literal".into(),
        });
    }
    let rules = ignore_rules(workspace);
    let mut hits = Vec::new();
    walk_grep(workspace, root, pattern, glob, limit, &rules, &mut hits)?;
    Ok(hits)
}

fn walk_grep(
    workspace: &Path,
    path: &Path,
    pattern: &str,
    glob: Option<&str>,
    limit: usize,
    rules: &[String],
    hits: &mut Vec<Hit>,
) -> Result<(), ToolError> {
    if hits.len() >= limit {
        return Ok(());
    }
    let meta = fs::symlink_metadata(path).map_err(|source| ToolError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        let mut entries: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(path).map_err(|source| ToolError::Io {
            path: path.to_path_buf(),
            source,
        })? {
            let entry = entry.map_err(|source| ToolError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            entries.push(entry.path());
        }
        entries.sort();
        for entry in entries {
            if hits.len() >= limit {
                break;
            }
            let name = entry
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.starts_with('.') {
                continue;
            }
            let rel = entry.strip_prefix(workspace).unwrap_or(&entry);
            let child_meta = match fs::symlink_metadata(&entry) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if child_meta.file_type().is_symlink() {
                continue;
            }
            if ignored(rules, rel, child_meta.is_dir()) {
                continue;
            }
            walk_grep(workspace, &entry, pattern, glob, limit, rules, hits)?;
        }
        return Ok(());
    }
    if !meta.is_file() {
        return Ok(());
    }
    let rel = path.strip_prefix(workspace).unwrap_or(path);
    let rel_text = rel.to_string_lossy().replace('\\', "/");
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(glob) = glob.filter(|glob| !glob.is_empty()) {
        if !glob_allows(glob, &rel_text, &name) {
            return Ok(());
        }
    }
    if binary_file(path) {
        return Ok(());
    }
    let file = open(path)?;
    let reader = BufReader::new(file);
    for (index, line) in reader.lines().enumerate() {
        if hits.len() >= limit {
            break;
        }
        let line = line.map_err(|source| ToolError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if line.contains(pattern) {
            hits.push(Hit {
                path: rel_text.clone(),
                line: (index + 1) as u64,
                text: line,
            });
        }
    }
    Ok(())
}

fn literal_pattern(pattern: &str) -> bool {
    !pattern.chars().any(|ch| {
        matches!(
            ch,
            '.' | '*' | '+' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '\\'
        )
    })
}

fn ignore_rules(workspace: &Path) -> Vec<String> {
    fs::read_to_string(workspace.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .map(str::to_string)
        .collect()
}

fn ignored(rules: &[String], rel: &Path, is_dir: bool) -> bool {
    if rel.components().any(|part| {
        let name = part.as_os_str();
        name == ".git" || name == "target"
    }) {
        return true;
    }
    let rel_text = rel.to_string_lossy().replace('\\', "/");
    let name = rel
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    rules.iter().any(|rule| {
        let dir_only = rule.ends_with('/');
        if dir_only && !is_dir {
            return false;
        }
        let pat = rule.trim_end_matches('/').trim_start_matches('/');
        if pat.is_empty() {
            return false;
        }
        if pat.contains('*') || pat.contains('?') {
            return glob_match(pat, &name) || glob_match(pat, &rel_text);
        }
        if pat.contains('/') {
            return rel_text == pat || rel_text.starts_with(&format!("{pat}/"));
        }
        name == pat || rel_text.split('/').any(|part| part == pat)
    })
}

fn glob_allows(glob: &str, rel: &str, name: &str) -> bool {
    if glob.contains('/') {
        glob_match(glob, rel)
    } else {
        glob_match(glob, name)
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let mut p = 0;
    let mut t = 0;
    let mut star_p = None;
    let mut star_t = 0;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star_p = Some(p);
            star_t = t;
            p += 1;
        } else if let Some(saved) = star_p {
            star_t += 1;
            t = star_t;
            p = saved + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn binary_file(path: &Path) -> bool {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return true,
    };
    let mut buf = [0u8; 8192];
    match file.read(&mut buf) {
        Ok(n) => buf[..n].contains(&0),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    mod pdf_fixture {
        include!("../tests/fixtures/pdf.rs");
    }
    use super::*;
    use crate::permit::Answer;

    fn workspace(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kyotoagent-tools-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the workspace exists");
        fs::canonicalize(&dir).expect("the workspace resolves")
    }

    /// A session on `workspace`, with the gate answers left as the test left
    /// them.
    fn tools_for(name: &str) -> (Tools, PathBuf) {
        let workspace = workspace(name);
        let dir =
            std::env::temp_dir().join(format!("kyotoagent-tools-{}-{name}-s", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let session = Session::at(&dir);
        session
            .create(&crate::session::SessionMeta::new(
                "91bc",
                &workspace,
                "gpt",
                "2026-09-29T00:00:00.000Z",
            ))
            .expect("the session is created");
        (Tools::at(&session).expect("the tools are built"), workspace)
    }

    #[tokio::test]
    async fn shell_descendants_do_not_hold_timeout_or_cancel_open() {
        for cancelled in [false, true] {
            let (tools, workspace) = tools_for(if cancelled {
                "group-cancel"
            } else {
                "group-timeout"
            });
            let (tx, mut rx) = tokio::sync::watch::channel(false);
            let trigger = tokio::spawn(async move {
                if cancelled {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    tx.send(true).unwrap();
                } else {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            });
            let argv = vec![
                "sh".into(),
                "-c".into(),
                "sleep 30 & echo $! > child.pid; wait".into(),
            ];
            let output = tokio::time::timeout(
                Duration::from_secs(6),
                tools.execute_cancellable(&argv, Some(1), &mut rx),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(output.timed_out, !cancelled);
            let pid: i32 = fs::read_to_string(workspace.join("child.pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
            trigger.abort();
            fs::remove_dir_all(workspace).unwrap();
        }
    }
    #[tokio::test]
    async fn already_cancelled_commands_never_start() {
        let (tools, workspace) = tools_for("already-cancelled");
        let (_tx, mut rx) = tokio::sync::watch::channel(true);
        let argv = vec!["sh".into(), "-c".into(), "touch should-not-exist".into()];
        tools
            .execute_cancellable(&argv, Some(1), &mut rx)
            .await
            .unwrap();
        assert!(!workspace.join("should-not-exist").exists());
        fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn a_path_inside_the_workspace_that_leaves_it_is_refused() {
        let (tools, workspace) = tools_for("escape");
        let outside = workspace
            .join("..")
            .join(format!("kyotoagent-outside-{}", std::process::id()));
        fs::create_dir_all(&outside).expect("somewhere else");
        let secret = outside.join("secret.txt");
        fs::write(&secret, "not for the agent").expect("a file elsewhere");
        // A link dropped in the repository, pointing out of it.
        std::os::unix::fs::symlink(&outside, workspace.join("away")).expect("a link out");

        let error = tools
            .read_file("t1", "away/secret.txt", None, None, None)
            .expect_err("a link out of the workspace is not followed");
        assert!(matches!(error, ToolError::Outside { .. }), "{error}");
        // And a `..` that walks out is the same answer.
        let error = tools
            .read_file("t1", "../kyotoagent-outside-x/secret.txt", None, None, None)
            .expect_err("a parent that leaves the workspace is refused");
        assert!(matches!(error, ToolError::Outside { .. }), "{error}");

        // The tool that would have written through it is refused the same way.
        let error = tools
            .write_file("t1", "away/secret.txt", "mine now")
            .expect_err("a write through a link out is refused");
        assert!(matches!(error, ToolError::Outside { .. }), "{error}");
        assert_eq!(
            fs::read_to_string(&secret).expect("the file is readable"),
            "not for the agent"
        );

        fs::remove_dir_all(&outside).expect("clean up");
        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn pdf_reads_extract_text_or_render_selected_pages() {
        let (tools, workspace) = tools_for("pdf-read");
        fs::write(
            workspace.join("document.txt"),
            pdf_fixture::document(&["Alpha document", "Beta document"]),
        )
        .unwrap();
        let text = tools
            .read_file_with_options(
                "t1",
                "document.txt",
                ReadOptions {
                    pages: Some("2".into()),
                    format: Some("text".into()),
                    ..ReadOptions::default()
                },
            )
            .unwrap();
        assert!(text.text.contains("Beta document"));
        assert!(!text.text.contains("Alpha document"));
        assert!(text.images.is_empty());
        let images = tools
            .read_file("t1", "document.txt", None, None, None)
            .unwrap();
        assert_eq!(images.images.len(), 2);
        for image in images.images {
            image.validate().unwrap();
        }
        for pages in ["0", "3", "2-1", "invalid"] {
            assert!(tools
                .read_file_with_options(
                    "t1",
                    "document.txt",
                    ReadOptions {
                        pages: Some(pages.into()),
                        ..ReadOptions::default()
                    }
                )
                .is_err());
        }
        fs::write(
            workspace.join("long.pdf"),
            pdf_fixture::document(&["Page"; 12]),
        )
        .unwrap();
        assert!(tools.read_file("t1", "long.pdf", None, None, None).is_err());
        let text = tools
            .read_file_with_options(
                "t1",
                "long.pdf",
                ReadOptions {
                    pages: Some("11-".into()),
                    format: Some("text".into()),
                    ..ReadOptions::default()
                },
            )
            .unwrap();
        assert!(text.text.contains("Page 12 of 12"));
        fs::write(workspace.join("broken.pdf"), b"%PDF-1.4 broken").unwrap();
        assert!(tools
            .read_file("t1", "broken.pdf", None, None, None)
            .is_err());
        fs::remove_dir_all(tools.session().dir()).unwrap();
    }

    #[test]
    fn file_types_follow_bytes_instead_of_names() {
        let (tools, workspace) = tools_for("magic-read");
        fs::write(workspace.join("photo.txt"), crate::splash::PNG).unwrap();
        let image = tools
            .read_file("t1", "photo.txt", None, None, None)
            .unwrap();
        assert_eq!(image.images.len(), 1);
        image.images[0].validate().unwrap();
        assert!(!image.text.contains('\u{fffd}'));
        for name in [
            "plain.png",
            "plain.pdf",
            "plain.zip",
            "plain.pptx",
            "drawing.svg",
        ] {
            fs::write(workspace.join(name), "Readable text π\n").unwrap();
            let text = tools.read_file("t1", name, Some(0), None, None).unwrap();
            assert_eq!(text.text, "Readable text π\n");
            assert!(text.images.is_empty());
        }
        fs::write(workspace.join("broken.txt"), b"\xff\xfe\xfa").unwrap();
        assert!(tools
            .read_file("t1", "broken.txt", None, None, None)
            .is_err());
        fs::write(workspace.join("broken.png"), &crate::splash::PNG[..40]).unwrap();
        assert!(tools
            .read_file("t1", "broken.png", None, None, None)
            .is_err());
        fs::remove_dir_all(tools.session().dir()).unwrap();
    }

    #[test]
    fn raster_file_reads_transcode_supported_magic_signatures() {
        let (tools, workspace) = tools_for("raster-formats");
        let pixels = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            4,
            4,
            image::Rgb([255, 0, 0]),
        ));
        for format in [
            image::ImageFormat::Png,
            image::ImageFormat::Jpeg,
            image::ImageFormat::Gif,
            image::ImageFormat::WebP,
            image::ImageFormat::Bmp,
            image::ImageFormat::Tiff,
            image::ImageFormat::Ico,
        ] {
            let mut bytes = std::io::Cursor::new(Vec::new());
            if format == image::ImageFormat::Ico {
                image::DynamicImage::ImageRgba8(pixels.to_rgba8())
                    .write_to(&mut bytes, format)
                    .unwrap();
            } else {
                pixels.write_to(&mut bytes, format).unwrap();
            }
            fs::write(workspace.join("image.data"), bytes.get_ref()).unwrap();
            let read = tools
                .read_file("t1", "image.data", None, None, None)
                .unwrap();
            assert_eq!(read.images.len(), 1, "{format:?}");
            read.images[0].validate().unwrap();
        }
        fs::remove_dir_all(tools.session().dir()).unwrap();
    }

    #[test]
    fn byte_reads_preserve_utf8_at_the_read_boundary() {
        let (tools, workspace) = tools_for("utf8-boundary");
        let text = format!("{}πmore", "x".repeat(READ_LIMIT - 1));
        fs::write(workspace.join("unicode.txt"), &text).unwrap();
        let first = tools
            .read_file("t1", "unicode.txt", Some(0), None, None)
            .unwrap();
        assert_eq!(first.next_offset, Some((READ_LIMIT - 1) as u64));
        let rest = tools
            .read_file("t1", "unicode.txt", first.next_offset, None, None)
            .unwrap();
        assert_eq!(format!("{}{}", first.text, rest.text), text);
        fs::remove_dir_all(tools.session().dir()).unwrap();
    }

    #[test]
    fn binary_reads_never_return_bytes_as_text() {
        let (tools, workspace) = tools_for("binary-read");
        for name in [
            "archive.zip",
            "sheet.xlsx",
            "slides.pptx",
            "program.exe",
            "hidden.txt",
        ] {
            fs::write(workspace.join(name), b"PK\0\xffbinary payload").unwrap();
            for (offset, line) in [(None, None), (Some(4), None), (None, Some(1))] {
                let error = tools.read_file("t1", name, offset, line, None).unwrap_err();
                assert!(error.to_string().contains("binary"), "{error}");
                assert!(!error.to_string().contains("payload"));
            }
        }
        fs::remove_dir_all(tools.session().dir()).unwrap();
    }

    #[test]
    fn a_read_stops_at_its_limit_and_says_where_to_continue() {
        let (tools, workspace) = tools_for("read");
        let big: String = (0..(READ_LIMIT + 100)).map(|_| 'x').collect();
        fs::write(workspace.join("big.txt"), &big).expect("a big file");

        let first = tools
            .read_file("t1", "big.txt", Some(0), None, None)
            .expect("the first read runs");
        assert_eq!(first.text.len(), READ_LIMIT);
        assert!(!first.text.contains('→'), "a byte read has no line prefix");
        assert!(first.next_line.is_none());
        assert_eq!(first.next_offset, Some(READ_LIMIT as u64));
        assert!(first.summary().contains(&format!("offset {READ_LIMIT}")));

        let rest = tools
            .read_file("t1", "big.txt", first.next_offset, None, None)
            .expect("the second read runs");
        assert_eq!(rest.text.len(), 100);
        assert_eq!(rest.next_offset, None, "the end of the file");

        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn a_listing_is_one_level_and_names_a_link_as_a_link() {
        let (tools, workspace) = tools_for("list");
        fs::create_dir_all(workspace.join("src")).expect("a subdirectory");
        fs::write(workspace.join("a.txt"), "a").expect("a file");
        std::os::unix::fs::symlink("/etc", workspace.join("etc")).expect("a link");

        let listed = tools.list_dir("t1", ".").expect("the listing runs");
        let names: Vec<(&str, EntryKind)> = listed
            .entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind))
            .collect();
        assert_eq!(
            names,
            vec![
                ("a.txt", EntryKind::File),
                ("etc", EntryKind::Link),
                ("src", EntryKind::Dir),
            ]
        );
        assert!(!listed.truncated);
        assert!(listed.summary().contains("etc (link)"));

        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn a_write_of_nothing_makes_a_file_with_nothing_in_it() {
        let (tools, _workspace) = tools_for("empty");
        tools.gate().queue(Answer::allow_session());

        let written = tools
            .write_file("t1", "new.txt", "")
            .expect("the write runs");
        assert!(written.created, "the file was not there");
        assert_eq!(written.bytes, 0);
        assert_eq!(
            written.summary(),
            format!("Created {} with 0 bytes.", written.path)
        );

        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn a_command_stops_when_its_timeout_is_up() {
        let (tools, _workspace) = tools_for("timeout");
        tools.gate().queue(Answer::allow_session());

        let started = Instant::now();
        let output = tools
            .run("t1", &["sleep".to_string(), "30".to_string()], Some(1))
            .expect("the command runs");
        assert!(output.timed_out, "{output:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "it did stop early"
        );

        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn a_command_reports_its_exit_and_the_end_of_its_output() {
        let (tools, _workspace) = tools_for("exit");
        tools.gate().queue(Answer::allow_session());

        let output = tools
            .run(
                "t1",
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "echo out; echo err 1>&2; exit 3".to_string(),
                ],
                None,
            )
            .expect("the command runs");
        assert_eq!(output.exit, Some(3));
        assert!(output.stdout.contains("out"));
        assert!(output.stderr.contains("err"));
        assert!(output.summary().starts_with("exited 3"));
        assert!(!output.timed_out);

        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn a_timeout_outside_the_range_is_refused_before_anything_runs() {
        let (tools, _workspace) = tools_for("bad-timeout");
        assert!(matches!(
            tools.run("t1", &["true".to_string()], Some(MAX_TIMEOUT_SEC + 1)),
            Err(ToolError::BadTimeout { .. })
        ));
        assert!(matches!(
            tools.run("t1", &["true".to_string()], Some(0)),
            Err(ToolError::BadTimeout { .. })
        ));
        assert!(matches!(
            tools.run("t1", &[], None),
            Err(ToolError::NoCommand)
        ));
        // None of that reached the gate, so nothing was asked.
        assert!(tools.gate().open_permission().is_none());

        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn a_write_over_the_limit_is_refused_and_the_workspace_is_untouched() {
        let (tools, workspace) = tools_for("too-large");
        let big = "x".repeat(WRITE_LIMIT + 1);
        let error = tools
            .write_file("t1", "big.txt", &big)
            .expect_err("the write is too large");
        assert!(matches!(error, ToolError::TooLarge { .. }), "{error}");
        assert!(!workspace.join("big.txt").exists(), "nothing was written");
        assert!(
            tools.gate().open_permission().is_none(),
            "and nothing was asked"
        );

        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn a_write_into_a_directory_that_is_not_there_says_so() {
        let (tools, _workspace) = tools_for("no-dir");
        tools.gate().queue(Answer::allow_session());
        let error = tools
            .write_file("t1", "missing/big.txt", "hello")
            .expect_err("there is no directory to write in");
        assert!(matches!(error, ToolError::NoDirectory { .. }), "{error}");

        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn install_gh(bin: &Path, script: &str) {
        fs::create_dir_all(bin).expect("bin");
        let gh = bin.join("gh");
        fs::write(&gh, script).expect("gh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = fs::metadata(&gh).expect("meta").permissions();
            perm.set_mode(0o755);
            fs::set_permissions(&gh, perm).expect("chmod");
        }
    }

    #[test]
    fn a_gh_pr_create_stores_the_url_from_stdout_and_a_later_one_replaces_it() {
        let (tools, workspace) = tools_for("pr-create");
        tools.gate().queue(Answer::allow_session());
        let first = format!(
            "{}/pmdroid/kyotoagent/pull/14",
            crate::session::github_origin()
        );
        let second = format!(
            "{}/pmdroid/kyotoagent/pull/22",
            crate::session::github_origin()
        );
        let bin = workspace.join("bin");
        let argv = vec!["gh".to_string(), "pr".to_string(), "create".to_string()];
        let _guard = PATH_LOCK.lock().expect("path lock");
        let old = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{old}", bin.display()));
        install_gh(&bin, &format!("#!/bin/sh\nprintf '%s\\n' '{first}'\n"));
        tools.run("t1", &argv, None).expect("create");
        assert_eq!(
            tools.session().meta().expect("meta").pull_url.as_deref(),
            Some(first.as_str())
        );
        install_gh(&bin, &format!("#!/bin/sh\nprintf '%s\\n' '{second}'\n"));
        tools.run("t1", &argv, None).expect("second create");
        assert_eq!(
            tools.session().meta().expect("meta").pull_url.as_deref(),
            Some(second.as_str())
        );
        std::env::set_var("PATH", old);
        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn a_gh_pr_create_stores_a_url_printed_on_stderr() {
        let (tools, workspace) = tools_for("pr-stderr");
        tools.gate().queue(Answer::allow_session());
        let url = format!(
            "{}/pmdroid/kyotoagent/pull/14",
            crate::session::github_origin()
        );
        let bin = workspace.join("bin");
        let script = format!("#!/bin/sh\nprintf '%s\\n' '{url}' >&2\n");
        install_gh(&bin, &script);
        let _guard = PATH_LOCK.lock().expect("path lock");
        let old = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{old}", bin.display()));
        tools
            .run(
                "t1",
                &["gh".to_string(), "pr".to_string(), "create".to_string()],
                None,
            )
            .expect("create");
        assert_eq!(
            tools.session().meta().expect("meta").pull_url.as_deref(),
            Some(url.as_str())
        );
        std::env::set_var("PATH", old);
        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn a_command_that_is_not_gh_pr_create_does_not_store_a_url() {
        let (tools, _workspace) = tools_for("not-pr");
        tools.gate().queue(Answer::allow_session());
        let url = format!(
            "{}/pmdroid/kyotoagent/pull/14",
            crate::session::github_origin()
        );
        let output = tools
            .run(
                "t1",
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    format!("printf '%s\\n' '{url}'"),
                ],
                None,
            )
            .expect("echo");
        assert!(output.stdout.contains("pull/14"));
        assert_eq!(tools.session().meta().expect("meta").pull_url, None);
        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }

    #[test]
    fn the_line_walker_honors_a_glob_and_skips_target() {
        let (tools, workspace) = tools_for("walk");
        fs::create_dir_all(workspace.join("src")).expect("src");
        fs::write(workspace.join("src/lib.rs"), "walker-needle\n").expect("rust");
        fs::write(workspace.join("notes.md"), "walker-needle\n").expect("markdown");
        fs::create_dir_all(workspace.join("target")).expect("target");
        fs::write(workspace.join("target/out.rs"), "walker-needle\n").expect("build output");
        let mut binary = workspace.join("src/blob.rs");
        fs::write(&binary, b"walker-needle\0hidden\n").expect("binary");
        let hits = grep_walk(&workspace, &workspace, "walker-needle", Some("*.rs"), 20)
            .expect("the walker runs");
        let paths: Vec<&str> = hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(paths, vec!["src/lib.rs"]);
        binary = workspace.join(".gitignore");
        fs::write(&binary, "src/\n").expect("gitignore");
        let hits =
            grep_walk(&workspace, &workspace, "walker-needle", None, 20).expect("the walker runs");
        let paths: Vec<&str> = hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(paths, vec!["notes.md"]);
        fs::remove_dir_all(tools.session().dir()).expect("clean up");
    }
}
