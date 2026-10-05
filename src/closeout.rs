use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::events::{CloseoutRunBody, ProofItem};
use crate::tools::RunOutput;

pub const DEFAULT_MAX_FAILURES: u32 = 3;
const TAIL_LINES: usize = 200;
pub const ID_RULE: &str = "^[a-z][a-z0-9-]*$";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseoutItem {
    pub id: String,
    pub kind: CloseoutKind,
    pub run: String,
    pub hint: String,
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseoutKind {
    Setup,
    Command,
    Cucumber,
    Visual,
    Receipt,
    Review,
    Ci,
    ReviewThreads,
}

impl CloseoutKind {
    pub fn label(self) -> &'static str {
        match self {
            CloseoutKind::Setup => "setup",
            CloseoutKind::Command => "command",
            CloseoutKind::Cucumber => "cucumber",
            CloseoutKind::Visual => "visual",
            CloseoutKind::Receipt => "receipt",
            CloseoutKind::Review => "review",
            CloseoutKind::Ci => "ci",
            CloseoutKind::ReviewThreads => "reviewThreads",
        }
    }

    pub fn is_supported(self) -> bool {
        matches!(self, CloseoutKind::Command)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawFile")]
pub struct CloseoutFile {
    pub setup: Vec<CloseoutItem>,
    pub executions: HashMap<String, CloseoutExecution>,
    pub items: Vec<CloseoutItem>,
    pub max_failures: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseoutExecution {
    pub argv: Vec<String>,
    pub timeout: u64,
}

#[derive(Debug)]
pub enum CloseoutError {
    Parse { path: PathBuf, source: String },
    BadId { id: String },
    DuplicateId { id: String },
    UnsupportedKind { id: String, kind: String },
    BadVersion { version: u32 },
    BadPath { path: String },
    BadDefinition { message: String },
}

impl std::fmt::Display for CloseoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloseoutError::Parse { path, source } => write!(f, "{}: {source}", path.display()),
            CloseoutError::BadId { id } => write!(f, "{id} is not an id the file accepts"),
            CloseoutError::DuplicateId { id } => write!(f, "{id} appears twice in the file"),
            CloseoutError::UnsupportedKind { id, kind } => {
                write!(f, "{id} has kind {kind}, which kyotoagent does not accept")
            }
            CloseoutError::BadVersion { version } => write!(f, "version {version} is not 1"),
            CloseoutError::BadPath { path } => write!(f, "{path} is not a path the file accepts"),
            CloseoutError::BadDefinition { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CloseoutError {}

#[derive(Clone, Debug, Default)]
pub struct ItemState {
    pub attempts: u32,
    pub failures: u32,
    pub passed: bool,
    pub last_failure: Option<(i32, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct CloseoutState {
    pub file: Option<CloseoutFile>,
    pub items: HashMap<String, ItemState>,
    pub written_paths: Vec<String>,
    pub stop: Option<String>,
    pub proof_items: Vec<ProofItem>,
    snapshot: HashMap<String, u64>,
}

pub fn closeout_path(workspace: &Path) -> PathBuf {
    let kyoto = workspace.join(".kyotoagent").join("closeout.yaml");
    if kyoto.is_file() {
        kyoto
    } else {
        workspace.join(".agents").join("closeout.yaml")
    }
}

const CLOSEOUT_DIRS: [&str; 2] = [".kyotoagent", ".agents"];

pub fn located(workspace: &Path) -> Option<PathBuf> {
    for name in CLOSEOUT_DIRS {
        let path = workspace.join(name).join("closeout.yaml");
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

pub fn read(workspace: &Path) -> Result<Option<CloseoutFile>, CloseoutError> {
    match located(workspace) {
        Some(path) => load_file(&path),
        None => Ok(None),
    }
}

impl CloseoutState {
    pub fn new(workspace: &Path) -> Result<CloseoutState, CloseoutError> {
        Ok(Self::with_file(workspace, read(workspace)?))
    }

    pub fn with_file(workspace: &Path, file: Option<CloseoutFile>) -> CloseoutState {
        CloseoutState {
            file,
            snapshot: workspace_snapshot(workspace),
            ..CloseoutState::default()
        }
    }

    pub fn cannot_finish(&self) -> Option<String> {
        self.required_blocker()
    }

    pub fn required_blocker(&self) -> Option<String> {
        let file = self.file.as_ref()?;
        let mut open = Vec::new();
        for item in file.setup.iter().chain(&file.items) {
            if !self.is_required(item) {
                continue;
            }
            let state = self.items.get(&item.id).cloned().unwrap_or_default();
            if !state.passed {
                open.push((item, state));
            }
        }
        if open.is_empty() {
            return None;
        }
        let mut result = String::from("Cannot finish yet.");
        for (item, state) in open {
            let status = if state.failures > 0 {
                "failed"
            } else {
                "missing"
            };
            result.push_str(&format!(
                " Check {} is {}. Attempts {} of {}. Hint: {}.",
                item.id, status, state.failures, file.max_failures, item.hint
            ));
        }
        Some(result)
    }

    pub fn is_required(&self, item: &CloseoutItem) -> bool {
        if item.kind == CloseoutKind::Setup
            && !self
                .file
                .as_ref()
                .is_some_and(|file| file.items.iter().any(|check| self.is_required(check)))
        {
            return false;
        }
        if item.paths.is_empty() {
            !self.written_paths.is_empty()
        } else {
            self.written_paths
                .iter()
                .any(|written| item.paths.iter().any(|glob| glob_matches(glob, written)))
        }
    }

    pub fn pinned_run(&self, argv: &[String]) -> Option<String> {
        let file = self.file.as_ref()?;
        for item in file.setup.iter().chain(&file.items) {
            let (check_argv, _) = file.execution(item);
            if argv == check_argv || argv.join(" ") == item.run {
                return Some(item.id.clone());
            }
        }
        None
    }

    pub fn record_write(&mut self, path: &str) {
        for (id, state) in &mut self.items {
            if !self
                .file
                .as_ref()
                .is_some_and(|file| file.setup.iter().any(|step| step.id == *id))
            {
                state.passed = false;
            }
        }
        for item in &mut self.proof_items {
            if item.outcome == "passed" && item.kind != "setup" {
                item.outcome = "stale".into();
            }
        }
        self.written_paths.push(path.to_string());
    }

    pub fn refresh_workspace(&mut self, workspace: &Path) -> Vec<String> {
        if self.file.is_none() {
            return Vec::new();
        }
        let current = workspace_snapshot(workspace);
        let mut changed: Vec<String> = self
            .snapshot
            .keys()
            .chain(current.keys())
            .filter(|path| self.snapshot.get(*path) != current.get(*path))
            .cloned()
            .collect();
        changed.sort();
        changed.dedup();
        for path in &changed {
            self.record_write(path);
        }
        self.snapshot = current;
        changed
    }
    pub fn item_mut(&mut self, id: &str) -> &mut ItemState {
        self.items.entry(id.to_string()).or_default()
    }

    pub fn record_run(&mut self, item: ProofItem) {
        if let Some(existing) = self.proof_items.iter_mut().find(|seen| seen.id == item.id) {
            *existing = item;
        } else {
            self.proof_items.push(item);
        }
    }
}

fn workspace_snapshot(workspace: &Path) -> HashMap<String, u64> {
    use std::hash::{Hash, Hasher};
    let mut paths = Vec::new();
    let listing = std::process::Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(workspace)
        .output();
    if let Some(output) = listing.ok().filter(|output| output.status.success()) {
        paths.extend(
            output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| String::from_utf8_lossy(path).into_owned()),
        );
    } else {
        snapshot_paths(workspace, workspace, &mut paths);
    }
    paths
        .into_iter()
        .filter_map(|path| {
            let full = workspace.join(&path);
            let metadata = std::fs::symlink_metadata(&full).ok()?;
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            if metadata.file_type().is_symlink() {
                std::fs::read_link(full).ok()?.hash(&mut hash);
            } else if metadata.is_file() {
                std::fs::read(full).ok()?.hash(&mut hash);
            } else {
                return None;
            }
            Some((path, hash.finish()))
        })
        .collect()
}
fn snapshot_paths(root: &Path, dir: &Path, paths: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.file_type() else {
            continue;
        };
        if metadata.is_dir() {
            if !matches!(
                entry.file_name().to_str(),
                Some(".git" | "target" | "node_modules")
            ) {
                snapshot_paths(root, &path, paths);
            }
        } else if let Ok(relative) = path.strip_prefix(root) {
            paths.push(relative.to_string_lossy().into_owned());
        }
    }
}
pub fn parse(path: &Path) -> Result<CloseoutFile, CloseoutError> {
    match load_file(path)? {
        Some(file) => Ok(file),
        None => Err(CloseoutError::Parse {
            path: path.to_path_buf(),
            source: "no such file".to_string(),
        }),
    }
}

fn load_file(path: &Path) -> Result<Option<CloseoutFile>, CloseoutError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CloseoutError::Parse {
                path: path.to_path_buf(),
                source: source.to_string(),
            });
        }
    };
    let value: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|source| CloseoutError::Parse {
            path: path.to_path_buf(),
            source: source.to_string(),
        })?;
    let raw = if value.get("version").is_some() {
        serde_yaml::from_str::<LegacyFile>(&text).map(RawFile::Legacy)
    } else {
        serde_yaml::from_str::<PublicFile>(&text).map(RawFile::Public)
    }
    .map_err(|source| CloseoutError::Parse {
        path: path.to_path_buf(),
        source: source.to_string(),
    })?;
    Ok(Some(CloseoutFile::from_raw(raw)?))
}

#[derive(Clone, Debug)]
enum RawFile {
    Legacy(LegacyFile),
    Public(PublicFile),
}

impl<'de> Deserialize<'de> for RawFile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        if value.get("version").is_some() {
            serde_yaml::from_value(value)
                .map(Self::Legacy)
                .map_err(serde::de::Error::custom)
        } else {
            serde_yaml::from_value(value)
                .map(Self::Public)
                .map_err(serde::de::Error::custom)
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct LegacyFile {
    version: u32,
    retry: Option<RawRetry>,
    items: Vec<CloseoutItem>,
    setup: Option<Vec<SetupStep>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicFile {
    #[serde(rename = "specVersion")]
    spec_version: String,
    description: Option<String>,
    #[serde(default)]
    items: Vec<PublicCommand>,
    setup: Option<Vec<SetupStep>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicCommand {
    id: String,
    kind: CloseoutKind,
    gate: String,
    exec: Vec<String>,
    #[serde(rename = "timeoutSeconds")]
    timeout: u64,
    paths: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupStep {
    id: String,
    exec: Vec<String>,
    #[serde(rename = "timeoutSeconds")]
    timeout: u64,
    paths: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawRetry {
    #[serde(rename = "maxFailedAttemptsPerItem")]
    max_failed_attempts_per_item: u32,
}

impl TryFrom<RawFile> for CloseoutFile {
    type Error = CloseoutError;

    fn try_from(raw: RawFile) -> Result<Self, Self::Error> {
        Self::from_raw(raw)
    }
}

fn validate_execution(
    id: &str,
    argv: &[String],
    timeout: u64,
    paths: &Option<Vec<String>>,
) -> Result<(), CloseoutError> {
    let valid_id = is_valid_id(id) && !id.ends_with('-') && !id.contains("--");
    let valid_argv = !argv.is_empty()
        && argv.len() <= 32
        && argv
            .iter()
            .all(|arg| !arg.is_empty() && arg.len() <= 4096 && !arg.contains('\0'));
    let valid_paths = paths.as_ref().is_none_or(|paths| {
        !paths.is_empty()
            && paths.len() <= 100
            && paths
                .iter()
                .all(|path| path.len() <= 256 && is_valid_path_pattern(path))
    });
    if !valid_id || !valid_argv || !(1..=86400).contains(&timeout) || !valid_paths {
        return Err(CloseoutError::BadDefinition {
            message: format!("{id} needs a valid id, exec, timeoutSeconds and paths"),
        });
    }
    Ok(())
}

impl CloseoutFile {
    pub fn execution(&self, item: &CloseoutItem) -> (Vec<String>, Option<u64>) {
        match self.executions.get(&item.id) {
            Some(exec) => (exec.argv.clone(), Some(exec.timeout)),
            None => (vec!["sh".into(), "-c".into(), item.run.clone()], None),
        }
    }

    fn from_raw(raw: RawFile) -> Result<CloseoutFile, CloseoutError> {
        let mut executions = HashMap::new();
        let (items, setup, max_failures) = match raw {
            RawFile::Legacy(raw) => {
                if raw.version != 1 {
                    return Err(CloseoutError::BadVersion {
                        version: raw.version,
                    });
                }
                (
                    raw.items,
                    raw.setup,
                    raw.retry
                        .map(|retry| retry.max_failed_attempts_per_item)
                        .unwrap_or(DEFAULT_MAX_FAILURES),
                )
            }
            RawFile::Public(raw) => {
                if raw.spec_version != "0.1"
                    || raw.items.len() > 128
                    || raw.description.as_ref().is_some_and(|description| {
                        description.is_empty() || description.len() > 500
                    })
                {
                    return Err(CloseoutError::BadDefinition {
                        message: "Invalid Closeout 0.1 policy".into(),
                    });
                }
                let mut items = Vec::new();
                for item in raw.items {
                    validate_execution(&item.id, &item.exec, item.timeout, &item.paths)?;
                    if item.kind != CloseoutKind::Command || item.gate != "beforePR" {
                        return Err(CloseoutError::BadDefinition {
                            message: format!("{} needs kind command and gate beforePR", item.id),
                        });
                    }
                    executions.insert(
                        item.id.clone(),
                        CloseoutExecution {
                            argv: item.exec.clone(),
                            timeout: item.timeout,
                        },
                    );
                    items.push(CloseoutItem {
                        run: item.exec.join(" "),
                        hint: format!("Run {}", item.id),
                        id: item.id,
                        kind: item.kind,
                        paths: item.paths.unwrap_or_default(),
                    });
                }
                (items, raw.setup, DEFAULT_MAX_FAILURES)
            }
        };
        let mut ids = std::collections::HashSet::new();
        for item in &items {
            if !is_valid_id(&item.id) {
                return Err(CloseoutError::BadId {
                    id: item.id.clone(),
                });
            }
            if !ids.insert(item.id.clone()) {
                return Err(CloseoutError::DuplicateId {
                    id: item.id.clone(),
                });
            }
            if !item.kind.is_supported() {
                return Err(CloseoutError::UnsupportedKind {
                    id: item.id.clone(),
                    kind: item.kind.label().to_string(),
                });
            }
            for path in &item.paths {
                if !is_valid_path_pattern(path) {
                    return Err(CloseoutError::BadPath { path: path.clone() });
                }
            }
        }
        let mut steps = Vec::new();
        if let Some(setup) = setup {
            if setup.is_empty() || setup.len() > 32 {
                return Err(CloseoutError::BadDefinition {
                    message: "setup needs between 1 and 32 steps".into(),
                });
            }
            for step in setup {
                validate_execution(&step.id, &step.exec, step.timeout, &step.paths)?;
                if !ids.insert(step.id.clone()) {
                    return Err(CloseoutError::DuplicateId { id: step.id });
                }
                executions.insert(
                    step.id.clone(),
                    CloseoutExecution {
                        argv: step.exec.clone(),
                        timeout: step.timeout,
                    },
                );
                steps.push(CloseoutItem {
                    run: step.exec.join(" "),
                    hint: format!("Run setup {}", step.id),
                    id: step.id,
                    kind: CloseoutKind::Setup,
                    paths: step.paths.unwrap_or_default(),
                });
            }
        }
        Ok(CloseoutFile {
            items,
            setup: steps,
            executions,
            max_failures,
        })
    }
}

fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
        _ => false,
    }
}

fn is_valid_path_pattern(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.starts_with('/') || path.starts_with('~') {
        return false;
    }
    if path.contains('!') || path.contains('\\') {
        return false;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return false;
    }
    for segment in path.strip_suffix('/').unwrap_or(path).split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return false;
        }
    }
    true
}

pub fn glob_matches(pattern: &str, path: &str) -> bool {
    if pattern.ends_with('/') {
        return path.starts_with(pattern);
    }
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();
    glob_match(&pattern_parts, &path_parts)
}

fn glob_match(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        for skip in 0..=path.len() {
            if glob_match(&pattern[1..], &path[skip..]) {
                return true;
            }
        }
        return false;
    }
    if path.is_empty() {
        return false;
    }
    if segment_matches(pattern[0], path[0]) {
        glob_match(&pattern[1..], &path[1..])
    } else {
        false
    }
}

fn segment_matches(pattern: &str, segment: &str) -> bool {
    let mut p = pattern.chars().peekable();
    let mut s = segment.chars().peekable();
    loop {
        let Some(p_char) = p.peek().copied() else {
            return s.next().is_none();
        };
        if p_char == '*' {
            p.next();
            if p.peek().is_none() {
                return true;
            }
            let rest: String = p.clone().collect();
            let remaining: String = s.clone().collect();
            let chars: Vec<char> = remaining.chars().collect();
            for at in 0..=chars.len() {
                let suffix: String = chars[at..].iter().collect();
                if segment_matches(&rest, &suffix) {
                    return true;
                }
            }
            return false;
        }
        p.next();
        if p_char == '?' {
            if s.next().is_none() {
                return false;
            }
        } else {
            match s.next() {
                Some(s_char) if s_char == p_char => continue,
                _ => return false,
            }
        }
    }
}

pub fn tail_of(output: &RunOutput) -> String {
    let mut text = String::new();
    if !output.stdout.is_empty() {
        text.push_str(&output.stdout);
    }
    if !output.stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&output.stderr);
    }
    text.lines()
        .rev()
        .take(TAIL_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn run_body(id: &str, attempt: u32, output: &RunOutput) -> CloseoutRunBody {
    CloseoutRunBody {
        passed: None,
        transcript: None,
        argv: output.argv.clone(),
        timed_out: output.timed_out,
        truncated: output.truncated,
        id: id.to_string(),
        attempt,
        exit: output.exit.unwrap_or(-1),
        tail: tail_of(output),
    }
}

pub fn proof_item(id: &str, passed: bool, argv: Vec<String>, exit: i32, tail: String) -> ProofItem {
    ProofItem {
        id: id.to_string(),
        kind: CloseoutKind::Command.label().to_string(),
        outcome: if passed { "passed" } else { "failed" }.to_string(),
        argv,
        exit: Some(exit),
        tail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn subsequent_writes_mark_recorded_passes_stale() {
        let mut state = CloseoutState::default();
        state.record_run(proof_item(
            "tests",
            true,
            vec!["test".into()],
            0,
            "ok".into(),
        ));
        state.item_mut("tests").passed = true;
        state.record_write("src/app.rs");
        assert!(!state.item_mut("tests").passed);
        assert_eq!(state.proof_items[0].outcome, "stale");
    }

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "kyotoagent-closeout-{}-{name}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".kyotoagent")).expect("the workspace exists");
        dir
    }

    fn write_file(dir: &Path, text: &str) {
        std::fs::write(dir.join(".kyotoagent").join("closeout.yaml"), text)
            .expect("the closeout file is written");
    }

    #[test]
    fn checks_only_gate_workspace_changes_and_detect_shell_edits() {
        let dir = temp_dir("snapshot");
        write_file(
            &dir,
            "version: 1\nitems:
  - id: test\n    kind: command\n    run: true\n    hint: pass\n",
        );
        std::fs::write(dir.join("dirty.txt"), "preexisting dirty contents").unwrap();
        let mut state = CloseoutState::new(&dir).unwrap();
        state.refresh_workspace(&dir);
        assert!(state.cannot_finish().is_none());
        let external = dir.with_extension("external");
        std::fs::write(&external, "global config").unwrap();
        state.refresh_workspace(&dir);
        assert!(state.cannot_finish().is_none());
        std::process::Command::new("sh")
            .args(["-c", "printf changed > dirty.txt"])
            .current_dir(&dir)
            .status()
            .unwrap();
        state.refresh_workspace(&dir);
        assert!(state.cannot_finish().is_some());
        state.item_mut("test").passed = true;
        state.refresh_workspace(&dir);
        assert!(state.cannot_finish().is_none());
        std::fs::remove_file(dir.join("dirty.txt")).unwrap();
        state.refresh_workspace(&dir);
        assert!(state.cannot_finish().is_some());
        std::fs::remove_file(external).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn an_id_the_file_accepts_is_lowercase_then_dashes_and_digits() {
        assert!(is_valid_id("test"));
        assert!(is_valid_id("cargo-test"));
        assert!(is_valid_id("a1"));
        assert!(!is_valid_id("Test"));
        assert!(!is_valid_id("1test"));
        assert!(!is_valid_id("-test"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("test_test"));
    }

    #[test]
    fn a_glob_matches_the_paths_it_names() {
        assert!(glob_matches("src/**", "src/main.rs"));
        assert!(glob_matches("src/**", "src/lib/mod.rs"));
        assert!(glob_matches("*.md", "README.md"));
        assert!(glob_matches("tests/*.rs", "tests/closeout.rs"));
        assert!(!glob_matches("src/**", "tests/closeout.rs"));
        assert!(!glob_matches("*.md", "README.txt"));
        assert!(glob_matches("src", "src"));
        assert!(!glob_matches("src", "src/main.rs"));
    }

    #[test]
    fn a_double_star_matches_zero_segments_too() {
        assert!(glob_matches("**/test", "test"));
        assert!(glob_matches("**/test", "src/test"));
        assert!(glob_matches("**/test", "src/lib/test"));
    }

    #[test]
    fn a_missing_file_is_not_a_closeout_workspace() {
        let state = CloseoutState::new(Path::new("/no/such/workspace")).expect("missing is empty");
        assert!(state.file.is_none());
        assert!(state.cannot_finish().is_none());
    }

    #[test]
    fn kyoto_closeout_takes_precedence_over_agents() {
        let dir = temp_dir("order");
        let _ = std::fs::remove_dir_all(dir.join(".kyotoagent"));
        let agents = "version: 1\nitems:\n  - id: from-agents\n    kind: command\n    run: echo agents\n    hint: agents\n";
        let kyoto = "version: 1\nitems:\n  - id: from-kyoto\n    kind: command\n    run: echo kyoto\n    hint: kyoto\n";
        std::fs::create_dir_all(dir.join(".agents")).expect("the agents directory exists");
        std::fs::write(dir.join(".agents").join("closeout.yaml"), agents)
            .expect("the agents file writes");
        let state = CloseoutState::new(&dir).expect("the agents file loads");
        let file = state.file.expect("the agents file");
        assert_eq!(file.items[0].id, "from-agents");
        assert_eq!(
            closeout_path(&dir),
            dir.join(".agents").join("closeout.yaml")
        );

        std::fs::create_dir_all(dir.join(".kyotoagent")).expect("the kyoto directory exists");
        std::fs::write(dir.join(".kyotoagent").join("closeout.yaml"), kyoto)
            .expect("the kyoto file writes");
        let state = CloseoutState::new(&dir).expect("the kyoto file loads");
        let file = state.file.expect("the kyoto file");
        assert_eq!(file.items[0].id, "from-kyoto");
        assert_eq!(
            closeout_path(&dir),
            dir.join(".kyotoagent").join("closeout.yaml")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_workspace_with_no_checks_lets_finish_through() {
        let dir = temp_dir("empty");
        let _ = std::fs::remove_dir_all(dir.join(".kyotoagent"));
        std::fs::create_dir_all(&dir).expect("the workspace exists");
        let state = CloseoutState::new(&dir).expect("missing is empty");
        assert!(state.cannot_finish().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_with_no_retry_block_allows_three_failures() {
        let dir = temp_dir("default");
        write_file(
            &dir,
            "version: 1\nitems:\n  - id: test\n    kind: command\n    run: cargo test\n    hint: Fix the failing test\n",
        );
        let state = CloseoutState::new(&dir).expect("the file is pinned");
        let file = state.file.as_ref().expect("the file is pinned");
        assert_eq!(file.max_failures, DEFAULT_MAX_FAILURES);
        assert_eq!(file.max_failures, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_duplicate_id_is_a_parse_error() {
        let dir = temp_dir("dup");
        write_file(
            &dir,
            "version: 1\nitems:\n  - id: test\n    kind: command\n    run: a\n    hint: a\n  - id: test\n    kind: command\n    run: b\n    hint: b\n",
        );
        let error = parse(&dir.join(".kyotoagent").join("closeout.yaml"))
            .expect_err("a duplicate id is refused");
        assert!(
            matches!(error, CloseoutError::DuplicateId { .. }),
            "{error}"
        );
        CloseoutState::new(&dir).expect_err("a duplicate id fails closed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unsupported_kind_is_a_parse_error() {
        let dir = temp_dir("kind");
        write_file(
            &dir,
            "version: 1\nitems:\n  - id: shots\n    kind: visual\n    run: capture\n    hint: Fix the shots\n",
        );
        let error = parse(&dir.join(".kyotoagent").join("closeout.yaml"))
            .expect_err("a visual item is refused");
        assert!(
            matches!(error, CloseoutError::UnsupportedKind { .. }),
            "{error}"
        );
        CloseoutState::new(&dir).expect_err("an unsupported kind fails closed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_id_is_a_parse_error() {
        let dir = temp_dir("badid");
        write_file(
            &dir,
            "version: 1\nitems:\n  - id: Test\n    kind: command\n    run: a\n    hint: a\n",
        );
        let error =
            parse(&dir.join(".kyotoagent").join("closeout.yaml")).expect_err("a bad id is refused");
        assert!(matches!(error, CloseoutError::BadId { .. }), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_parent_path_is_a_parse_error() {
        let dir = temp_dir("badpath");
        write_file(
            &dir,
            "version: 1\nitems:\n  - id: test\n    kind: command\n    run: a\n    hint: a\n    paths:\n      - \"../secret\"\n",
        );
        let error = parse(&dir.join(".kyotoagent").join("closeout.yaml"))
            .expect_err("a parent path is refused");
        assert!(
            matches!(error, CloseoutError::BadPath { ref path } if path == "../secret"),
            "{error}"
        );
        let refused = CloseoutState::new(&dir).expect_err("a bad path fails closed");
        assert!(refused.to_string().contains("../secret"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_malformed_file_is_a_parse_error() {
        let dir = temp_dir("malformed");
        write_file(&dir, "version: 1\nitems: [");
        let error = CloseoutState::new(&dir).expect_err("malformed yaml fails closed");
        assert!(matches!(error, CloseoutError::Parse { .. }), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_retry_block_sets_the_maximum() {
        let dir = temp_dir("retry");
        write_file(
            &dir,
            "version: 1\nretry:\n  maxFailedAttemptsPerItem: 5\nitems:\n  - id: test\n    kind: command\n    run: cargo test\n    hint: Fix the failing test\n",
        );
        let state = CloseoutState::new(&dir).expect("the file is pinned");
        let file = state.file.as_ref().expect("the file is pinned");
        assert_eq!(file.max_failures, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_makes_a_pass_stale() {
        let dir = temp_dir("stale");
        write_file(
            &dir,
            "version: 1\nitems:\n  - id: test\n    kind: command\n    run: cargo test\n    hint: Fix the failing test\n",
        );
        let mut state = CloseoutState::new(&dir).expect("the file is pinned");
        state.item_mut("test").passed = true;
        assert!(state.cannot_finish().is_none(), "the check passed");
        state.record_write("src/main.rs");
        let refused = state.cannot_finish().expect("the pass is stale");
        assert!(refused.contains("missing"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_item_with_paths_is_required_only_when_a_path_matches() {
        let dir = temp_dir("paths");
        write_file(
            &dir,
            "version: 1\nitems:\n  - id: test\n    kind: command\n    run: cargo test\n    hint: Fix the failing test\n    paths:\n      - \"src/**\"\n",
        );
        let mut state = CloseoutState::new(&dir).expect("the file is pinned");
        assert!(state.cannot_finish().is_none(), "no path written");
        state.record_write("README.md");
        assert!(state.cannot_finish().is_none(), "the path does not match");
        state.record_write("src/main.rs");
        let refused = state.cannot_finish().expect("the path matches");
        assert!(refused.contains("test"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cannot_finish_text_names_the_id_status_and_hint() {
        let dir = temp_dir("text");
        write_file(
            &dir,
            "version: 1\nitems:\n  - id: test\n    kind: command\n    run: cargo test\n    hint: Fix the failing test\n",
        );
        let mut state = CloseoutState::new(&dir).expect("the file is pinned");
        state.record_write("src/main.rs");
        let refused = state.cannot_finish().expect("the check is missing");
        assert_eq!(
            refused,
            "Cannot finish yet. Check test is missing. Attempts 0 of 3. Hint: Fix the failing test."
        );
        state.item_mut("test").failures = 1;
        let refused = state.cannot_finish().expect("the check failed");
        assert_eq!(
            refused,
            "Cannot finish yet. Check test is failed. Attempts 1 of 3. Hint: Fix the failing test."
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pinned_run_names_the_id_to_use_instead() {
        let dir = temp_dir("pinned");
        write_file(
            &dir,
            "version: 1\nitems:\n  - id: test\n    kind: command\n    run: cargo test\n    hint: Fix the failing test\n",
        );
        let state = CloseoutState::new(&dir).expect("the file is pinned");
        let argv = vec!["sh".to_string(), "-c".to_string(), "cargo test".to_string()];
        assert_eq!(state.pinned_run(&argv).as_deref(), Some("test"));
        let argv = vec!["cargo".to_string(), "test".to_string()];
        assert_eq!(state.pinned_run(&argv).as_deref(), Some("test"));
        let argv = vec!["ls".to_string()];
        assert_eq!(state.pinned_run(&argv), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_output_tail_keeps_the_end() {
        let output = RunOutput {
            argv: vec!["sh".to_string()],
            exit: Some(0),
            stdout: "line1\nline2\nline3\nline4\nline5\n".to_string(),
            stderr: String::new(),
            timed_out: false,
            truncated: false,
            denied: false,
        };
        let tail = tail_of(&output);
        assert_eq!(tail, "line1\nline2\nline3\nline4\nline5");
    }

    #[test]
    fn the_output_tail_keeps_only_the_last_lines() {
        let mut stdout = String::new();
        for i in 0..250 {
            stdout.push_str(&format!("line{i}\n"));
        }
        let output = RunOutput {
            argv: vec!["sh".to_string()],
            exit: Some(0),
            stdout,
            stderr: String::new(),
            timed_out: false,
            truncated: false,
            denied: false,
        };
        let tail = tail_of(&output);
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines.len(), TAIL_LINES);
        assert_eq!(lines[0], "line50");
        assert_eq!(lines[TAIL_LINES - 1], "line249");
    }

    #[test]
    fn a_path_pattern_rejects_the_shapes_the_file_does_not_accept() {
        assert!(!is_valid_path_pattern("/src/**"));
        assert!(!is_valid_path_pattern("!src/**"));
        assert!(!is_valid_path_pattern("src\\main.rs"));
        assert!(!is_valid_path_pattern("../secret"));
        assert!(!is_valid_path_pattern("src/../lib"));
        assert!(!is_valid_path_pattern("./src"));
        assert!(!is_valid_path_pattern("C:secret"));
        assert!(!is_valid_path_pattern("c:/windows"));
        assert!(is_valid_path_pattern("src/**"));
        assert!(is_valid_path_pattern("Cargo.toml"));
        assert!(is_valid_path_pattern("tests/*.rs"));
    }

    #[test]
    fn the_committed_closeout_names_test_fmt_and_clippy() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let kyotoagent = std::fs::read_to_string(root.join(".kyotoagent").join("closeout.yaml"))
            .expect("the kyotoagent file is committed");

        let file = parse(&root.join(".kyotoagent").join("closeout.yaml"))
            .expect("the kyotoagent file parses");
        assert_eq!(file.max_failures, 3);
        let expected = [
            ("cargo-test", "cargo test --offline", "cargo test must pass"),
            ("cargo-fmt", "cargo fmt --check", "rustfmt must be clean"),
            (
                "cargo-clippy",
                "cargo clippy --all-targets --offline -- -D warnings",
                "clippy -D warnings must be clean",
            ),
        ];
        assert_eq!(file.items.len(), expected.len());
        for (item, (id, run, hint)) in file.items.iter().zip(expected) {
            assert_eq!(item.id, id);
            assert_eq!(item.kind, CloseoutKind::Command);
            assert_eq!(item.run, run);
            assert_eq!(item.hint, hint);
            assert!(item.paths.is_empty());
        }
        assert!(file.items.iter().all(|item| item.id != "cargo"));

        let value: serde_yaml::Value =
            serde_yaml::from_str(&kyotoagent).expect("the kyotoagent file is yaml");
        assert_eq!(value["version"].as_u64(), Some(1));
        let items = value["items"].as_sequence().expect("one items list");
        assert_eq!(items.len(), expected.len());
        for (item, (id, run, _)) in items.iter().zip(expected) {
            assert_eq!(item["kind"].as_str(), Some("command"));
            assert_eq!(item["id"].as_str(), Some(id));
            assert_eq!(item["run"].as_str(), Some(run));
        }
    }

    #[test]
    fn the_repo_pins_stable_and_checks_fmt_on_github() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let toolchain = std::fs::read_to_string(root.join("rust-toolchain.toml"))
            .expect("toolchain is committed");
        assert!(toolchain.contains("channel = \"stable\""));
        assert!(toolchain.contains("\"rustfmt\""));
        assert!(toolchain.contains("\"clippy\""));
        let rustfmt =
            std::fs::read_to_string(root.join("rustfmt.toml")).expect("rustfmt.toml is committed");
        assert!(rustfmt.contains("edition = \"2021\""));

        let ci = std::fs::read_to_string(root.join(".github").join("workflows").join("ci.yml"))
            .expect("the workflow is committed");
        assert!(ci.contains("pull_request"));
        assert!(ci.contains("main"));
        assert!(ci.contains("CARGO_TERM_COLOR: always"));
        assert!(ci.contains("ubuntu-latest"));
        assert!(ci.contains("actions/checkout@v4"));
        assert!(ci.contains("dtolnay/rust-toolchain@stable"));
        assert!(ci.contains("components: rustfmt, clippy"));
        assert!(ci.contains("Swatinem/rust-cache@v2"));
        assert!(!ci.contains("--offline"));
        let fmt = ci.find("cargo fmt --check").expect("fmt step");
        let clippy = ci
            .find("cargo clippy --all-targets -- -D warnings")
            .expect("clippy step");
        let test = ci.find("cargo test").expect("test step");
        assert!(fmt < clippy && clippy < test);
    }

    const ONE_CHECK: &str = "version: 1\nitems:\n  - id: test\n    kind: command\n    run: cargo test\n    hint: Fix the failing test\n";

    #[test]
    fn kyoto_closeout_wins_and_agents_is_the_fallback() {
        let dir = temp_dir("both");
        let _ = std::fs::remove_dir_all(dir.join(".kyotoagent"));
        std::fs::create_dir_all(dir.join(".agents")).expect("agents dir");
        std::fs::write(dir.join(".agents").join("closeout.yaml"), ONE_CHECK).expect("agents file");
        let agents = CloseoutState::new(&dir).expect("agents file loads");
        assert_eq!(agents.file.as_ref().expect("file").items[0].id, "test");
        std::fs::create_dir_all(dir.join(".kyotoagent")).expect("kyoto dir");
        std::fs::write(
            dir.join(".kyotoagent").join("closeout.yaml"),
            "version: 1\nitems:\n  - id: kyoto\n    kind: command\n    run: cargo test\n    hint: Kyoto\n",
        )
        .expect("kyoto file");
        let kyoto = CloseoutState::new(&dir).expect("kyoto file loads");
        assert_eq!(kyoto.file.as_ref().expect("file").items[0].id, "kyoto");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_kyoto_file_does_not_fall_through_to_agents() {
        let dir = temp_dir("bad-kyoto");
        write_file(&dir, "version: 1\nitems: [");
        std::fs::create_dir_all(dir.join(".agents")).expect("agents dir");
        std::fs::write(dir.join(".agents").join("closeout.yaml"), ONE_CHECK).expect("agents file");
        CloseoutState::new(&dir).expect_err("the kyoto file fails closed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finish_and_pr_gates_require_the_same_current_pass() {
        let dir = temp_dir("hidden");
        write_file(&dir, ONE_CHECK);
        let mut state = CloseoutState::new(&dir).expect("the file is pinned");
        state.record_write("src/main.rs");
        assert!(state.cannot_finish().is_some());
        assert_eq!(state.cannot_finish(), state.required_blocker());
        state.item_mut("test").passed = true;
        assert!(state.cannot_finish().is_none());
        assert!(state.required_blocker().is_none());
        assert!(state.file.is_some());
        assert!(dir.join(".kyotoagent").join("closeout.yaml").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
