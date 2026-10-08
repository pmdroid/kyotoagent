use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::events::{
    CloseoutChangedBody, CloseoutRunBody, CloseoutStartedBody, Event, EventKind, ProofBody,
    ProofItem,
};
use crate::tools::RunOutput;

mod retry;
pub(crate) use retry::{AttemptIdentity, RetryLedger};
pub use retry::{RetryPolicy, RetryScope};

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

impl CloseoutItem {
    fn matches_path(&self, path: &str) -> bool {
        self.paths.is_empty() || self.paths.iter().any(|glob| glob_matches(glob, path))
    }
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
        matches!(self, CloseoutKind::Command | CloseoutKind::Review)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawFile")]
pub struct CloseoutFile {
    pub imports: Vec<CloseoutImport>,
    pub setup: Vec<CloseoutItem>,
    pub executions: HashMap<String, CloseoutExecution>,
    pub reviews: HashMap<String, CloseoutReview>,
    pub items: Vec<CloseoutItem>,
    pub max_failures: u32,
    pub retry: Option<RetryPolicy>,
    pub policy_digest: String,
    pub policy_files: std::collections::BTreeMap<PathBuf, String>,
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
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) base_ref_name: Option<String>,
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
        Some(path) => load_file(&path, workspace),
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
            workspace: Some(workspace.to_path_buf()),
            ..CloseoutState::default()
        }
    }

    pub fn replay(&mut self, events: &[Event]) {
        for event in events {
            match event.kind {
                EventKind::CloseoutChanged => {
                    if let Ok(body) = event.body_as::<CloseoutChangedBody>() {
                        for path in body.paths {
                            self.record_write(&path);
                        }
                    }
                }
                EventKind::Proof => {
                    if let Ok(body) = event.body_as::<ProofBody>() {
                        for path in body.wrote {
                            if !self.written_paths.contains(&path) {
                                self.record_write(&path);
                            }
                        }
                    }
                }
                EventKind::CloseoutStarted => {
                    if let Ok(body) = event.body_as::<CloseoutStartedBody>() {
                        let state = self.item_mut(&body.id);
                        state.passed = false;
                        state.attempts = body.attempt;
                    }
                }
                EventKind::CloseoutRun => {
                    if let Ok(body) = event.body_as::<CloseoutRunBody>() {
                        let current = self.workspace.clone().as_ref().map(|workspace| {
                            workspace_fingerprint(
                                workspace,
                                &git_text(workspace, &["rev-parse", "HEAD"]).unwrap_or_default(),
                                &git_text(workspace, &["status", "--porcelain"])
                                    .unwrap_or_default(),
                            )
                        });
                        let same_candidate = !body.workspace_fingerprint.is_empty()
                            && current.as_ref() == Some(&body.workspace_fingerprint);
                        let same_policy = self
                            .file
                            .as_ref()
                            .is_some_and(|file| file.policy_digest == body.policy_digest)
                            && !body.policy_digest.is_empty();
                        let state = self.item_mut(&body.id);
                        state.attempts = body.attempt;
                        state.passed = same_candidate
                            && same_policy
                            && body.passed.unwrap_or(body.exit == 0 && !body.timed_out);
                        if state.passed {
                            state.last_failure = None;
                        } else {
                            state.failures += 1;
                            state.last_failure = Some((body.exit, body.tail));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub fn proof_items_with_carried_passes(&self) -> Vec<ProofItem> {
        let mut items = self.proof_items.clone();
        if let Some(file) = &self.file {
            for item in file.setup.iter().chain(&file.items) {
                if self.is_required(item)
                    && self.items.get(&item.id).is_some_and(|state| state.passed)
                    && !items.iter().any(|proof| proof.id == item.id)
                {
                    items.push(ProofItem {
                        id: item.id.clone(),
                        kind: item.kind.label().into(),
                        outcome: "passed_earlier".into(),
                        argv: Vec::new(),
                        exit: None,
                        tail: String::new(),
                    });
                }
            }
        }
        items
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
            let status = if file.retry.is_some() && state.failures >= file.max_failures {
                "exhausted"
            } else if state.failures > 0 {
                "failed"
            } else {
                "missing"
            };
            let attempts = if file.max_failures == u32::MAX {
                format!("Failed attempts {}.", state.failures)
            } else {
                format!("Attempts {} of {}.", state.failures, file.max_failures)
            };
            result.push_str(&format!(
                " Check {} is {}. {} Hint: {}.",
                item.id, status, attempts, item.hint
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
        self.active_written_paths()
            .iter()
            .any(|written| item.matches_path(written))
    }

    pub(crate) fn report(&self) -> serde_json::Value {
        let mut required = Vec::new();
        let mut pending = Vec::new();
        let mut blocked = None;
        if let Some(file) = &self.file {
            for item in file
                .setup
                .iter()
                .chain(&file.items)
                .filter(|item| self.is_required(item))
            {
                let state = self.items.get(&item.id).cloned().unwrap_or_default();
                let exhausted = file.retry.is_some() && state.failures >= file.max_failures;
                let status = if exhausted {
                    blocked = self.required_blocker();
                    "exhausted"
                } else if state.passed {
                    "passed"
                } else if self
                    .proof_items
                    .iter()
                    .any(|proof| proof.id == item.id && proof.outcome == "stale")
                {
                    "stale"
                } else if state.failures > 0 {
                    "failed"
                } else {
                    "missing"
                };
                if !state.passed && !exhausted {
                    pending.push(&item.id);
                }
                let matched_paths: std::collections::BTreeSet<_> = self
                    .active_written_paths()
                    .into_iter()
                    .filter(|path| item.matches_path(path))
                    .collect();
                required.push(serde_json::json!({
                    "id": item.id,
                    "kind": item.kind.label(),
                    "hint": item.hint,
                    "matched_paths": matched_paths,
                    "status": status,
                    "failed_attempts": state.failures,
                    "remaining_attempts": (file.max_failures != u32::MAX).then(|| file.max_failures.saturating_sub(state.failures)),
                    "retry_scope": file.retry.as_ref().map(|retry| retry.scope),
                    "different_model": file.reviews.get(&item.id).is_some_and(|review| review.independence.different_model),
                }));
            }
        }
        serde_json::json!({"required": required, "pending": pending, "blocked": blocked})
    }

    pub fn pinned_run(&self, argv: &[String]) -> Option<String> {
        let file = self.file.as_ref()?;
        for item in file.setup.iter().chain(&file.items) {
            if item.kind == CloseoutKind::Review {
                continue;
            }
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
        let reported = if rebase_in_progress(workspace) {
            Vec::new()
        } else if let Some(paths) = paths_against_base(workspace, self.base_ref_name.as_deref()) {
            changed
                .iter()
                .filter(|path| paths.contains(*path))
                .cloned()
                .collect()
        } else {
            changed.clone()
        };
        for path in &changed {
            self.record_write(path);
        }
        changed = reported;
        self.snapshot = current;
        changed
    }
    pub(crate) fn active_written_paths(&self) -> Vec<String> {
        if self.written_paths.is_empty() {
            return Vec::new();
        }
        let paths = self
            .workspace
            .as_deref()
            .and_then(|workspace| paths_against_base(workspace, self.base_ref_name.as_deref()));
        self.written_paths
            .iter()
            .filter(|path| paths.as_ref().is_none_or(|paths| paths.contains(*path)))
            .cloned()
            .collect()
    }

    pub(crate) fn tracks_path(&self, path: &str) -> bool {
        self.workspace.as_deref().is_none_or(|workspace| {
            !rebase_in_progress(workspace)
                && paths_against_base(workspace, self.base_ref_name.as_deref())
                    .is_none_or(|paths| paths.contains(path))
        })
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

pub(crate) fn workspace_fingerprint(workspace: &Path, head: &str, status: &str) -> String {
    let paths: std::collections::BTreeMap<_, _> =
        workspace_snapshot(workspace).into_iter().collect();
    let bytes = serde_json::to_vec(&(head, status, paths)).unwrap();
    ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn git_text(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()?;
    output.status.success().then(|| {
        let text = String::from_utf8_lossy(&output.stdout);
        if args.contains(&"-z") {
            text.into_owned()
        } else {
            text.trim().to_string()
        }
    })
}

pub(crate) fn merge_base(workspace: &Path, base_ref_name: Option<&str>) -> Option<String> {
    let remote = git_text(
        workspace,
        &["rev-parse", "--symbolic-full-name", "@{upstream}"],
    )
    .and_then(|upstream| {
        upstream
            .strip_prefix("refs/remotes/")
            .and_then(|path| path.split('/').next())
            .map(str::to_string)
    })
    .unwrap_or_else(|| "origin".into());
    let target = if let Some(base) = base_ref_name {
        let remote_ref = format!("refs/remotes/{remote}/{base}");
        if git_text(workspace, &["rev-parse", "--verify", &remote_ref]).is_some() {
            remote_ref
        } else {
            base.to_string()
        }
    } else {
        git_text(
            workspace,
            &["symbolic-ref", &format!("refs/remotes/{remote}/HEAD")],
        )
        .unwrap_or_else(|| "origin/main".into())
    };
    if git_text(workspace, &["rev-parse", "--verify", "MERGE_HEAD"]).is_some() {
        git_text(workspace, &["merge-base", &target, "HEAD", "MERGE_HEAD"])
    } else {
        git_text(workspace, &["merge-base", &target, "HEAD"])
    }
}

fn rebase_in_progress(workspace: &Path) -> bool {
    ["rebase-merge", "rebase-apply"].iter().any(|name| {
        git_text(workspace, &["rev-parse", "--git-path", name])
            .is_some_and(|path| workspace.join(path).is_dir())
    })
}

fn paths_against_base(
    workspace: &Path,
    base_ref_name: Option<&str>,
) -> Option<std::collections::HashSet<String>> {
    if rebase_in_progress(workspace) {
        return None;
    }
    let base = merge_base(workspace, base_ref_name)?;
    let tracked = git_text(workspace, &["diff", "--name-only", "-z", &base, "--"])?;
    let untracked = git_text(
        workspace,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    Some(
        tracked
            .split('\0')
            .chain(untracked.split('\0'))
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

pub(crate) fn workspace_snapshot(workspace: &Path) -> HashMap<String, u64> {
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
    let parent = path.parent().unwrap_or(Path::new("."));
    let root = if matches!(
        parent.file_name().and_then(|name| name.to_str()),
        Some(".agents" | ".kyotoagent")
    ) {
        parent.parent().unwrap_or(Path::new("."))
    } else {
        parent
    };
    parse_from_root(path, root)
}

pub fn parse_from_root(path: &Path, root: &Path) -> Result<CloseoutFile, CloseoutError> {
    match load_file(path, root)? {
        Some(file) => Ok(file),
        None => Err(CloseoutError::Parse {
            path: path.to_path_buf(),
            source: "no such file".to_string(),
        }),
    }
}

fn load_file(path: &Path, root: &Path) -> Result<Option<CloseoutFile>, CloseoutError> {
    let Some(file) = load_document(path, false)? else {
        return Ok(None);
    };
    let mut stack = vec![path.canonicalize().map_err(|source| CloseoutError::Parse {
        path: path.to_path_buf(),
        source: source.to_string(),
    })?];
    let mut file = file.resolve_imports(root, &mut stack)?;
    file.policy_digest = file.digest(root)?;
    Ok(Some(file))
}

fn load_document(path: &Path, imported: bool) -> Result<Option<CloseoutFile>, CloseoutError> {
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
    if imported && value.get("version").is_some() {
        return Err(CloseoutError::BadDefinition {
            message: "imported policies need specVersion 0.1".into(),
        });
    }
    if imported && value.get("retry").is_some() {
        return Err(CloseoutError::BadDefinition {
            message: "retry is only allowed on the entry policy".into(),
        });
    }
    let raw = if value.get("version").is_some() {
        serde_yaml::from_str::<LegacyFile>(&text).map(RawFile::Legacy)
    } else {
        serde_yaml::from_str::<PublicFile>(&text).map(RawFile::Public)
    }
    .map_err(|source| CloseoutError::Parse {
        path: path.to_path_buf(),
        source: source.to_string(),
    })?;
    let mut file = CloseoutFile::from_raw(raw)?;
    file.policy_files
        .insert(path.to_path_buf(), retry::hash(text.as_bytes()));
    Ok(Some(file))
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
    items: Vec<PublicItem>,
    #[serde(default)]
    imports: Vec<CloseoutImport>,
    setup: Option<Vec<SetupStep>>,
    #[serde(default, deserialize_with = "present_retry")]
    retry: Option<RetryPolicy>,
}

fn present_retry<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<RetryPolicy>, D::Error> {
    RetryPolicy::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseoutImport {
    pub path: String,
    #[serde(rename = "as")]
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseoutReview {
    pub skill: String,
    #[serde(skip)]
    pub policy_skill: String,
    pub independence: ReviewIndependence,
    #[serde(rename = "failOn")]
    pub fail_on: Severity,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewIndependence {
    #[serde(rename = "differentSession")]
    pub different_session: bool,
    #[serde(rename = "differentModel")]
    pub different_model: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
pub enum Severity {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFinding {
    pub severity: Severity,
    pub location: String,
    pub explanation: String,
    pub evidence: String,
}

impl CloseoutReview {
    pub fn accepts(
        &self,
        candidate_session: &str,
        candidate_model: &str,
        reviewer_session: &str,
        reviewer_model: &str,
        findings: &[ReviewFinding],
    ) -> bool {
        let distinct = |required: bool, candidate: &str, reviewer: &str| {
            !required || (!candidate.is_empty() && !reviewer.is_empty() && candidate != reviewer)
        };
        distinct(
            self.independence.different_session,
            candidate_session,
            reviewer_session,
        ) && distinct(
            self.independence.different_model,
            candidate_model,
            reviewer_model,
        ) && findings.iter().all(|finding| {
            !finding.location.is_empty()
                && !finding.explanation.is_empty()
                && !finding.evidence.is_empty()
                && finding.severity > self.fail_on
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum PublicItem {
    Command(PublicCommand),
    Review(PublicReview),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicReview {
    id: String,
    kind: CloseoutKind,
    gate: String,
    skill: String,
    independence: ReviewIndependence,
    #[serde(rename = "failOn")]
    fail_on: Severity,
    paths: Option<Vec<String>>,
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
    fn resolve_imports(
        mut self,
        root: &Path,
        stack: &mut Vec<PathBuf>,
    ) -> Result<Self, CloseoutError> {
        for review in self.reviews.values_mut() {
            let skill = root.join(&review.skill);
            validate_skill(&skill)?;
            retry::skill_files(skill.parent().unwrap(), &mut self.policy_files)?;
            review.skill = skill
                .canonicalize()
                .map_err(|source| CloseoutError::Parse {
                    path: skill.clone(),
                    source: source.to_string(),
                })?
                .to_string_lossy()
                .into_owned();
        }
        let mut imported_items = Vec::new();
        for entry in std::mem::take(&mut self.imports) {
            let path = root.join(&entry.path);
            let canonical = path.canonicalize().map_err(|source| CloseoutError::Parse {
                path: path.clone(),
                source: source.to_string(),
            })?;
            let canonical_root = root.canonicalize().map_err(|source| CloseoutError::Parse {
                path: root.to_path_buf(),
                source: source.to_string(),
            })?;
            if !canonical.starts_with(&canonical_root) {
                return Err(CloseoutError::BadPath { path: entry.path });
            }
            if stack.contains(&canonical) {
                let chain = stack
                    .iter()
                    .chain(std::iter::once(&canonical))
                    .map(|path| {
                        path.strip_prefix(&canonical_root)
                            .unwrap_or(path)
                            .display()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(" -> ");
                return Err(CloseoutError::BadDefinition {
                    message: format!("import cycle: {chain}"),
                });
            }
            let child = load_document(&path, true)?.ok_or_else(|| CloseoutError::Parse {
                path: path.clone(),
                source: "import is missing".into(),
            })?;
            if !child.setup.is_empty() {
                return Err(CloseoutError::BadDefinition {
                    message: "setup is only allowed on the entry policy".into(),
                });
            }
            stack.push(canonical);
            let mut child = child.resolve_imports(root, stack)?;
            stack.pop();
            self.policy_files.extend(child.policy_files);
            for mut item in child.items {
                let id = format!("{}/{}", entry.name, item.id);
                if let Some(execution) = child.executions.remove(&item.id) {
                    self.executions.insert(id.clone(), execution);
                }
                if let Some(review) = child.reviews.remove(&item.id) {
                    self.reviews.insert(id.clone(), review);
                }
                item.id = id;
                if item.kind == CloseoutKind::Command {
                    item.hint = format!("Run {}", item.id);
                }
                imported_items.push(item);
            }
        }
        imported_items.append(&mut self.items);
        self.items = imported_items;
        Ok(self)
    }

    pub fn execution(&self, item: &CloseoutItem) -> (Vec<String>, Option<u64>) {
        match self.executions.get(&item.id) {
            Some(exec) => (exec.argv.clone(), Some(exec.timeout)),
            None => (vec!["sh".into(), "-c".into(), item.run.clone()], None),
        }
    }

    fn from_raw(raw: RawFile) -> Result<CloseoutFile, CloseoutError> {
        let mut reviews = HashMap::new();
        let mut imports = Vec::new();
        let mut retry = None;
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
                if let Some(policy) = &raw.retry {
                    policy.validate()?;
                }
                let limit = raw
                    .retry
                    .as_ref()
                    .map_or(u32::MAX, |policy| policy.max_failed_attempts_per_item);
                retry = raw.retry;
                if raw.spec_version != "0.1"
                    || raw.items.len() > 128
                    || raw.imports.len() > 64
                    || raw.description.as_ref().is_some_and(|description| {
                        description.is_empty() || description.len() > 500
                    })
                {
                    return Err(CloseoutError::BadDefinition {
                        message: "Invalid Closeout 0.1 policy".into(),
                    });
                }
                let mut names = std::collections::HashSet::new();
                for entry in &raw.imports {
                    if !is_valid_id(&entry.name)
                        || entry.name.ends_with('-')
                        || entry.name.contains("--")
                    {
                        return Err(CloseoutError::BadId {
                            id: entry.name.clone(),
                        });
                    }
                    if !names.insert(entry.name.clone()) {
                        return Err(CloseoutError::BadDefinition {
                            message: format!("duplicate import name {}", entry.name),
                        });
                    }
                    if !is_valid_import_path(&entry.path) {
                        return Err(CloseoutError::BadPath {
                            path: entry.path.clone(),
                        });
                    }
                }
                imports = raw.imports;
                let mut items = Vec::new();
                for item in raw.items {
                    let item = match item {
                        PublicItem::Command(item) => item,
                        PublicItem::Review(item) => {
                            validate_execution(
                                &item.id,
                                std::slice::from_ref(&item.skill),
                                1,
                                &item.paths,
                            )?;
                            if item.kind != CloseoutKind::Review
                                || item.gate != "beforePR"
                                || !is_valid_import_path(&item.skill)
                                || item.skill.len() > 256
                                || !item.skill.ends_with("/SKILL.md")
                            {
                                return Err(CloseoutError::BadDefinition { message: format!("{} needs kind review, gate beforePR and a skill path ending in /SKILL.md", item.id) });
                            }
                            reviews.insert(
                                item.id.clone(),
                                CloseoutReview {
                                    skill: item.skill.clone(),
                                    policy_skill: item.skill.clone(),
                                    independence: item.independence,
                                    fail_on: item.fail_on,
                                },
                            );
                            items.push(CloseoutItem {
                                id: item.id,
                                kind: CloseoutKind::Review,
                                run: String::new(),
                                hint: format!("Review using {}", item.skill),
                                paths: item.paths.unwrap_or_default(),
                            });
                            continue;
                        }
                    };
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
                (items, raw.setup, limit)
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
            if !item.kind.is_supported()
                || (item.kind == CloseoutKind::Review && !reviews.contains_key(&item.id))
            {
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
            imports,
            items,
            setup: steps,
            executions,
            reviews,
            max_failures,
            retry,
            policy_digest: String::new(),
            policy_files: Default::default(),
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

fn validate_skill(skill: &Path) -> Result<(), CloseoutError> {
    let metadata = std::fs::metadata(skill).map_err(|source| CloseoutError::Parse {
        path: skill.to_path_buf(),
        source: source.to_string(),
    })?;
    if !metadata.is_file() {
        return Err(CloseoutError::Parse {
            path: skill.to_path_buf(),
            source: "skill must be a regular file".into(),
        });
    }
    Ok(())
}

fn is_valid_import_path(path: &str) -> bool {
    is_valid_path_pattern(path) && !path.ends_with('/') && !path.contains(['*', '?', '[', ']'])
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
        workspace_fingerprint: String::new(),
        policy_digest: String::new(),
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
    fn public_retry_requires_a_bounded_limit_and_scope() {
        for scope in ["task", "candidate"] {
            let file: CloseoutFile = serde_yaml::from_str(&format!(
                "specVersion: '0.1'\nretry:\n  maxFailedAttemptsPerItem: 5\n  scope: {scope}\n"
            ))
            .unwrap();
            assert_eq!(file.max_failures, 5);
        }
        for retry in [
            "maxFailedAttemptsPerItem: 0\n  scope: task",
            "maxFailedAttemptsPerItem: 100001\n  scope: task",
            "maxFailedAttemptsPerItem: 5",
            "scope: task",
            "maxFailedAttemptsPerItem: 5\n  scope: session",
            "maxFailedAttemptsPerItem: 5\n  scope: task\n  extra: true",
        ] {
            assert!(serde_yaml::from_str::<CloseoutFile>(&format!(
                "specVersion: '0.1'\nretry:\n  {retry}\n"
            ))
            .is_err());
        }
        let file: CloseoutFile = serde_yaml::from_str("specVersion: '0.1'\n").unwrap();
        assert_eq!(file.max_failures, u32::MAX);
        assert!(serde_yaml::from_str::<CloseoutFile>("specVersion: '0.1'\nretry: null\n").is_err());
    }

    #[test]
    fn an_import_cannot_set_a_retry_limit() {
        let dir = temp_dir("import-retry");
        write_file(
            &dir,
            "specVersion: '0.1'\nimports:\n  - path: child.yaml\n    as: child\n",
        );
        std::fs::write(
            dir.join("child.yaml"),
            "specVersion: '0.1'\nretry:\n  maxFailedAttemptsPerItem: 5\n  scope: task\n",
        )
        .unwrap();
        assert!(read(&dir)
            .unwrap_err()
            .to_string()
            .contains("retry is only allowed on the entry policy"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retry_identity_changes_with_imported_policy_and_skill_bytes() {
        let dir = temp_dir("retry-digest");
        std::fs::create_dir_all(dir.join("skill")).unwrap();
        std::fs::write(dir.join("skill/SKILL.md"), "Review").unwrap();
        write_file(&dir, "specVersion: '0.1'\nretry:\n  maxFailedAttemptsPerItem: 5\n  scope: task\nimports:\n  - path: child.yaml\n    as: child\n");
        let child = "specVersion: '0.1'\nitems:\n  - id: review\n    kind: review\n    gate: beforePR\n    skill: skill/SKILL.md\n    independence:\n      differentSession: true\n      differentModel: true\n    failOn: P1\n";
        std::fs::write(dir.join("child.yaml"), child).unwrap();
        let first = read(&dir).unwrap().unwrap().policy_digest;
        std::fs::write(dir.join("child.yaml"), format!("{child}\n")).unwrap();
        let second = read(&dir).unwrap().unwrap().policy_digest;
        assert_ne!(first, second);
        std::fs::write(dir.join("skill/SKILL.md"), "New criteria").unwrap();
        assert_ne!(second, read(&dir).unwrap().unwrap().policy_digest);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn policy_digest_preserves_import_alias_paths_and_sorts_files_by_utf16() {
        let dir = temp_dir("digest-order");
        let child = "specVersion: '0.1'\n";
        for name in ["\u{e000}.yaml", "\u{10000}.yaml"] {
            std::fs::write(dir.join(name), child).unwrap();
        }
        std::os::unix::fs::symlink(dir.join("\u{e000}.yaml"), dir.join("alias.yaml")).unwrap();
        let entry = "specVersion: '0.1'\nimports:\n  - path: \u{e000}.yaml\n    as: bmp\n  - path: \u{10000}.yaml\n    as: astral\n  - path: alias.yaml\n    as: alias\n";
        write_file(&dir, entry);
        let files: Vec<_> = [(".kyotoagent/closeout.yaml", entry), ("alias.yaml", child), ("\u{10000}.yaml", child), ("\u{e000}.yaml", child)].into_iter().map(|(path, contents)| serde_json::json!({"path":path,"sha256":retry::hash(contents.as_bytes())})).collect();
        let expected = serde_json::json!({"specVersion":"0.1","files":files,"items":[]});
        assert_eq!(
            read(&dir).unwrap().unwrap().policy_digest,
            format!(
                "sha256:{}",
                retry::hash(&serde_json::to_vec(&expected).unwrap())
            )
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nested_imports_resolve_from_the_root_in_depth_first_order() {
        let dir = temp_dir("imports");
        std::fs::create_dir_all(dir.join("policies")).unwrap();
        write_file(&dir, "specVersion: '0.1'\nimports:\n  - path: policies/outer.yaml\n    as: quality\nitems:\n  - id: entry\n    kind: command\n    gate: beforePR\n    exec: [echo, entry]\n    timeoutSeconds: 30\n");
        std::fs::write(dir.join("policies/outer.yaml"), "specVersion: '0.1'\nimports:\n  - path: policies/inner.yaml\n    as: nested\nitems:\n  - id: outer\n    kind: command\n    gate: beforePR\n    exec: [echo, outer]\n    timeoutSeconds: 40\n").unwrap();
        std::fs::write(dir.join("policies/inner.yaml"), "specVersion: '0.1'\nitems:\n  - id: inner\n    kind: command\n    gate: beforePR\n    exec: [echo, inner]\n    timeoutSeconds: 50\n    paths: ['src/**']\n").unwrap();
        let file = read(&dir).unwrap().unwrap();
        assert_eq!(
            file.items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["quality/nested/inner", "quality/outer", "entry"]
        );
        assert_eq!(
            file.execution(&file.items[0]),
            (vec!["echo".into(), "inner".into()], Some(50))
        );
        let mut state = CloseoutState::with_file(&dir, Some(file));
        state.record_write("src/lib.rs");
        assert!(state
            .cannot_finish()
            .unwrap()
            .contains("quality/nested/inner"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn imports_reject_missing_files_cycles_duplicate_names_and_imported_setup() {
        for (name, child, imports, expected) in [
            ("missing", None, "  - path: child.yaml\n    as: child\n", "child.yaml"),
            ("cycle", Some("specVersion: '0.1'\nimports:\n  - path: .kyotoagent/closeout.yaml\n    as: entry\n"), "  - path: child.yaml\n    as: child\n", "import cycle:"),
            ("duplicate", Some("specVersion: '0.1'\n"), "  - path: child.yaml\n    as: child\n  - path: child.yaml\n    as: child\n", "duplicate import name"),
            ("setup", Some("specVersion: '0.1'\nsetup:\n  - id: prepare\n    exec: [echo, ready]\n    timeoutSeconds: 5\n"), "  - path: child.yaml\n    as: child\n", "setup is only allowed"),
            ("legacy", Some("version: 1\nitems: []\n"), "  - path: child.yaml\n    as: child\n", "imported policies need specVersion"),
        ] {
            let dir = temp_dir(name);
            write_file(&dir, &format!("specVersion: '0.1'\nimports:\n{imports}"));
            if let Some(child) = child { std::fs::write(dir.join("child.yaml"), child).unwrap(); }
            assert!(read(&dir).unwrap_err().to_string().contains(expected));
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn import_paths_and_names_must_stay_inside_the_repository() {
        for path in [
            "../outside.yaml",
            "/outside.yaml",
            "~/.agents/closeout.yaml",
            "a/./b.yaml",
            "a//b.yaml",
            "a\\b.yaml",
            "C:/outside.yaml",
            "a/",
        ] {
            let dir = temp_dir("unsafe-import");
            write_file(
                &dir,
                &format!("specVersion: '0.1'\nimports:\n  - path: '{path}'\n    as: child\n"),
            );
            assert!(read(&dir).is_err(), "{path}");
            std::fs::remove_dir_all(dir).unwrap();
        }
        for name in ["Bad", "child--name", "child-", "a/b"] {
            let dir = temp_dir("unsafe-namespace");
            write_file(
                &dir,
                &format!("specVersion: '0.1'\nimports:\n  - path: child.yaml\n    as: '{name}'\n"),
            );
            assert!(read(&dir).is_err(), "{name}");
            std::fs::remove_dir_all(dir).unwrap();
        }
        let dir = temp_dir("symlink-import");
        let outside = temp_dir("outside-import");
        std::fs::write(outside.join("policy.yaml"), "specVersion: '0.1'\n").unwrap();
        std::os::unix::fs::symlink(outside.join("policy.yaml"), dir.join("child.yaml")).unwrap();
        write_file(
            &dir,
            "specVersion: '0.1'\nimports:\n  - path: child.yaml\n    as: child\n",
        );
        assert!(read(&dir).unwrap_err().to_string().contains("child.yaml"));
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn one_file_can_be_imported_under_two_names() {
        let dir = temp_dir("shared-import");
        std::fs::write(dir.join("child.yaml"), "specVersion: '0.1'\nitems:\n  - id: test\n    kind: command\n    gate: beforePR\n    exec: [echo, ok]\n    timeoutSeconds: 5\n").unwrap();
        write_file(&dir, "specVersion: '0.1'\nimports:\n  - path: child.yaml\n    as: first\n  - path: child.yaml\n    as: second\n");
        let file = read(&dir).unwrap().unwrap();
        assert_eq!(
            file.items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["first/test", "second/test"]
        );
        assert_eq!(file.executions.len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn review_severity_and_independence_are_evaluated_by_the_host() {
        let mut review = CloseoutReview {
            policy_skill: String::new(),
            skill: ".agents/skills/review/SKILL.md".into(),
            independence: ReviewIndependence {
                different_session: true,
                different_model: true,
            },
            fail_on: Severity::P1,
        };
        assert!(review.accepts("candidate", "writer", "reviewer", "reader", &[]));
        assert!(!review.accepts("candidate", "writer", "candidate", "reader", &[]));
        assert!(!review.accepts("candidate", "writer", "reviewer", "writer", &[]));
        assert!(!review.accepts("", "writer", "reviewer", "reader", &[]));
        assert!(!review.accepts("candidate", "", "reviewer", "reader", &[]));
        for severity in [Severity::P0, Severity::P1, Severity::P2, Severity::P3] {
            let finding = ReviewFinding {
                severity,
                location: "src/lib.rs:1".into(),
                explanation: "Finding".into(),
                evidence: "Observed behavior".into(),
            };
            assert_eq!(
                review.accepts("candidate", "writer", "reviewer", "reader", &[finding]),
                severity > Severity::P1
            );
        }
        review.independence = ReviewIndependence {
            different_session: false,
            different_model: false,
        };
        assert!(review.accepts("", "", "", "", &[]));
        let finding = ReviewFinding {
            severity: Severity::P3,
            location: String::new(),
            explanation: "Finding".into(),
            evidence: "Observed behavior".into(),
        };
        assert!(!review.accepts("candidate", "writer", "reviewer", "reader", &[finding]));
    }

    #[test]
    fn reviews_allow_symlinked_skill_files_and_directories() {
        let dir = temp_dir("review-skill");
        write_file(&dir, "specVersion: '0.1'\nitems:\n  - id: review\n    kind: review\n    gate: beforePR\n    skill: skills/review/SKILL.md\n    independence:\n      differentSession: true\n      differentModel: false\n    failOn: P1\n");
        assert!(read(&dir).is_err());
        std::fs::create_dir_all(dir.join("skills/review")).unwrap();
        std::fs::write(dir.join("skills/review/SKILL.md"), "Review changes").unwrap();
        let file = read(&dir).unwrap().unwrap();
        assert_eq!(file.items[0].kind, CloseoutKind::Review);
        assert_eq!(file.reviews["review"].fail_on, Severity::P1);
        std::os::unix::fs::symlink("SKILL.md", dir.join("skills/review/link")).unwrap();
        let linked = read(&dir).unwrap().unwrap();
        assert_eq!(
            linked.policy_files[&dir.join("skills/review/link")],
            retry::hash(b"Review changes")
        );
        std::fs::create_dir_all(dir.join("shared")).unwrap();
        std::fs::write(dir.join("shared/reference.md"), "Review reference").unwrap();
        std::os::unix::fs::symlink("../../shared", dir.join("skills/review/references")).unwrap();
        let before = read(&dir).unwrap().unwrap();
        assert_eq!(
            before.policy_files[&dir.join("skills/review/references/reference.md")],
            retry::hash(b"Review reference")
        );
        std::fs::write(dir.join("shared/reference.md"), "Updated reference").unwrap();
        let after = read(&dir).unwrap().unwrap();
        assert_ne!(before.policy_digest, after.policy_digest);
        std::os::unix::fs::symlink("missing", dir.join("skills/review/broken")).unwrap();
        assert!(read(&dir).is_err());
        std::fs::remove_file(dir.join("skills/review/broken")).unwrap();
        std::os::unix::fs::symlink(".", dir.join("skills/review/cycle")).unwrap();
        assert!(read(&dir).unwrap_err().to_string().contains("cycle"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reviews_follow_shared_skill_symlinks_outside_the_repository() {
        for directory_link in [false, true] {
            let dir = temp_dir("external-review-skill");
            let shared = temp_dir("shared-review-skill");
            write_file(&dir, "specVersion: '0.1'\nitems:\n  - id: review\n    kind: review\n    gate: beforePR\n    skill: skills/review/SKILL.md\n    independence:\n      differentSession: true\n      differentModel: false\n    failOn: P1\n");
            std::fs::write(shared.join("SKILL.md"), "Shared review").unwrap();
            std::fs::create_dir_all(dir.join("skills")).unwrap();
            if directory_link {
                std::os::unix::fs::symlink(&shared, dir.join("skills/review")).unwrap();
                std::fs::write(shared.join("reference.md"), "Shared reference").unwrap();
            } else {
                std::fs::create_dir_all(dir.join("skills/review")).unwrap();
                std::os::unix::fs::symlink(
                    shared.join("SKILL.md"),
                    dir.join("skills/review/SKILL.md"),
                )
                .unwrap();
            }
            let before = read(&dir).unwrap().unwrap();
            assert_eq!(
                before.reviews["review"].skill,
                shared.join("SKILL.md").to_string_lossy()
            );
            assert_eq!(
                before.policy_files[&dir.join("skills/review/SKILL.md")],
                retry::hash(b"Shared review")
            );
            if directory_link {
                assert_eq!(
                    before.policy_files[&dir.join("skills/review/reference.md")],
                    retry::hash(b"Shared reference")
                );
            }
            std::fs::write(shared.join("SKILL.md"), "Updated shared review").unwrap();
            let after = read(&dir).unwrap().unwrap();
            assert_ne!(before.policy_digest, after.policy_digest);
            std::fs::remove_dir_all(dir).unwrap();
            std::fs::remove_dir_all(shared).unwrap();
        }
    }

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
    fn git(dir: &Path, args: &[&str]) -> std::process::Output {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[test]
    fn merges_only_require_checks_for_paths_that_differ_from_the_base() {
        for conflict in [false, true] {
            let dir = temp_dir(if conflict { "merge-conflict" } else { "merge" });
            write_file(&dir, "version: 1\nitems:\n  - id: engine-local\n    kind: command\n    run: true\n    hint: Check engine\n    paths: ['apps/engine/**']\n  - id: worker-local\n    kind: command\n    run: true\n    hint: Check worker\n    paths: ['apps/worker/**']\n");
            git(&dir, &["init", "-b", "main"]);
            git(&dir, &["config", "user.name", "Closeout Test"]);
            git(&dir, &["config", "user.email", "closeout@example.test"]);
            for path in ["apps/engine/x", "apps/worker/y"] {
                std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
                std::fs::write(dir.join(path), "base\n").unwrap();
            }
            git(&dir, &["add", "."]);
            git(&dir, &["commit", "-m", "Base"]);
            git(&dir, &["checkout", "-b", "feature"]);
            std::fs::write(dir.join("apps/engine/x"), "feature\n").unwrap();
            git(&dir, &["commit", "-am", "Engine"]);
            git(&dir, &["checkout", "main"]);
            std::fs::write(dir.join("apps/worker/y"), "main\n").unwrap();
            if conflict {
                std::fs::write(dir.join("apps/engine/x"), "main\n").unwrap();
            }
            git(&dir, &["commit", "-am", "Main"]);
            git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
            git(&dir, &["checkout", "feature"]);
            let mut state = CloseoutState::new(&dir).unwrap();
            state.record_write("apps/engine/x");
            state.record_write("apps/worker/y");
            let merged = std::process::Command::new("git")
                .args(["merge", "origin/main", "--no-edit"])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert_eq!(merged.status.success(), !conflict);
            if !conflict {
                std::fs::write(dir.join("apps/engine/x"), "updated feature\n").unwrap();
            }
            assert_eq!(state.refresh_workspace(&dir), vec!["apps/engine/x"]);
            let items = &state.file.as_ref().unwrap().items;
            assert!(state.is_required(&items[0]));
            assert!(!state.is_required(&items[1]));
            assert!(state.refresh_workspace(&dir).is_empty());
            if conflict {
                std::fs::write(dir.join("apps/engine/x"), "resolved feature\n").unwrap();
                git(&dir, &["add", "."]);
                git(&dir, &["commit", "-m", "Resolve merge"]);
                state.refresh_workspace(&dir);
                assert!(!state.is_required(&state.file.as_ref().unwrap().items[1]));
            }
            git(
                &dir,
                &[
                    "restore",
                    "--source=origin/main",
                    "--staged",
                    "--worktree",
                    "apps/engine/x",
                ],
            );
            assert!(!state.is_required(&state.file.as_ref().unwrap().items[0]));
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn base_selection_prefers_the_pr_then_the_upstream_default_then_origin_main() {
        let dir = temp_dir("base-selection");
        git(&dir, &["init", "-b", "main"]);
        git(&dir, &["config", "user.name", "Closeout Test"]);
        git(&dir, &["config", "user.email", "closeout@example.test"]);
        std::fs::write(dir.join("file"), "main").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "Main"]);
        let main = git_text(&dir, &["rev-parse", "HEAD"]).unwrap();
        git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(dir.join("file"), "default").unwrap();
        git(&dir, &["commit", "-am", "Default"]);
        let default = git_text(&dir, &["rev-parse", "HEAD"]).unwrap();
        git(
            &dir,
            &["update-ref", "refs/remotes/upstream/develop", "HEAD"],
        );
        git(
            &dir,
            &[
                "symbolic-ref",
                "refs/remotes/upstream/HEAD",
                "refs/remotes/upstream/develop",
            ],
        );
        std::fs::write(dir.join("file"), "release").unwrap();
        git(&dir, &["commit", "-am", "Release"]);
        let release = git_text(&dir, &["rev-parse", "HEAD"]).unwrap();
        git(
            &dir,
            &["update-ref", "refs/remotes/upstream/release", "HEAD"],
        );
        git(&dir, &["remote", "add", "upstream", "."]);
        git(&dir, &["config", "branch.main.remote", "upstream"]);
        git(&dir, &["config", "branch.main.merge", "refs/heads/develop"]);
        assert_eq!(merge_base(&dir, Some("release")), Some(release));
        assert_eq!(merge_base(&dir, None), Some(default));
        git(
            &dir,
            &["symbolic-ref", "--delete", "refs/remotes/upstream/HEAD"],
        );
        assert_eq!(merge_base(&dir, None), Some(main));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restoring_one_of_two_changed_paths_invalidates_the_pass() {
        let dir = temp_dir("restore-one");
        write_file(&dir, "version: 1\nitems:\n  - id: test\n    kind: command\n    run: cmp a b\n    hint: Compare the files\n");
        git(&dir, &["init", "-b", "main"]);
        git(&dir, &["config", "user.name", "Closeout Test"]);
        git(&dir, &["config", "user.email", "closeout@example.test"]);
        std::fs::write(dir.join("a"), "base\n").unwrap();
        std::fs::write(dir.join("b"), "base\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "Base"]);
        git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(dir.join("a"), "changed\n").unwrap();
        std::fs::write(dir.join("b"), "changed\n").unwrap();
        let mut state = CloseoutState::new(&dir).unwrap();
        state.refresh_workspace(&dir);
        state.item_mut("test").passed = true;
        std::fs::write(dir.join("a"), "base\n").unwrap();
        state.refresh_workspace(&dir);
        assert!(
            !state.item_mut("test").passed,
            "restoring one changed path must invalidate the pass"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refresh_skips_rebase_changes_and_updates_the_snapshot() {
        let dir = temp_dir("rebase");
        write_file(&dir, "version: 1\nitems:\n  - id: test\n    kind: command\n    run: true\n    hint: Check changes\n");
        git(&dir, &["init", "-b", "main"]);
        git(&dir, &["config", "user.name", "Closeout Test"]);
        git(&dir, &["config", "user.email", "closeout@example.test"]);
        std::fs::write(dir.join("file"), "base\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "Base"]);
        git(&dir, &["checkout", "-b", "feature"]);
        std::fs::write(dir.join("file"), "feature\n").unwrap();
        git(&dir, &["commit", "-am", "Feature"]);
        git(&dir, &["checkout", "main"]);
        std::fs::write(dir.join("file"), "main\n").unwrap();
        std::fs::write(dir.join("imported"), "main\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "Main"]);
        git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&dir, &["checkout", "feature"]);
        let mut state = CloseoutState::new(&dir).unwrap();
        let rebased = std::process::Command::new("git")
            .args(["rebase", "origin/main"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(!rebased.status.success());
        assert!(rebase_in_progress(&dir));
        state.item_mut("test").passed = true;
        assert!(state.refresh_workspace(&dir).is_empty());
        assert!(
            !state.item_mut("test").passed,
            "a rebase mutation must invalidate the pass before path filtering"
        );
        state.written_paths.clear();
        assert!(!state.tracks_path("imported"));
        std::fs::write(dir.join("file"), "resolved feature\n").unwrap();
        git(&dir, &["add", "file"]);
        git(&dir, &["-c", "core.editor=true", "rebase", "--continue"]);
        assert_eq!(state.refresh_workspace(&dir), vec!["file"]);
        assert!(state.refresh_workspace(&dir).is_empty());
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
