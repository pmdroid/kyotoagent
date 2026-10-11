use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::*;
use crate::events::now;
use crate::screen::Status;
use crate::session::{Session, SessionMeta};
use crate::subagent::{self, Isolation, SpawnInput};

const KILL_BUDGET: Duration = Duration::from_secs(5);

impl Runner {
    pub async fn spawn_subagent(&self, session_id: &str, args: &Value) -> String {
        self.spawn_child(session_id, args, None).await
    }

    pub(super) async fn spawn_closeout_reviewer(
        &self,
        session_id: &str,
        args: &Value,
        skill_directory: &Path,
    ) -> String {
        self.spawn_child(session_id, args, Some(skill_directory))
            .await
    }

    async fn spawn_child(
        &self,
        session_id: &str,
        args: &Value,
        skill_directory: Option<&Path>,
    ) -> String {
        let input = match subagent::parse_spawn(args) {
            Ok(input) => input,
            Err(error) => return error,
        };
        let Some(parent_state) = self.session_state(session_id) else {
            return "no such session".to_string();
        };
        let parent = match parent_state.session.meta() {
            Ok(meta) => meta,
            Err(error) => return error.to_string(),
        };
        if parent.parent_id.is_some() {
            return subagent::DEPTH_ERROR.to_string();
        }
        if parent.archived {
            return "the session is archived".to_string();
        }
        let catalog = self.list_models().await;
        let current = self.current_config();
        let selection = match subagent::resolve_model(
            input.model.as_deref(),
            &crate::session::SessionModel {
                model: parent.model.clone(),
                effort: parent.effort.clone(),
                provider: parent
                    .model_override
                    .as_ref()
                    .and_then(|selection| selection.provider.clone())
                    .or_else(|| current.provider.clone()),
            },
            &catalog,
            current.provider.as_deref(),
        ) {
            Ok(selection) => selection,
            Err(error) => return error,
        };
        if let Some(from) = input.resume_from.clone() {
            return self.resume_child(&parent, &input, &from, selection).await;
        }
        let id = subagent::new_id(
            parent_state
                .session
                .dir()
                .parent()
                .unwrap_or(parent_state.session.dir()),
        );
        let workspace = match self.child_workspace(&parent, &input, &id) {
            Ok(workspace) => workspace,
            Err(error) => return error,
        };
        let mut meta = SessionMeta::new(&id, &workspace, &selection.model, &now());
        meta.parent_id = Some(parent.id.clone());
        meta.closeout_reviewer = skill_directory.is_some();
        if let Some(directory) = skill_directory {
            meta.allow
                .outside_read_paths
                .push(directory.to_string_lossy().into_owned());
        }
        meta.description = Some(input.description.clone());
        meta.title = Some(input.description.clone());
        meta.isolation = Some(input.isolation.label().to_string());
        meta.hidden = !input.visible;
        meta.yolo = parent.yolo;
        meta.enhance = false;
        meta.show_closeout = parent.show_closeout;
        meta.profile = parent.profile.clone();
        meta.effort = selection.effort.clone();
        meta.model_override = Some(selection);
        meta.requested_workspace = parent
            .requested_workspace
            .clone()
            .or_else(|| Some(parent.workspace.clone()));
        meta.status = Status::Working;
        let child = Session::at(
            &parent_state
                .session
                .dir()
                .parent()
                .unwrap_or(parent_state.session.dir())
                .join(&id),
        );
        if let Err(error) = child.create(&meta) {
            if input.isolation == Isolation::Worktree {
                subagent::remove_worktree(PathBuf::from(&parent.workspace).as_path(), &workspace);
            }
            return error.to_string();
        }
        if let Err(error) = self.add_session(&child) {
            let _ = fs::remove_dir_all(child.dir());
            if input.isolation == Isolation::Worktree {
                subagent::remove_worktree(PathBuf::from(&parent.workspace).as_path(), &workspace);
            }
            return error.to_string();
        }
        self.track_child(
            &id,
            &parent.id,
            input.background && skill_directory.is_none(),
        );
        let Some(child_state) = self.session_state(&id) else {
            self.forget_child(&id);
            return "no such session".to_string();
        };
        if let Err(error) = self.start_child(&child_state, &id, &input.prompt) {
            return error;
        }
        self.finish_spawn(&child, &id, input.background).await
    }

    pub async fn check_task(
        &self,
        session_id: &str,
        ids: &[&str],
        timeout_sec: Option<u64>,
    ) -> String {
        if ids.is_empty() {
            return "check_task needs an id".to_string();
        }
        if timeout_sec.unwrap_or(0) > 0 {
            let disarmed = self.disarm_child_wakes(session_id, ids);
            let budget = Duration::from_secs(timeout_sec.unwrap_or(0));
            let start = tokio::time::Instant::now();
            while start.elapsed() < budget && !self.listed_idle(session_id, ids) {
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            self.rearm_running_wakes(session_id, &disarmed);
        }
        let reports: Vec<String> = ids
            .iter()
            .map(|id| self.task_report(session_id, id))
            .collect();
        if reports.len() == 1 {
            return reports.into_iter().next().unwrap_or_default();
        }
        let values: Vec<Value> = reports
            .iter()
            .map(|report| {
                serde_json::from_str(report).unwrap_or_else(|_| Value::String(report.clone()))
            })
            .collect();
        serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_string())
    }

    pub async fn archive_session(&self, caller: &str, id: &str) -> String {
        let Some(state) = self.session_state(id) else {
            return "no such session".to_string();
        };
        match state.session.meta() {
            Ok(meta) if meta.archived => return format!("archived {id}"),
            Ok(_) => {}
            Err(error) => return error.to_string(),
        }
        if caller != id {
            self.retire(id).await;
        } else if let Some(state) = self.session_state(id) {
            state.retiring.store(true, Ordering::Release);
            self.cancel(id);
        }
        match state.session.set_archived(true, &now()) {
            Ok(()) => format!("archived {id}"),
            Err(error) => error.to_string(),
        }
    }

    pub async fn kill_task(&self, session_id: &str, id: &str) -> String {
        if let Some(state) = self.session_state(session_id) {
            if state.tools.tasks().check(id).is_ok() {
                if state.tools.tasks().is_running(id) {
                    state.tools.tasks().cancel_id(id);
                    let start = tokio::time::Instant::now();
                    while state.tools.tasks().is_running(id) && start.elapsed() < KILL_BUDGET {
                        tokio::time::sleep(Duration::from_millis(15)).await;
                    }
                }
                return match state.tools.tasks().check(id) {
                    Ok(view) => view.summary(),
                    Err(error) => error,
                };
            }
        }
        if let Some(child) = self.child_session(session_id, id) {
            self.cancel(id);
            let _ = subagent::wait_until_idle(&child.session, KILL_BUDGET).await;
            let meta = child.session.meta().ok();
            let dir = child.session.dir().to_path_buf();
            self.finish_for_delete(id).await;
            if let Some(meta) = meta.as_ref() {
                if meta.isolation.as_deref() == Some("worktree") {
                    let dest = PathBuf::from(&meta.workspace);
                    let repo = self
                        .session_state(session_id)
                        .and_then(|parent| parent.session.meta().ok())
                        .map(|parent| PathBuf::from(parent.workspace))
                        .unwrap_or_else(|| dest.clone());
                    subagent::remove_worktree(&repo, &dest);
                }
            }
            self.forget(id);
            match fs::remove_dir_all(&dir) {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return source.to_string(),
            }
            return format!("closed {id}");
        }
        format!("no task {id} in this session")
    }

    pub(super) fn settle_child(&self, state: &Arc<SessionState>) {
        let Ok(meta) = state.session.meta() else {
            return;
        };
        let Some(parent_id) = meta.parent_id.clone() else {
            return;
        };
        let wake = {
            let mut runs = self.child_runs.lock().expect("child runs");
            let Some(slot) = runs.get_mut(&meta.id) else {
                return;
            };
            slot.settled = true;
            slot.wake
        };
        if !wake {
            return;
        }
        self.flush_parent_wakes(&parent_id);
    }

    fn flush_parent_wakes(&self, parent_id: &str) {
        let ids = {
            let mut runs = self.child_runs.lock().expect("child runs");
            let waiting = runs
                .values()
                .any(|run| run.parent_id == parent_id && run.wake && !run.settled);
            if waiting {
                return;
            }
            let mut batch: Vec<(u64, String)> = runs
                .iter()
                .filter(|(_, run)| run.parent_id == parent_id && run.wake && run.settled)
                .map(|(id, run)| (run.seq, id.clone()))
                .collect();
            batch.sort_by_key(|(seq, _)| *seq);
            for (_, id) in &batch {
                if let Some(slot) = runs.get_mut(id) {
                    slot.wake = false;
                }
            }
            batch.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
        };
        if ids.is_empty() {
            return;
        }
        let mut snapshots = Vec::new();
        for id in &ids {
            let Some(child) = self.session_state(id) else {
                continue;
            };
            snapshots.push(subagent::snapshot(&child.session));
        }
        if snapshots.is_empty() {
            return;
        }
        let Some(parent) = self.session_state(parent_id) else {
            return;
        };
        let labels: Vec<&str> = snapshots.iter().map(|snap| snap.id.as_str()).collect();
        self.queue_or_start(
            &parent,
            subagent::wake_ask(&labels),
            subagent::wake_contexts(&snapshots),
            None,
            true,
        );
    }

    async fn resume_child(
        &self,
        parent: &SessionMeta,
        input: &SpawnInput,
        from: &str,
        selection: crate::session::SessionModel,
    ) -> String {
        let Some(child_state) = self.child_session(&parent.id, from) else {
            return "resume_from needs a finished child of this session".to_string();
        };
        let child_meta = match child_state.session.meta() {
            Ok(meta) => meta,
            Err(error) => return error.to_string(),
        };
        if child_meta.status != Status::Idle {
            return "resume_from needs a finished child of this session".to_string();
        }
        if input.model.is_some() {
            if let Err(error) = child_state.session.set_session_model(selection) {
                return error.to_string();
            }
        }
        self.track_child(from, &parent.id, input.background);
        let _ = child_state.session.update(|meta| {
            meta.status = Status::Working;
            true
        });
        if let Err(error) = self.start_child(&child_state, from, &input.prompt) {
            return error;
        }
        self.finish_spawn(&child_state.session, from, input.background)
            .await
    }

    fn start_child(&self, state: &Arc<SessionState>, id: &str, prompt: &str) -> Result<(), String> {
        match self.start_turn(state, prompt, "", None, false, Vec::new()) {
            Ok(TurnStart::Id(_)) => Ok(()),
            Ok(TurnStart::Busy) => Err("the child session is already working".to_string()),
            Ok(TurnStart::Spent) => {
                self.forget_child(id);
                Err("the child session did not start".to_string())
            }
            Err(error) => {
                self.forget_child(id);
                Err(error.to_string())
            }
        }
    }

    async fn finish_spawn(&self, session: &Session, id: &str, background: bool) -> String {
        if !background {
            let idle = subagent::wait_until_idle(session, subagent::FOREGROUND_BUDGET).await;
            if !idle {
                self.arm_child_wake(id);
            }
        }
        subagent::snapshot(session).json()
    }

    fn forget_child(&self, id: &str) {
        self.child_runs.lock().expect("child runs").remove(id);
    }

    fn track_child(&self, id: &str, parent_id: &str, wake: bool) {
        let mut runs = self.child_runs.lock().expect("child runs");
        let seq = runs
            .values()
            .map(|run| run.seq)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        runs.insert(
            id.to_string(),
            ChildRun {
                wake,
                settled: false,
                seq,
                parent_id: parent_id.to_string(),
            },
        );
    }

    fn disarm_child_wakes(&self, session_id: &str, ids: &[&str]) -> Vec<String> {
        let children: Vec<String> = ids
            .iter()
            .filter(|id| self.child_session(session_id, id).is_some())
            .map(|id| (*id).to_string())
            .collect();
        let disarmed = {
            let mut runs = self.child_runs.lock().expect("child runs");
            let mut cleared = Vec::new();
            for id in children {
                if let Some(slot) = runs.get_mut(&id) {
                    if slot.wake {
                        slot.wake = false;
                        cleared.push(id);
                    }
                }
            }
            cleared
        };
        if !disarmed.is_empty() {
            self.flush_parent_wakes(session_id);
        }
        disarmed
    }

    fn rearm_running_wakes(&self, session_id: &str, ids: &[String]) {
        let running: Vec<String> = ids
            .iter()
            .filter(|id| !self.one_idle(session_id, id))
            .cloned()
            .collect();
        if running.is_empty() {
            return;
        }
        let mut runs = self.child_runs.lock().expect("child runs");
        for id in running {
            if let Some(slot) = runs.get_mut(&id) {
                if !slot.settled {
                    slot.wake = true;
                }
            }
        }
    }

    fn listed_idle(&self, session_id: &str, ids: &[&str]) -> bool {
        ids.iter().all(|id| self.one_idle(session_id, id))
    }

    fn one_idle(&self, session_id: &str, id: &str) -> bool {
        if let Some(state) = self.session_state(session_id) {
            if state.tools.tasks().check(id).is_ok() {
                return !state.tools.tasks().is_running(id);
            }
        }
        if let Some(child) = self.child_session(session_id, id) {
            return child
                .session
                .meta()
                .map(|meta| meta.status == Status::Idle)
                .unwrap_or(true);
        }
        true
    }

    fn task_report(&self, session_id: &str, id: &str) -> String {
        if let Some(state) = self.session_state(session_id) {
            if let Ok(view) = state.tools.tasks().check(id) {
                return view.summary();
            }
        }
        if let Some(child) = self.child_session(session_id, id) {
            return subagent::snapshot(&child.session).json();
        }
        format!("no task {id} in this session")
    }

    fn arm_child_wake(&self, id: &str) {
        let mut runs = self.child_runs.lock().expect("child runs");
        let slot = runs.entry(id.to_string()).or_default();
        if !slot.settled {
            slot.wake = true;
        }
    }

    fn child_workspace(
        &self,
        parent: &SessionMeta,
        input: &SpawnInput,
        id: &str,
    ) -> Result<PathBuf, String> {
        let parent_workspace = PathBuf::from(&parent.workspace);
        match input.isolation {
            Isolation::Worktree => subagent::add_worktree(&parent_workspace, id),
            Isolation::None => {
                if let Some(cwd) = input.cwd.as_deref() {
                    subagent::resolve_cwd(&parent_workspace, cwd, &parent.allow)
                } else {
                    fs::canonicalize(&parent_workspace).map_err(|error| error.to_string())
                }
            }
        }
    }

    fn child_session(&self, parent_id: &str, id: &str) -> Option<Arc<SessionState>> {
        let state = self.session_state(id)?;
        let meta = state.session.meta().ok()?;
        if meta.parent_id.as_deref() == Some(parent_id) {
            Some(state)
        } else {
            None
        }
    }

    pub(super) fn session_state(&self, id: &str) -> Option<Arc<SessionState>> {
        self.sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(id)
            .cloned()
    }
}
