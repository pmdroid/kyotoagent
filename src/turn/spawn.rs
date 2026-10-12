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

    pub async fn create_session(&self, session_id: &str, args: &Value) -> String {
        if !matches!(
            args.get("lifetime").and_then(Value::as_str),
            Some("temporary" | "lasting")
        ) {
            return "lifetime must be temporary or lasting".into();
        }
        if args.get("resume_from").is_some() {
            return "Use message_session to follow up with an existing child.".into();
        }
        self.spawn_child(session_id, args, None).await
    }

    pub fn list_children(&self, session_id: &str) -> String {
        let children = self.coordinated_children(session_id, false);
        let rows: Vec<Value> = children
            .iter()
            .filter_map(|child| {
                let meta = child.session.meta().ok()?;
                Some(serde_json::json!({
                    "id": meta.id, "description": meta.description, "lifetime": meta.child_lifetime,
                    "workspace": meta.workspace, "hidden": meta.hidden, "archived": meta.archived,
                    "snapshot": subagent::snapshot(&child.session),
                    "report": subagent::reports(&child.session).last(),
                }))
            })
            .collect();
        serde_json::to_string(&rows).unwrap_or_default()
    }

    pub fn message_session(&self, parent_id: &str, id: &str, text: &str) -> String {
        let Some(child) = self.child_session(parent_id, id) else {
            return "no child session with this id".into();
        };
        if !child
            .session
            .meta()
            .is_ok_and(|meta| meta.child_lifetime.is_some())
        {
            return "Use spawn_subagent resume_from for this worker.".into();
        }
        if text.trim().is_empty() || text.trim_start().starts_with('/') {
            return "message_session needs a non-empty task, not a slash command".into();
        }
        match self.ask_with(id, text, Some(false)) {
            Ok(AskOutcome::Started(assignment_id)) => {
                serde_json::json!({"id": id, "state": "started", "assignment_id": assignment_id})
                    .to_string()
            }
            Ok(AskOutcome::Queued(queue_id)) => {
                serde_json::json!({"id": id, "state": "queued", "queue_id": queue_id}).to_string()
            }
            Ok(_) => "the message did not start".into(),
            Err(error) => error.to_string(),
        }
    }

    pub async fn stop_session(&self, parent_id: &str, id: &str) -> String {
        if self.child_session(parent_id, id).is_none() {
            return "no child session with this id".into();
        }
        self.retire(id).await;
        if let Some(child) = self.session_state(id) {
            for schedule in child.tools.schedules().pending().unwrap_or_default() {
                let _ = child.tools.schedules().cancel("stop", &schedule.id);
            }
            child.pending_wakes.lock().expect("pending wakes").clear();
            let queued: Vec<_> = child
                .ask_queue
                .lock()
                .expect("ask queue")
                .iter()
                .map(|ask| ask.id.clone())
                .collect();
            for queued_id in queued {
                let _ = self.remove_queued(id, &queued_id);
            }
        }
        format!("stopped {id}; session history and workspace retained")
    }

    fn coordinated_children(&self, parent_id: &str, active_only: bool) -> Vec<Arc<SessionState>> {
        let mut children: Vec<_> = self
            .sessions
            .lock()
            .expect("sessions")
            .values()
            .filter(|child| {
                child.session.meta().is_ok_and(|meta| {
                    meta.parent_id.as_deref() == Some(parent_id)
                        && meta.child_lifetime.is_some()
                        && (!active_only || !meta.archived)
                })
            })
            .cloned()
            .collect();
        children.sort_by_key(|child| child.session.meta().map(|meta| meta.id).unwrap_or_default());
        children
    }

    pub(super) fn deliver_child_reports(&self, parent_id: &str) {
        let Some(parent) = self.session_state(parent_id) else {
            return;
        };
        if self.occupied(&parent)
            || self.waiting(&parent)
            || parent.session.meta().is_ok_and(|meta| {
                meta.goal.is_some_and(|goal| {
                    matches!(
                        goal.status,
                        crate::goal::GoalStatus::Paused | crate::goal::GoalStatus::BudgetExhausted
                    )
                })
            })
            || parent.retiring.load(Ordering::Acquire)
            || parent.session.meta().is_ok_and(|meta| meta.archived)
        {
            return;
        }
        let mut delivering = self.delivering_reports.lock().expect("report delivery");
        if delivering.contains(parent_id) {
            return;
        }
        let events = parent.session.events().unwrap_or_default();
        let delivered: HashSet<String> = events
            .iter()
            .filter(|event| {
                event.kind == EventKind::UserAsk
                    && events.iter().any(|result| {
                        result.kind == EventKind::Result
                            && result.turn_id == event.turn_id
                            && result
                                .body_as::<ResultBody>()
                                .is_ok_and(|body| body.text != "The server stopped.")
                    })
            })
            .filter_map(|event| event.body_as::<AskBody>().ok())
            .filter_map(|body| {
                body.context
                    .strip_prefix("Child reports:\n")
                    .map(str::to_string)
            })
            .filter_map(|context| serde_json::from_str::<Vec<subagent::ChildReport>>(&context).ok())
            .flatten()
            .map(|report| format!("{}:{}", report.child_id, report.result_id))
            .collect();
        let reports: Vec<_> = self
            .coordinated_children(parent_id, false)
            .iter()
            .filter(|child| !self.occupied(child))
            .flat_map(|child| subagent::reports(&child.session))
            .filter(|report| {
                !delivered.contains(&format!("{}:{}", report.child_id, report.result_id))
            })
            .collect();
        if reports.is_empty() {
            return;
        }
        let context = format!(
            "Child reports:\n{}",
            serde_json::to_string(&reports).unwrap_or_default()
        );
        delivering.insert(parent_id.to_string());
        if !matches!(self.start_turn(&parent, "Child sessions reported back. Review their results and continue coordinating the user's request.", &context, None, true, Vec::new()), Ok(TurnStart::Id(_))) {
            delivering.remove(parent_id);
        }
    }

    pub async fn archive_children(&self, parent_id: &str) {
        let children: Vec<_> = self
            .sessions
            .lock()
            .expect("sessions")
            .values()
            .filter_map(|child| child.session.meta().ok())
            .filter(|meta| meta.parent_id.as_deref() == Some(parent_id))
            .collect();
        for child in children {
            if child.child_lifetime.is_some() {
                self.retire(&child.id).await;
                if let Some(state) = self.session_state(&child.id) {
                    let _ = state.session.set_archived(true, &now());
                }
            } else {
                self.kill_task(parent_id, &child.id).await;
            }
        }
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
        meta.child_lifetime = match args.get("lifetime").and_then(Value::as_str) {
            Some("temporary") => Some(subagent::ChildLifetime::Temporary),
            Some("lasting") => Some(subagent::ChildLifetime::Lasting),
            _ => None,
        };
        meta.closeout_reviewer = skill_directory.is_some();
        if let Some(directory) = skill_directory {
            meta.allow
                .outside_read_paths
                .push(directory.to_string_lossy().into_owned());
        }
        meta.description = Some(input.description.clone());
        meta.title = Some(input.description.clone());
        meta.isolation = Some(input.isolation.label().to_string());
        meta.hidden = !input.visible
            && !(meta.child_lifetime == Some(subagent::ChildLifetime::Lasting)
                && args.get("visible").is_none());
        meta.yolo = parent.yolo;
        meta.enhance = false;
        meta.show_closeout = parent.show_closeout;
        meta.profile = parent.profile.clone();
        meta.effort = selection.effort.clone();
        meta.model_override = Some(selection);
        meta.service_tier = parent.service_tier.clone();
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
        if meta.child_lifetime.is_none() {
            self.track_child(
                &id,
                &parent.id,
                input.background && skill_directory.is_none(),
            );
        }
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
        if caller != id && self.child_session(caller, id).is_none() {
            return "only this session or its direct children can be archived".into();
        }
        state.retiring.store(true, Ordering::Release);
        self.archive_children(id).await;
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
            if child
                .session
                .meta()
                .is_ok_and(|meta| meta.child_lifetime.is_some())
            {
                return "Use stop_session to stop execution or archive_session to retain this child's history and workspace.".into();
            }
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
        if meta.child_lifetime.is_some() {
            self.deliver_child_reports(&parent_id);
            return;
        }
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
