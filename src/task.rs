use std::collections::{HashMap, HashSet};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command as TokioCommand;
use tokio::sync::watch;

use crate::events::{now, Event, EventKind, TaskDoneBody, TaskStartBody, TaskStatus};
use crate::session::{Session, SessionError};
use crate::tools::{ToolError, OUTPUT_LIMIT};

pub const LAST_TASK_LINES: usize = 40;
pub const MAX_TASK_TIMEOUT_SEC: u64 = 86_400;

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize)]
pub struct Started {
    pub id: String,
    pub state: TaskStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskView {
    pub id: String,
    pub argv: Vec<String>,
    pub state: TaskStatus,
    pub exit: Option<i32>,
    pub tail: String,
}

impl TaskView {
    pub fn summary(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| format!("task {} {}", self.id, self.state.label()))
    }
}

#[derive(Clone, Debug)]
pub struct Tasks {
    session: Session,
    workspace: PathBuf,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    live: HashMap<String, LiveTask>,
}

struct LiveTask {
    argv: Vec<String>,
    pid: Option<u32>,
    cancel: watch::Sender<bool>,
    output: Arc<Mutex<Tail>>,
}

impl std::fmt::Debug for LiveTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveTask")
            .field("argv", &self.argv)
            .field("pid", &self.pid)
            .finish()
    }
}

struct Tail {
    bytes: Vec<u8>,
}

impl Tail {
    fn new() -> Tail {
        Tail { bytes: Vec::new() }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > OUTPUT_LIMIT * 2 {
            let excess = self.bytes.len() - OUTPUT_LIMIT * 2;
            self.bytes.drain(..excess);
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

impl Tasks {
    pub fn at(session: &Session) -> Result<Tasks, ToolError> {
        let meta = session.meta()?;
        let workspace =
            crate::tools::resolve(Path::new(&meta.workspace)).map_err(|source| ToolError::Io {
                path: PathBuf::from(&meta.workspace),
                source,
            })?;
        Ok(Tasks {
            session: session.clone(),
            workspace,
            inner: Arc::new(Mutex::new(Inner {
                live: HashMap::new(),
            })),
        })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.lock().live.contains_key(id)
    }

    pub fn check(&self, id: &str) -> Result<TaskView, String> {
        if let Some(live) = self.lock().live.get(id) {
            let tail = last_lines(
                &live.output.lock().expect("task output").text(),
                LAST_TASK_LINES,
            );
            return Ok(TaskView {
                id: id.to_string(),
                argv: live.argv.clone(),
                state: TaskStatus::Running,
                exit: None,
                tail,
            });
        }
        let events = self.session.events().map_err(|error| error.to_string())?;
        let mut started: Option<TaskStartBody> = None;
        let mut done: Option<TaskDoneBody> = None;
        for event in &events {
            match event.kind {
                EventKind::TaskStart => {
                    if let Ok(body) = event.body_as::<TaskStartBody>() {
                        if body.id == id {
                            started = Some(body);
                            done = None;
                        }
                    }
                }
                EventKind::TaskDone => {
                    if let Ok(body) = event.body_as::<TaskDoneBody>() {
                        if body.id == id {
                            done = Some(body);
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(done) = done {
            return Ok(TaskView {
                id: done.id,
                argv: done.argv,
                state: done.state,
                exit: Some(done.exit),
                tail: last_lines(&done.tail, LAST_TASK_LINES),
            });
        }
        if let Some(started) = started {
            return Ok(TaskView {
                id: started.id,
                argv: started.argv,
                state: TaskStatus::Running,
                exit: None,
                tail: String::new(),
            });
        }
        Err(format!("no task {id} in this session"))
    }

    pub fn open_id_on_turn(&self, turn_id: &str) -> Result<Option<String>, SessionError> {
        let events = self.session.events()?;
        let mut started = Vec::new();
        let mut done = HashSet::new();
        for event in &events {
            match event.kind {
                EventKind::TaskStart if event.turn_id == turn_id => {
                    if let Ok(body) = event.body_as::<TaskStartBody>() {
                        started.push(body.id);
                    }
                }
                EventKind::TaskDone => {
                    if let Ok(body) = event.body_as::<TaskDoneBody>() {
                        done.insert(body.id);
                    }
                }
                _ => {}
            }
        }
        Ok(started.into_iter().find(|id| !done.contains(id)))
    }

    pub async fn start(
        &self,
        turn_id: &str,
        argv: &[String],
        timeout_sec: Option<u64>,
    ) -> Result<Started, ToolError> {
        let program = argv.first().cloned().ok_or(ToolError::NoCommand)?;
        let id = self.new_id()?;
        let mut child = TokioCommand::new(&program)
            .args(&argv[1..])
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| ToolError::Spawn {
                program: program.clone(),
                source,
            })?;

        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let output = Arc::new(Mutex::new(Tail::new()));
        let mut drains = Vec::new();
        if let Some(pipe) = stdout {
            let dest = Arc::clone(&output);
            drains.push(tokio::spawn(async move {
                drain_into(pipe, dest).await;
            }));
        }
        if let Some(pipe) = stderr {
            let dest = Arc::clone(&output);
            drains.push(tokio::spawn(async move {
                drain_into(pipe, dest).await;
            }));
        }

        let (cancel_tx, cancel_rx) = watch::channel(false);
        {
            let mut inner = self.lock();
            inner.live.insert(
                id.clone(),
                LiveTask {
                    argv: argv.to_vec(),
                    pid,
                    cancel: cancel_tx,
                    output: Arc::clone(&output),
                },
            );
        }

        let start = Event::new(
            &self.session.next_event_id()?,
            &now(),
            turn_id,
            EventKind::TaskStart,
        )
        .with_body(&TaskStartBody {
            id: id.clone(),
            argv: argv.to_vec(),
        })
        .map_err(|source| SessionError::Json {
            path: self.session.events_path(),
            source,
        })?;
        if let Err(error) = self.session.append(&start) {
            signal(pid, libc::SIGKILL);
            let _ = child.wait().await;
            for drain in drains {
                drain.abort();
            }
            self.lock().live.remove(&id);
            return Err(error.into());
        }

        let tasks = self.clone();
        let argv = argv.to_vec();
        let task_id = id.clone();
        let turn_id = turn_id.to_string();
        tokio::spawn(async move {
            let mut body = wait_for_child(
                child,
                pid,
                cancel_rx,
                timeout_sec,
                Arc::clone(&output),
                task_id,
                argv,
            )
            .await;
            for mut drain in drains {
                if tokio::time::timeout(std::time::Duration::from_secs(2), &mut drain)
                    .await
                    .is_err()
                {
                    signal(pid, libc::SIGKILL);
                    if tokio::time::timeout(std::time::Duration::from_secs(2), &mut drain)
                        .await
                        .is_err()
                    {
                        drain.abort();
                    }
                }
            }
            body.tail = last_lines(&output.lock().expect("task output").text(), LAST_TASK_LINES);
            tasks.finish(&turn_id, body);
        });

        Ok(Started {
            id,
            state: TaskStatus::Running,
        })
    }

    pub fn cancel_id(&self, id: &str) -> bool {
        let sender = self.lock().live.get(id).map(|live| live.cancel.clone());
        let Some(sender) = sender else {
            return false;
        };
        let _ = sender.send(true);
        true
    }

    pub fn cancel_all(&self) {
        let senders: Vec<watch::Sender<bool>> = self
            .lock()
            .live
            .values()
            .map(|live| live.cancel.clone())
            .collect();
        for sender in senders {
            let _ = sender.send(true);
        }
    }

    pub async fn wait_idle(&self) {
        while !self.lock().live.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub fn settle_orphans(&self) -> Result<Vec<TaskDoneBody>, ToolError> {
        let events = self.session.events()?;
        let mut open: HashMap<String, TaskStartBody> = HashMap::new();
        let mut turn_of: HashMap<String, String> = HashMap::new();
        for event in &events {
            match event.kind {
                EventKind::TaskStart => {
                    if let Ok(body) = event.body_as::<TaskStartBody>() {
                        turn_of.insert(body.id.clone(), event.turn_id.clone());
                        open.insert(body.id.clone(), body);
                    }
                }
                EventKind::TaskDone => {
                    if let Ok(body) = event.body_as::<TaskDoneBody>() {
                        open.remove(&body.id);
                    }
                }
                _ => {}
            }
        }
        let mut done = Vec::new();
        for (id, start) in open {
            if self.lock().live.contains_key(&id) {
                continue;
            }
            let turn_id = turn_of
                .get(&id)
                .cloned()
                .unwrap_or_else(|| "t1".to_string());
            let body = TaskDoneBody {
                id: start.id,
                argv: start.argv,
                exit: -1,
                tail: String::new(),
                state: TaskStatus::Stopped,
            };
            self.append_done(&turn_id, &body)?;
            done.push(body);
        }
        Ok(done)
    }

    fn finish(&self, turn_id: &str, body: TaskDoneBody) {
        let _ = self.append_done(turn_id, &body);
        self.lock().live.remove(&body.id);
    }

    fn append_done(&self, turn_id: &str, body: &TaskDoneBody) -> Result<(), ToolError> {
        let event = Event::new(
            &self.session.next_event_id()?,
            &now(),
            turn_id,
            EventKind::TaskDone,
        )
        .with_body(body)
        .map_err(|source| SessionError::Json {
            path: self.session.events_path(),
            source,
        })?;
        self.session.append(&event)?;
        Ok(())
    }

    fn new_id(&self) -> Result<String, ToolError> {
        let events = self.session.events()?;
        let mut taken = Vec::new();
        for event in &events {
            if event.kind == EventKind::TaskStart {
                if let Ok(body) = event.body_as::<TaskStartBody>() {
                    taken.push(body.id);
                }
            }
        }
        loop {
            let id = generate_id();
            if taken.iter().any(|have| have == &id) {
                continue;
            }
            if self.lock().live.contains_key(&id) {
                continue;
            }
            return Ok(id);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

async fn wait_for_child(
    mut child: tokio::process::Child,
    pid: Option<u32>,
    mut cancel: watch::Receiver<bool>,
    timeout_sec: Option<u64>,
    output: Arc<Mutex<Tail>>,
    id: String,
    argv: Vec<String>,
) -> TaskDoneBody {
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let status = child.wait().await.ok();
        let _ = done_tx.send(status);
    });

    let mut state = TaskStatus::Exited;
    let status = if let Some(secs) = timeout_sec {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
        tokio::select! {
            result = &mut done_rx => result.ok().flatten(),
            _ = cancel.changed() => {
                state = TaskStatus::Stopped;
                signal(pid, libc::SIGTERM);
                match tokio::time::timeout(std::time::Duration::from_secs(2), &mut done_rx).await {
                    Ok(result) => result.ok().flatten(),
                    Err(_) => {
                        signal(pid, libc::SIGKILL);
                        done_rx.await.ok().flatten()
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                state = TaskStatus::TimedOut;
                signal(pid, libc::SIGKILL);
                done_rx.await.ok().flatten()
            }
        }
    } else {
        tokio::select! {
            result = &mut done_rx => result.ok().flatten(),
            _ = cancel.changed() => {
                state = TaskStatus::Stopped;
                signal(pid, libc::SIGTERM);
                match tokio::time::timeout(std::time::Duration::from_secs(2), &mut done_rx).await {
                    Ok(result) => result.ok().flatten(),
                    Err(_) => {
                        signal(pid, libc::SIGKILL);
                        done_rx.await.ok().flatten()
                    }
                }
            }
        }
    };

    if state == TaskStatus::Stopped {
        signal(pid, libc::SIGKILL);
    }
    let exit = status
        .and_then(|status| {
            status
                .code()
                .or_else(|| status.signal().map(|signal| 128 + signal))
        })
        .unwrap_or(-1);
    let tail = last_lines(
        &output.lock().map(|guard| guard.text()).unwrap_or_default(),
        LAST_TASK_LINES,
    );
    TaskDoneBody {
        id,
        argv,
        exit,
        tail,
        state,
    }
}

async fn drain_into<R: AsyncRead + Unpin>(mut pipe: R, dest: Arc<Mutex<Tail>>) {
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Ok(mut tail) = dest.lock() {
                    tail.push(&chunk[..n]);
                }
            }
        }
    }
}

fn signal(pid: Option<u32>, sig: i32) {
    if let Some(pid) = pid {
        unsafe { libc::kill(-(pid as i32), sig) };
    }
}

fn generate_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.subsec_nanos() as u64)
        .unwrap_or(0);
    let count = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let bits = nanos.wrapping_mul(0x9e37_79b9)
        ^ count.wrapping_mul(0x85eb_ca6b)
        ^ (std::process::id() as u64);
    format!("{:08x}", bits as u32)
}

pub fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}
