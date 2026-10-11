use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::chat::ModelRow;
use crate::events::{EventKind, ProofBody, ResultBody};
use crate::screen::Status;
use crate::session::{AllowList, Session, SessionModel};

pub const FOREGROUND_BUDGET: Duration = Duration::from_secs(60);
pub const DEPTH_ERROR: &str = "spawn_subagent is refused at depth 1";
pub const CWD_WORKTREE_ERROR: &str = "cwd and worktree cannot both be set";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Isolation {
    None,
    Worktree,
}

impl Isolation {
    pub fn label(self) -> &'static str {
        match self {
            Isolation::None => "none",
            Isolation::Worktree => "worktree",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnInput {
    pub prompt: String,
    pub description: String,
    pub background: bool,
    pub isolation: Isolation,
    pub resume_from: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub visible: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Snapshot {
    pub id: String,
    pub state: String,
    pub result: String,
    pub proof: String,
}

impl Snapshot {
    pub fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                "{{\"id\":\"{}\",\"state\":\"{}\",\"result\":\"\",\"proof\":\"\"}}",
                self.id, self.state
            )
        })
    }
}

pub fn parse_spawn(args: &Value) -> Result<SpawnInput, String> {
    let prompt = required_string(args, "prompt", "spawn_subagent needs a prompt")?;
    let description = required_string(args, "description", "spawn_subagent needs a description")?;
    let background = match args.get("run_in_background") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("run_in_background must be a boolean".to_string()),
    };
    let isolation = match args.get("isolation").and_then(Value::as_str) {
        None | Some("") | Some("none") => Isolation::None,
        Some("worktree") => Isolation::Worktree,
        Some(_) => return Err("isolation must be none or worktree".to_string()),
    };
    let resume_from = optional_string(args, "resume_from");
    let cwd = optional_string(args, "cwd");
    let model = optional_string(args, "model");
    let visible = match args.get("visible") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("visible must be a boolean".to_string()),
    };
    if cwd.is_some() && isolation == Isolation::Worktree {
        return Err(CWD_WORKTREE_ERROR.to_string());
    }
    Ok(SpawnInput {
        prompt,
        description,
        background,
        isolation,
        resume_from,
        cwd,
        model,
        visible,
    })
}

pub fn resolve_model(
    requested: Option<&str>,
    parent: &SessionModel,
    catalog: &[ModelRow],
    current_provider: Option<&str>,
) -> Result<SessionModel, String> {
    let Some(requested) = requested.map(str::trim).filter(|model| !model.is_empty()) else {
        return Ok(parent.clone());
    };
    if requested == parent.model {
        return Ok(parent.clone());
    }
    let matches: Vec<&ModelRow> = catalog
        .iter()
        .filter(|row| row.matches(requested))
        .collect();
    let row = match matches.as_slice() {
        [row] => *row,
        [] => return Err(format!("model is not in the catalog: {requested}")),
        _ => {
            return Err(format!(
                "model {requested} is available from more than one provider; set the provider explicitly"
            ))
        }
    };
    let provider = row
        .provider
        .clone()
        .or_else(|| current_provider.map(str::to_string));
    let effort = match parent.effort.as_deref() {
        Some(effort)
            if !row.reasoning_efforts.is_empty()
                && !row.reasoning_efforts.iter().any(|known| known == effort) =>
        {
            return Err(format!(
                "reasoning effort {effort} is not available for {requested}"
            ))
        }
        other => other.map(str::to_string),
    };
    Ok(SessionModel {
        model: requested.to_string(),
        effort,
        provider,
    })
}

pub fn resolve_cwd(parent: &Path, cwd: &str, allow: &AllowList) -> Result<PathBuf, String> {
    let raw = Path::new(cwd);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        parent.join(raw)
    };
    let canon = fs::canonicalize(&joined).map_err(|_| format!("cwd does not exist: {cwd}"))?;
    let parent_canon =
        fs::canonicalize(parent).map_err(|_| "the workspace does not exist".to_string())?;
    if canon.starts_with(&parent_canon) {
        return Ok(canon);
    }
    let allowed = allow
        .write_paths
        .iter()
        .chain(allow.outside_read_paths.iter())
        .any(|path| {
            fs::canonicalize(path)
                .map(|allowed| canon == allowed || canon.starts_with(&allowed))
                .unwrap_or(false)
        });
    if allowed {
        Ok(canon)
    } else {
        Err(format!("cwd is outside the workspace: {cwd}"))
    }
}

pub fn add_worktree(parent_workspace: &Path, id: &str) -> Result<PathBuf, String> {
    let toplevel = git_toplevel(parent_workspace)?;
    let dest = worktree_dest(&toplevel, id);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let branch = format!("kyotoagent/{id}");
    let dest_text = dest.to_string_lossy().into_owned();
    let output = Command::new("git")
        .args(["worktree", "add", "-b", &branch, &dest_text, "HEAD"])
        .current_dir(&toplevel)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(&dest);
        return Err(command_text(&output));
    }
    Ok(dest)
}

pub fn remove_worktree(repo: &Path, dest: &Path) {
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force", &dest.to_string_lossy()])
        .current_dir(repo)
        .status();
    let _ = fs::remove_dir_all(dest);
}

pub fn snapshot(session: &Session) -> Snapshot {
    let meta = session.meta().ok();
    let id = meta
        .as_ref()
        .map(|meta| meta.id.clone())
        .unwrap_or_default();
    let status = meta
        .as_ref()
        .map(|meta| meta.status)
        .unwrap_or(Status::Idle);
    let (result, proof) = latest_finish(session);
    let state = match status {
        Status::Working => "working",
        Status::Waiting => "waiting",
        Status::Idle if result == "Stopped." => "cancelled",
        Status::Idle => "idle",
    };
    Snapshot {
        id,
        state: state.to_string(),
        result,
        proof,
    }
}

pub fn wake_ask(ids: &[&str]) -> String {
    match ids {
        [_] => "Subagent finished.".to_string(),
        _ => "Subagents finished.".to_string(),
    }
}

pub fn wake_context(snapshot: &Snapshot) -> String {
    format!(
        "result: {}\nproof: {}\nresume_from=\"{}\"\n",
        snapshot.result, snapshot.proof, snapshot.id
    )
}

pub fn wake_contexts(snapshots: &[Snapshot]) -> String {
    snapshots
        .iter()
        .map(wake_context)
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn wait_until_idle(session: &Session, budget: Duration) -> bool {
    let start = tokio::time::Instant::now();
    loop {
        let idle = session
            .meta()
            .map(|meta| meta.status == Status::Idle)
            .unwrap_or(true);
        if idle {
            return true;
        }
        if start.elapsed() >= budget {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

pub fn new_id(root: &Path) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.subsec_nanos() as u64)
            .unwrap_or(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let bits = nanos.wrapping_mul(0x9e37_79b9)
            ^ count.wrapping_mul(0x85eb_ca6b)
            ^ (std::process::id() as u64);
        let id = format!("{:08x}", bits as u32);
        if !root.join(&id).exists() {
            return id;
        }
    }
}

fn latest_finish(session: &Session) -> (String, String) {
    let Ok(events) = session.events() else {
        return (String::new(), String::new());
    };
    let mut result = String::new();
    let mut proof = String::new();
    for event in events {
        match event.kind {
            EventKind::Result => {
                if let Ok(body) = event.body_as::<ResultBody>() {
                    result = body.text;
                }
            }
            EventKind::Proof => {
                if let Ok(body) = event.body_as::<ProofBody>() {
                    proof = body.text;
                }
            }
            _ => {}
        }
    }
    (result, proof)
}

fn required_string(args: &Value, key: &str, missing: &str) -> Result<String, String> {
    match args.get(key).and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.to_string()),
        _ => Err(missing.to_string()),
    }
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn git_toplevel(workspace: &Path) -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(workspace)
        .output()
        .map_err(|_| "the workspace is not a git repository".to_string())?;
    if !output.status.success() {
        return Err("the workspace is not a git repository".to_string());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Err("the workspace is not a git repository".to_string());
    }
    Ok(PathBuf::from(path))
}

fn worktree_dest(toplevel: &Path, id: &str) -> PathBuf {
    let name = toplevel
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "repo".to_string());
    let parent = toplevel.parent().unwrap_or(toplevel);
    parent
        .join(format!(".{name}-kyotoagent-worktrees"))
        .join(id)
}

fn command_text(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = stdout.trim();
    if stdout.is_empty() {
        "git worktree add failed".to_string()
    } else {
        stdout.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cwd_and_worktree_are_refused_before_a_session_exists() {
        let error = parse_spawn(&json!({
            "prompt": "do the thing",
            "description": "Do the thing",
            "isolation": "worktree",
            "cwd": "/tmp",
        }))
        .expect_err("the pair is refused");
        assert!(error.contains("cwd and worktree"), "{error}");
    }

    #[test]
    fn one_child_wakes_alone_and_several_share_one_ask() {
        assert_eq!(wake_ask(&["aa"]), "Subagent finished.");
        assert!(!wake_ask(&["aa"]).contains("aa"));
        assert_eq!(wake_ask(&["aa", "bb"]), "Subagents finished.");
        assert!(!wake_ask(&["aa", "bb"]).contains("aa"));
        let context = wake_contexts(&[
            Snapshot {
                id: "aa".to_string(),
                state: "idle".to_string(),
                result: "r1".to_string(),
                proof: "p1".to_string(),
            },
            Snapshot {
                id: "bb".to_string(),
                state: "idle".to_string(),
                result: "r2".to_string(),
                proof: "p2".to_string(),
            },
        ]);
        assert_eq!(
            context,
            "result: r1\nproof: p1\nresume_from=\"aa\"\n\nresult: r2\nproof: p2\nresume_from=\"bb\"\n"
        );
    }

    #[test]
    fn a_missing_background_flag_runs_in_the_background() {
        let input = parse_spawn(&json!({
            "prompt": "do the thing",
            "description": "Do the thing",
        }))
        .expect("the input parses");
        assert!(input.background);
        assert_eq!(input.isolation, Isolation::None);
        assert!(input.cwd.is_none());
        assert!(input.resume_from.is_none());
        assert!(!input.visible);
    }

    #[test]
    fn visible_defaults_off_and_rejects_a_non_boolean() {
        let shown = parse_spawn(&json!({
            "prompt": "do the thing",
            "description": "Do the thing",
            "visible": true,
        }))
        .expect("the input parses");
        assert!(shown.visible);
        let error = parse_spawn(&json!({
            "prompt": "do the thing",
            "description": "Do the thing",
            "visible": "yes",
        }))
        .expect_err("a string is refused");
        assert!(error.contains("visible"), "{error}");
    }
}
