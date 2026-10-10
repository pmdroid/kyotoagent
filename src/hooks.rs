use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;

pub const TIMEOUT_SECS: u64 = 30;
pub const KYOTO_HOOKS_FILE: &str = ".kyotoagent/hooks.json";
pub const KYOTO_HOOKS_DIR: &str = ".kyotoagent/hooks";
pub const AGENTS_HOOKS_FILE: &str = ".agents/hooks.json";
pub const AGENTS_HOOKS_DIR: &str = ".agents/hooks";

const WRITE_ALIASES: &[&str] = &[
    "Write",
    "Edit",
    "MultiEdit",
    "write",
    "search_replace",
    "write_file",
];
const RUN_ALIASES: &[&str] = &["Bash", "run_terminal_command", "run"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    PreToolUse,
    PostToolUse,
    Stop,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct HookFile {
    #[serde(default)]
    hooks: HookEvents,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct HookEvents {
    #[serde(default, rename = "PreToolUse")]
    pre_tool_use: Vec<HookGroup>,
    #[serde(default, rename = "PostToolUse")]
    post_tool_use: Vec<HookGroup>,
    #[serde(default, rename = "Stop")]
    stop: Vec<HookGroup>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct HookGroup {
    #[serde(default)]
    matcher: String,
    #[serde(default)]
    hooks: Vec<HookHandler>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct HookHandler {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    command: String,
}

#[derive(Clone, Debug)]
struct Group {
    matcher: String,
    commands: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Hooks {
    home: PathBuf,
    pre: Vec<Group>,
    post: Vec<Group>,
    stop: Vec<Group>,
    sandbox: bool,
}

enum RunEnd {
    Exit(i32, String),
    Timeout,
    Crash(String),
}

pub fn fires(tool: &str) -> bool {
    tool == "write_file" || tool == "search_replace" || tool == "run"
}

pub fn load(workspace: &Path) -> Hooks {
    match home_dir() {
        Some(home) => load_in(workspace, &home),
        None => load_in(workspace, &PathBuf::new()),
    }
}

pub fn load_in(workspace: &Path, home: &Path) -> Hooks {
    let mut loaded = Hooks {
        home: home.to_path_buf(),
        ..Hooks::default()
    };
    for place in places(workspace, home) {
        load_place(&mut loaded, &place);
    }
    loaded
}

impl Hooks {
    pub fn with_sandbox(mut self, enabled: bool) -> Self {
        self.sandbox = enabled;
        self
    }

    pub async fn pre_tool_use(
        &self,
        workspace: &Path,
        tool: &str,
        input: &Value,
    ) -> Option<String> {
        self.run_first(
            Event::PreToolUse,
            workspace,
            tool,
            &payload(tool, input, None),
            true,
        )
        .await
    }

    pub async fn post_tool_use(
        &self,
        workspace: &Path,
        tool: &str,
        input: &Value,
        result: &str,
    ) -> Option<String> {
        self.run_all(
            Event::PostToolUse,
            workspace,
            tool,
            &payload(tool, input, Some(result)),
        )
        .await
    }

    pub async fn stop(&self, workspace: &Path) -> Option<String> {
        self.run_first(
            Event::Stop,
            workspace,
            "",
            &serde_json::json!({ "reason": "end_turn" }),
            false,
        )
        .await
    }

    fn commands_for(&self, event: Event, tool: &str) -> Vec<String> {
        groups(self, event)
            .iter()
            .filter(|group| event == Event::Stop || matcher_hits(&group.matcher, tool))
            .flat_map(|group| group.commands.iter().cloned())
            .collect()
    }

    async fn run_first(
        &self,
        event: Event,
        workspace: &Path,
        tool: &str,
        payload: &Value,
        fail_closed: bool,
    ) -> Option<String> {
        let commands = self.commands_for(event, tool);
        if commands.is_empty() {
            return None;
        }
        let body = payload.to_string();
        for command in &commands {
            match invoke(command, &self.home, workspace, &body, self.sandbox).await {
                RunEnd::Exit(2, stderr) => {
                    return Some(reason(stderr, "blocked by a hook"));
                }
                RunEnd::Exit(0, _) => {}
                RunEnd::Timeout => {
                    if fail_closed {
                        return Some("the hook timed out".to_string());
                    }
                }
                RunEnd::Exit(_, stderr) | RunEnd::Crash(stderr) => {
                    if fail_closed {
                        return Some(reason(stderr, "the hook failed"));
                    }
                }
            }
        }
        None
    }

    async fn run_all(
        &self,
        event: Event,
        workspace: &Path,
        tool: &str,
        payload: &Value,
    ) -> Option<String> {
        let commands = self.commands_for(event, tool);
        if commands.is_empty() {
            return None;
        }
        let body = payload.to_string();
        let mut notes = Vec::new();
        for command in &commands {
            if let RunEnd::Exit(2, stderr) =
                invoke(command, &self.home, workspace, &body, self.sandbox).await
            {
                notes.push(reason(stderr, "blocked by a hook"));
            }
        }
        if notes.is_empty() {
            None
        } else {
            Some(notes.join("\n\n"))
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = PathBuf::from(home);
    if home.as_os_str().is_empty() {
        None
    } else {
        Some(home)
    }
}

fn places(workspace: &Path, home: &Path) -> Vec<PathBuf> {
    let mut places = Vec::new();
    if !home.as_os_str().is_empty() {
        places.push(home.to_path_buf());
    }
    let root = project_root(workspace);
    places.extend(directories(&root, workspace));
    places
}

fn project_root(workspace: &Path) -> PathBuf {
    let mut current = workspace.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return current;
        }
        if !current.pop() {
            return workspace.to_path_buf();
        }
    }
}

fn directories(root: &Path, workspace: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = workspace.to_path_buf();
    loop {
        dirs.push(current.clone());
        if current == root {
            break;
        }
        if !current.pop() {
            break;
        }
    }
    dirs.reverse();
    dirs
}

fn load_place(into: &mut Hooks, base: &Path) {
    for file in [KYOTO_HOOKS_FILE, AGENTS_HOOKS_FILE] {
        merge(into, read_file(&base.join(file)));
    }
    for dir in [KYOTO_HOOKS_DIR, AGENTS_HOOKS_DIR] {
        for path in json_files(&base.join(dir)) {
            merge(into, read_file(&path));
        }
    }
    for dir in [KYOTO_HOOKS_DIR, AGENTS_HOOKS_DIR] {
        bind_scripts(into, &base.join(dir));
    }
}

fn bind_scripts(into: &mut Hooks, dir: &Path) {
    for path in script_files(dir) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(event) = event_for_script(name) else {
            continue;
        };
        if names_loaded_file(into, name) {
            continue;
        }
        push_script(into, event, script_command(&path));
    }
}

fn script_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| script_extension(path) && path.is_file())
        .collect();
    files.sort();
    files
}

fn script_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("py") | Some("sh")
    )
}

fn event_for_script(name: &str) -> Option<Event> {
    if name.starts_with("pre_") {
        Some(Event::PreToolUse)
    } else if name.starts_with("post_") {
        Some(Event::PostToolUse)
    } else if name.starts_with("stop_") {
        Some(Event::Stop)
    } else {
        None
    }
}

fn script_command(path: &Path) -> String {
    if path.extension().and_then(|ext| ext.to_str()) == Some("py") {
        format!("python3 {}", path.display())
    } else {
        path.display().to_string()
    }
}

fn push_script(into: &mut Hooks, event: Event, command: String) {
    let group = Group {
        matcher: String::new(),
        commands: vec![command],
    };
    match event {
        Event::PreToolUse => into.pre.push(group),
        Event::PostToolUse => into.post.push(group),
        Event::Stop => into.stop.push(group),
    }
}

fn names_loaded_file(hooks: &Hooks, filename: &str) -> bool {
    hooks
        .pre
        .iter()
        .chain(hooks.post.iter())
        .chain(hooks.stop.iter())
        .flat_map(|group| group.commands.iter())
        .any(|command| command_names_file(command, filename))
}

fn command_names_file(command: &str, filename: &str) -> bool {
    command
        .split_whitespace()
        .any(|part| Path::new(part).file_name().and_then(|name| name.to_str()) == Some(filename))
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json")
        })
        .collect();
    files.sort();
    files
}

fn read_file(path: &Path) -> HookFile {
    let Ok(text) = fs::read_to_string(path) else {
        return HookFile::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn merge(into: &mut Hooks, file: HookFile) {
    into.pre.extend(groups_from(file.hooks.pre_tool_use));
    into.post.extend(groups_from(file.hooks.post_tool_use));
    into.stop.extend(groups_from(file.hooks.stop));
}

fn groups_from(groups: Vec<HookGroup>) -> Vec<Group> {
    groups
        .into_iter()
        .filter_map(|group| {
            let commands: Vec<String> = group
                .hooks
                .into_iter()
                .filter(|handler| handler.kind == "command" && !handler.command.is_empty())
                .map(|handler| handler.command)
                .collect();
            if commands.is_empty() {
                None
            } else {
                Some(Group {
                    matcher: group.matcher,
                    commands,
                })
            }
        })
        .collect()
}

fn groups(hooks: &Hooks, event: Event) -> &[Group] {
    match event {
        Event::PreToolUse => &hooks.pre,
        Event::PostToolUse => &hooks.post,
        Event::Stop => &hooks.stop,
    }
}

fn matcher_hits(matcher: &str, tool: &str) -> bool {
    let matcher = matcher.trim();
    if matcher.is_empty() {
        return true;
    }
    let names = aliases(tool);
    matcher
        .split('|')
        .any(|piece| names.iter().any(|alias| *alias == piece.trim()))
}

fn aliases(tool: &str) -> &'static [&'static str] {
    match tool {
        "write_file" | "search_replace" => WRITE_ALIASES,
        "run" => RUN_ALIASES,
        _ => &[],
    }
}

fn expand_home(command: &str, home: &Path) -> String {
    command.replace("$HOME", &home.to_string_lossy())
}

fn payload(tool: &str, input: &Value, result: Option<&str>) -> Value {
    let mut tool_input = match input {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    if tool == "run" {
        if let Some(argv) = tool_input.get("argv").and_then(Value::as_array) {
            let command = argv
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            tool_input.insert("command".to_string(), Value::String(command));
        }
    }
    let mut body = Map::new();
    body.insert("tool_name".to_string(), Value::String(tool.to_string()));
    body.insert("tool_input".to_string(), Value::Object(tool_input));
    if let Some(result) = result {
        body.insert("tool_result".to_string(), Value::String(result.to_string()));
    }
    Value::Object(body)
}

fn reason(stderr: String, fallback: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

async fn invoke(
    command: &str,
    home: &Path,
    workspace: &Path,
    payload: &str,
    sandbox: bool,
) -> RunEnd {
    let command = expand_home(command, home);
    let command = match crate::operator_config::command("sh", &["-c".into(), command], sandbox) {
        Ok(command) => command,
        Err(error) => return RunEnd::Crash(error.to_string()),
    };
    let mut child = match TokioCommand::from(command)
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return RunEnd::Crash(error.to_string()),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes()).await;
    }
    match tokio::time::timeout(Duration::from_secs(TIMEOUT_SECS), child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            match output.status.code() {
                Some(code) => RunEnd::Exit(code, stderr),
                None => RunEnd::Crash(stderr),
            }
        }
        Ok(Err(error)) => RunEnd::Crash(error.to_string()),
        Err(_) => RunEnd::Timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kyotoagent-hooks-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the directory exists");
        dir
    }

    fn write_json(path: &Path, value: &Value) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("the parent exists");
        }
        fs::write(path, value.to_string()).expect("the file writes");
    }

    fn group(event: &str, matcher: &str, command: &str) -> Value {
        serde_json::json!({
            "hooks": {
                event: [{
                    "matcher": matcher,
                    "hooks": [{ "type": "command", "command": command }]
                }]
            }
        })
    }

    #[test]
    fn a_missing_hooks_directory_loads_nothing() {
        let root = temp_dir("missing");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let hooks = load_in(&workspace, &root.join("home"));
        assert!(hooks.pre.is_empty());
        assert!(hooks.post.is_empty());
        assert!(hooks.stop.is_empty());
        assert!(hooks
            .commands_for(Event::PreToolUse, "write_file")
            .is_empty());
    }

    #[test]
    fn files_merge_in_home_then_workspace_order() {
        let root = temp_dir("merge");
        let home = root.join("home");
        let workspace = root.join("w");
        write_json(
            &home.join(AGENTS_HOOKS_FILE),
            &group("PreToolUse", "write_file", "home-file"),
        );
        write_json(
            &home.join(AGENTS_HOOKS_DIR).join("b.json"),
            &group("PreToolUse", "Write", "home-dir"),
        );
        write_json(
            &workspace.join(AGENTS_HOOKS_FILE),
            &group("PreToolUse", "Edit", "workspace-file"),
        );
        write_json(
            &workspace.join(AGENTS_HOOKS_DIR).join("a.json"),
            &group("PreToolUse", "write", "workspace-dir"),
        );
        write_json(
            &home.join(AGENTS_HOOKS_DIR).join("skip.py"),
            &serde_json::json!({ "hooks": { "PreToolUse": [] } }),
        );

        let hooks = load_in(&workspace, &home);
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "write_file"),
            vec![
                "home-file".to_string(),
                "home-dir".to_string(),
                "workspace-file".to_string(),
                "workspace-dir".to_string(),
            ]
        );
    }

    #[test]
    fn a_run_matcher_misses_a_write() {
        let root = temp_dir("miss");
        let workspace = root.join("w");
        write_json(
            &workspace.join(AGENTS_HOOKS_FILE),
            &group("PreToolUse", "run", "deny-run"),
        );
        let hooks = load_in(&workspace, &PathBuf::new());
        assert!(hooks
            .commands_for(Event::PreToolUse, "write_file")
            .is_empty());
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "run"),
            vec!["deny-run".to_string()]
        );
        assert!(hooks
            .commands_for(Event::PreToolUse, "read_file")
            .is_empty());
    }

    #[test]
    fn write_and_run_aliases_match_kyotoagent_tools() {
        assert!(matcher_hits("Write", "write_file"));
        assert!(matcher_hits("Edit", "write_file"));
        assert!(matcher_hits("MultiEdit", "write_file"));
        assert!(matcher_hits("write", "write_file"));
        assert!(matcher_hits("search_replace", "write_file"));
        assert!(matcher_hits("write_file", "write_file"));
        assert!(matcher_hits("Bash", "run"));
        assert!(matcher_hits("run_terminal_command", "run"));
        assert!(matcher_hits("run", "run"));
        assert!(matcher_hits("", "write_file"));
        assert!(matcher_hits("Bash|run_terminal_command", "run"));
        assert!(!matcher_hits("Bash|run_terminal_command", "write_file"));
        assert!(matcher_hits(
            "Write|Edit|MultiEdit|write|search_replace",
            "write_file"
        ));
        assert!(matcher_hits(
            "Write|Edit|MultiEdit|write|search_replace",
            "search_replace"
        ));
        assert!(!matcher_hits(
            "Write|Edit|MultiEdit|write|search_replace",
            "run"
        ));
        assert!(matcher_hits(" Nope | Bash ", "run"));
        assert!(!matcher_hits("Nope|Also", "write_file"));
        assert!(!matcher_hits("Bash", "write_file"));
        assert!(!matcher_hits("Write", "run"));
    }

    #[test]
    fn home_in_the_command_string_expands() {
        let home = PathBuf::from("/tmp/agent-home");
        assert_eq!(
            expand_home("python3 $HOME/.agents/hooks/guard.py --hook", &home),
            "python3 /tmp/agent-home/.agents/hooks/guard.py --hook"
        );
    }

    #[test]
    fn a_run_payload_carries_a_joined_command() {
        let payload = payload(
            "run",
            &serde_json::json!({ "argv": ["gh", "pr", "create"] }),
            None,
        );
        assert_eq!(payload["tool_name"], "run");
        assert_eq!(payload["tool_input"]["command"], "gh pr create");
        assert_eq!(
            payload["tool_input"]["argv"],
            serde_json::json!(["gh", "pr", "create"])
        );
    }

    #[tokio::test]
    async fn a_command_that_exits_two_returns_stderr_and_keeps_stdin() {
        let root = temp_dir("exit-two");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        fs::write(
            workspace.join("hook.sh"),
            "cat > stdin.json\necho denied-pre >&2\nexit 2\n",
        )
        .expect("the script writes");
        write_json(
            &workspace.join(AGENTS_HOOKS_FILE),
            &group("PreToolUse", "write_file", "sh ./hook.sh"),
        );
        let hooks = load_in(&workspace, &root.join("home"));
        let reason = hooks
            .pre_tool_use(
                &workspace,
                "write_file",
                &serde_json::json!({ "path": "notes.md", "contents": "hi" }),
            )
            .await;
        assert_eq!(reason.as_deref(), Some("denied-pre"));
        let stdin = fs::read_to_string(workspace.join("stdin.json")).expect("stdin was written");
        let body: Value = serde_json::from_str(&stdin).expect("stdin is json");
        assert_eq!(body["tool_name"], "write_file");
        assert_eq!(body["tool_input"]["path"], "notes.md");
    }

    #[tokio::test]
    async fn a_command_that_exits_zero_allows_the_tool() {
        let root = temp_dir("exit-zero");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        fs::write(workspace.join("hook.sh"), "cat > stdin.json\nexit 0\n")
            .expect("the script writes");
        write_json(
            &workspace.join(AGENTS_HOOKS_FILE),
            &group("PreToolUse", "write_file", "sh ./hook.sh"),
        );
        let hooks = load_in(&workspace, &root.join("home"));
        let reason = hooks
            .pre_tool_use(
                &workspace,
                "write_file",
                &serde_json::json!({ "path": "notes.md", "contents": "hi" }),
            )
            .await;
        assert_eq!(reason, None);
        assert!(workspace.join("stdin.json").exists());
    }

    #[test]
    fn todo_does_not_fire_hooks() {
        assert!(fires("write_file"));
        assert!(fires("search_replace"));
        assert!(fires("run"));
        assert!(!fires("todo"));
        assert!(!fires("ask"));
        assert!(!fires("read_file"));
    }

    fn write_script(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("the parent exists");
        }
        fs::write(path, "#!/bin/sh\nexit 0\n").expect("the script writes");
    }

    fn assert_pipe_home(label: &str, relative: &str) {
        let root = temp_dir(label);
        let home = root.join("home");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        write_json(
            &home.join(relative),
            &group("PreToolUse", "Bash|run_terminal_command", "guard-run"),
        );
        let hooks = load_in(&workspace, &home);
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "run"),
            vec!["guard-run".to_string()]
        );
        assert!(hooks
            .commands_for(Event::PreToolUse, "write_file")
            .is_empty());
    }

    #[test]
    fn grok_codex_and_cursor_files_add_no_command() {
        let root = temp_dir("ignore");
        let home = root.join("home");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        write_json(
            &home.join(".grok/hooks/grok.json"),
            &group("PreToolUse", "", "from-grok"),
        );
        write_json(
            &home.join(".grok/hooks/orca-status.json"),
            &group("Stop", "", "from-orca"),
        );
        write_json(
            &home.join(".codex/hooks.json"),
            &group("PreToolUse", "", "from-codex"),
        );
        write_json(
            &home.join(".kyotoagent/hooks/cursor.json"),
            &serde_json::json!({
                "version": 1,
                "hooks": [{ "command": "from-cursor" }]
            }),
        );
        let hooks = load_in(&workspace, &home);
        assert!(hooks.commands_for(Event::PreToolUse, "run").is_empty());
        assert!(hooks
            .commands_for(Event::PreToolUse, "write_file")
            .is_empty());
        assert!(hooks.commands_for(Event::Stop, "").is_empty());
    }

    #[test]
    fn a_kyoto_home_pipe_matcher_hits_run_only() {
        assert_pipe_home("kyoto-pipe", ".kyotoagent/hooks.json");
    }

    #[test]
    fn an_agents_home_pipe_matcher_hits_run_only() {
        assert_pipe_home("agents-pipe", ".agents/hooks.json");
    }

    #[test]
    fn a_repo_ancestor_loads_kyoto_and_agents_hook_json() {
        let root = temp_dir("nested");
        let repo = root.join("repo");
        let workspace = repo.join("pkg");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        fs::create_dir_all(repo.join(".git")).expect("the git dir exists");
        write_json(
            &repo.join(".kyotoagent/hooks/extra.json"),
            &group("PreToolUse", "Bash|run_terminal_command", "from-extra"),
        );
        write_json(
            &repo.join(".agents/hooks/hooks.json"),
            &group(
                "PostToolUse",
                "Write|Edit|MultiEdit|write|search_replace",
                "from-agents",
            ),
        );
        write_json(
            &root.join(".kyotoagent/hooks.json"),
            &group("PreToolUse", "", "from-outside"),
        );
        let hooks = load_in(&workspace, &root.join("home"));
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "run"),
            vec!["from-extra".to_string()]
        );
        assert!(hooks
            .commands_for(Event::PreToolUse, "write_file")
            .is_empty());
        assert_eq!(
            hooks.commands_for(Event::PostToolUse, "write_file"),
            vec!["from-agents".to_string()]
        );
        assert_eq!(
            hooks.commands_for(Event::PostToolUse, "search_replace"),
            vec!["from-agents".to_string()]
        );
    }

    #[test]
    fn kyoto_json_loads_before_agents_json() {
        let root = temp_dir("order");
        let home = root.join("home");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        write_json(
            &home.join(".kyotoagent/hooks.json"),
            &group("PreToolUse", "", "kyoto-file"),
        );
        write_json(
            &home.join(".agents/hooks.json"),
            &group("PreToolUse", "", "agents-file"),
        );
        write_json(
            &home.join(".kyotoagent/hooks/b.json"),
            &group("PreToolUse", "", "kyoto-dir"),
        );
        write_json(
            &home.join(".agents/hooks/a.json"),
            &group("PreToolUse", "", "agents-dir"),
        );
        let hooks = load_in(&workspace, &home);
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "run"),
            vec![
                "kyoto-file".to_string(),
                "agents-file".to_string(),
                "kyoto-dir".to_string(),
                "agents-dir".to_string(),
            ]
        );
    }

    #[test]
    fn a_kyoto_pre_script_binds_for_every_firing_tool() {
        let root = temp_dir("pre-script");
        let home = root.join("home");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let script = home.join(".kyotoagent/hooks/pre_example.py");
        write_script(&script);
        let hooks = load_in(&workspace, &home);
        let expected = format!("python3 {}", script.display());
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "run"),
            vec![expected.clone()]
        );
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "write_file"),
            vec![expected.clone()]
        );
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "search_replace"),
            vec![expected]
        );
    }

    #[test]
    fn a_symlinked_pre_script_outside_the_directory_binds() {
        let root = temp_dir("pre-link");
        let home = root.join("home");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let outside = root.join("real.py");
        fs::write(&outside, "print(1)\n").expect("the target writes");
        let script = home.join(".kyotoagent/hooks/pre_example.py");
        fs::create_dir_all(script.parent().expect("a parent")).expect("the hooks dir exists");
        std::os::unix::fs::symlink(&outside, &script).expect("the link exists");
        let hooks = load_in(&workspace, &home);
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "write_file"),
            vec![format!("python3 {}", script.display())]
        );
    }

    #[test]
    fn an_agents_pre_script_binds_when_kyoto_has_none() {
        let root = temp_dir("agents-script");
        let home = root.join("home");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let script = home.join(".agents/hooks/pre_example.py");
        write_script(&script);
        let hooks = load_in(&workspace, &home);
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "run"),
            vec![format!("python3 {}", script.display())]
        );
    }

    #[test]
    fn the_kyoto_script_binds_when_both_folders_have_the_same_name() {
        let root = temp_dir("both-scripts");
        let home = root.join("home");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let kyoto = home.join(".kyotoagent/hooks/pre_example.py");
        let agents = home.join(".agents/hooks/pre_example.py");
        write_script(&kyoto);
        write_script(&agents);
        let hooks = load_in(&workspace, &home);
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "run"),
            vec![format!("python3 {}", kyoto.display())]
        );
    }

    #[test]
    fn a_stop_script_binds_and_other_names_do_not() {
        let root = temp_dir("stop-script");
        let home = root.join("home");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let stop = home.join(".kyotoagent/hooks/stop_example.sh");
        let notes = home.join(".kyotoagent/hooks/notes.py");
        let post = home.join(".agents/hooks/post_example.py");
        write_script(&stop);
        write_script(&notes);
        write_script(&post);
        let hooks = load_in(&workspace, &home);
        assert_eq!(
            hooks.commands_for(Event::Stop, ""),
            vec![stop.display().to_string()]
        );
        let post_command = format!("python3 {}", post.display());
        assert_eq!(
            hooks.commands_for(Event::PostToolUse, "run"),
            vec![post_command.clone()]
        );
        assert_eq!(
            hooks.commands_for(Event::PostToolUse, "write_file"),
            vec![post_command]
        );
        assert!(hooks.commands_for(Event::PreToolUse, "run").is_empty());
        assert!(hooks.pre.iter().all(|group| group
            .commands
            .iter()
            .all(|command| !command.contains("notes.py"))));
        assert!(hooks.post.iter().all(|group| group
            .commands
            .iter()
            .all(|command| !command.contains("notes.py"))));
        assert!(hooks.stop.iter().all(|group| group
            .commands
            .iter()
            .all(|command| !command.contains("notes.py"))));
    }

    #[test]
    fn a_script_named_by_hooks_json_is_not_added_again() {
        let root = temp_dir("named-script");
        let home = root.join("home");
        let workspace = root.join("w");
        fs::create_dir_all(&workspace).expect("the workspace exists");
        let script = home.join(".kyotoagent/hooks/pre_example.py");
        write_script(&script);
        let command = format!("python3 {} --hook", script.display());
        write_json(
            &home.join(".kyotoagent/hooks.json"),
            &group("PreToolUse", "", &command),
        );
        let hooks = load_in(&workspace, &home);
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "run"),
            vec![command.clone()]
        );
        assert_eq!(
            hooks.commands_for(Event::PreToolUse, "write_file"),
            vec![command]
        );
    }
}
