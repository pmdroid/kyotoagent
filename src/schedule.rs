use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;
use tokio::sync::Notify;

use crate::events::{
    format_millis, parse_millis, Event, EventKind, ScheduleBody, ScheduleCancelBody,
};
use crate::session::{Session, SessionError};

pub const MIN_MINUTES: i64 = 1;
pub const MAX_MINUTES: i64 = 1440;
pub const MAX_NOTE_CHARS: usize = 200;
pub const MAX_PENDING: usize = 8;

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct ScheduleWake {
    pub session_id: String,
    pub id: String,
    pub note: String,
}

struct Manual {
    now: Mutex<i64>,
    notify: Notify,
}

enum InnerClock {
    System,
    Manual(Manual),
}

#[derive(Clone)]
pub struct Clock {
    inner: Arc<InnerClock>,
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clock")
            .field("millis", &self.now_millis())
            .finish()
    }
}

impl Clock {
    pub fn system() -> Clock {
        Clock {
            inner: Arc::new(InnerClock::System),
        }
    }

    pub fn at(millis: i64) -> Clock {
        Clock {
            inner: Arc::new(InnerClock::Manual(Manual {
                now: Mutex::new(millis),
                notify: Notify::new(),
            })),
        }
    }

    pub fn now_millis(&self) -> i64 {
        match &*self.inner {
            InnerClock::System => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_millis() as i64)
                .unwrap_or(0),
            InnerClock::Manual(manual) => *manual.now.lock().expect("the clock is not poisoned"),
        }
    }

    pub fn now_stamp(&self) -> String {
        format_millis(self.now_millis())
    }

    pub fn advance(&self, duration: Duration) {
        if let InnerClock::Manual(manual) = &*self.inner {
            let mut now = manual.now.lock().expect("the clock is not poisoned");
            *now += duration.as_millis() as i64;
            drop(now);
            manual.notify.notify_waiters();
        }
    }

    pub async fn sleep_until(&self, due_millis: i64) {
        match &*self.inner {
            InnerClock::System => {
                let now = self.now_millis();
                if now >= due_millis {
                    return;
                }
                tokio::time::sleep(Duration::from_millis((due_millis - now) as u64)).await;
            }
            InnerClock::Manual(manual) => loop {
                {
                    let now = *manual.now.lock().expect("the clock is not poisoned");
                    if now >= due_millis {
                        return;
                    }
                }
                let notified = manual.notify.notified();
                tokio::pin!(notified);
                {
                    let now = *manual.now.lock().expect("the clock is not poisoned");
                    if now >= due_millis {
                        return;
                    }
                }
                notified.await;
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub id: String,
    pub note: String,
    pub due_at: String,
    pub turn_id: String,
}

#[derive(Clone, Debug)]
pub struct Schedules {
    session: Session,
    clock: Clock,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    live: HashMap<String, watch::Sender<bool>>,
    wake: Option<(String, UnboundedSender<ScheduleWake>)>,
}

impl Schedules {
    pub fn at(session: &Session, clock: Clock) -> Schedules {
        Schedules {
            session: session.clone(),
            clock,
            inner: Arc::new(Mutex::new(Inner {
                live: HashMap::new(),
                wake: None,
            })),
        }
    }

    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    pub fn listen(&self, session_id: &str, tx: UnboundedSender<ScheduleWake>) {
        self.lock().wake = Some((session_id.to_string(), tx));
    }

    pub fn abort_live(&self) {
        let senders: Vec<watch::Sender<bool>> =
            self.lock().live.drain().map(|(_, tx)| tx).collect();
        for sender in senders {
            let _ = sender.send(true);
        }
    }

    pub fn pending(&self) -> Result<Vec<Pending>, SessionError> {
        let events = self.session.events()?;
        let mut open: HashMap<String, Pending> = HashMap::new();
        for event in &events {
            match event.kind {
                EventKind::Schedule => {
                    if let Ok(body) = event.body_as::<ScheduleBody>() {
                        open.insert(
                            body.id.clone(),
                            Pending {
                                id: body.id,
                                note: body.note,
                                due_at: body.due_at,
                                turn_id: event.turn_id.clone(),
                            },
                        );
                    }
                }
                EventKind::ScheduleCancel => {
                    if let Ok(body) = event.body_as::<ScheduleCancelBody>() {
                        open.remove(&body.id);
                    }
                }
                _ => {}
            }
        }
        Ok(open.into_values().collect())
    }

    pub fn is_pending(&self, id: &str) -> bool {
        self.pending()
            .map(|pending| pending.iter().any(|item| item.id == id))
            .unwrap_or(false)
    }

    pub fn create(&self, turn_id: &str, minutes: i64, note: &str) -> Result<String, String> {
        if !(MIN_MINUTES..=MAX_MINUTES).contains(&minutes) {
            return Err(format!(
                "minutes must be from {MIN_MINUTES} to {MAX_MINUTES}"
            ));
        }
        if note.chars().count() > MAX_NOTE_CHARS {
            return Err(format!("note is at most {MAX_NOTE_CHARS} characters"));
        }
        let pending = self.pending().map_err(|error| error.to_string())?;
        if pending.len() >= MAX_PENDING {
            return Err(format!("this session already has {MAX_PENDING} schedules"));
        }
        let id = self.new_id().map_err(|error| error.to_string())?;
        let due_millis = self
            .clock
            .now_millis()
            .saturating_add(minutes.saturating_mul(60_000));
        let due_at = format_millis(due_millis);
        let event = Event::new(
            &self
                .session
                .next_event_id()
                .map_err(|error| error.to_string())?,
            &self.clock.now_stamp(),
            turn_id,
            EventKind::Schedule,
        )
        .with_body(&ScheduleBody {
            id: id.clone(),
            note: note.to_string(),
            due_at: due_at.clone(),
        })
        .map_err(|source| SessionError::Json {
            path: self.session.events_path(),
            source,
        })
        .map_err(|error| error.to_string())?;
        self.session
            .append(&event)
            .map_err(|error| error.to_string())?;
        self.start_timer(Pending {
            id: id.clone(),
            note: note.to_string(),
            due_at,
            turn_id: turn_id.to_string(),
        });
        Ok(id)
    }

    pub fn cancel(&self, turn_id: &str, id: &str) -> Result<String, String> {
        if !self.is_pending(id) {
            return Err(format!("no schedule {id} in this session"));
        }
        self.append_cancel(turn_id, id)?;
        self.abort(id);
        Ok(id.to_string())
    }

    pub fn fire(&self, id: &str) -> bool {
        let Ok(pending) = self.pending() else {
            return false;
        };
        let Some(item) = pending.into_iter().find(|item| item.id == id) else {
            return false;
        };
        if self.append_cancel(&item.turn_id, id).is_err() {
            return false;
        }
        self.abort(id);
        true
    }

    pub fn arm_pending(&self) {
        let Ok(pending) = self.pending() else {
            return;
        };
        let live: Vec<String> = self.lock().live.keys().cloned().collect();
        for item in pending {
            if live.iter().any(|id| id == &item.id) {
                continue;
            }
            self.start_timer(item);
        }
    }

    fn start_timer(&self, item: Pending) {
        let due_millis = parse_millis(&item.due_at).unwrap_or(0);
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        self.lock().live.insert(item.id.clone(), cancel_tx);
        let clock = self.clock.clone();
        let wake = self.lock().wake.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = clock.sleep_until(due_millis) => {
                    if let Some((session_id, tx)) = wake {
                        let _ = tx.send(ScheduleWake {
                            session_id,
                            id: item.id,
                            note: item.note,
                        });
                    }
                }
                _ = cancel_rx.changed() => {}
            }
        });
    }

    fn append_cancel(&self, turn_id: &str, id: &str) -> Result<(), String> {
        let event = Event::new(
            &self
                .session
                .next_event_id()
                .map_err(|error| error.to_string())?,
            &self.clock.now_stamp(),
            turn_id,
            EventKind::ScheduleCancel,
        )
        .with_body(&ScheduleCancelBody { id: id.to_string() })
        .map_err(|source| SessionError::Json {
            path: self.session.events_path(),
            source,
        })
        .map_err(|error| error.to_string())?;
        self.session
            .append(&event)
            .map_err(|error| error.to_string())
    }

    fn abort(&self, id: &str) {
        if let Some(sender) = self.lock().live.remove(id) {
            let _ = sender.send(true);
        }
    }

    fn new_id(&self) -> Result<String, SessionError> {
        let events = self.session.events()?;
        let mut taken = Vec::new();
        for event in &events {
            if event.kind == EventKind::Schedule {
                if let Ok(body) = event.body_as::<ScheduleBody>() {
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

pub fn remaining_minutes(due_at_millis: i64, now_millis: i64) -> u64 {
    let delta = due_at_millis.saturating_sub(now_millis);
    (delta / 60_000).max(1) as u64
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionMeta;
    use std::path::PathBuf;

    const T0: i64 = 1_790_640_000_000;

    fn temp_session(name: &str) -> (PathBuf, Session) {
        let dir =
            std::env::temp_dir().join(format!("kyotoagent-schedule-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let workspace = dir.join("w");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let session = Session::at(&dir.join("s"));
        session
            .create(&SessionMeta::new(
                "91bc",
                &workspace,
                "test",
                "2026-09-29T00:00:00.000Z",
            ))
            .expect("session");
        (dir, session)
    }

    #[test]
    fn remaining_minutes_stay_at_one_until_due() {
        assert_eq!(remaining_minutes(T0 + 600_000, T0), 10);
        assert_eq!(remaining_minutes(T0 + 60_000, T0), 1);
        assert_eq!(remaining_minutes(T0 + 59_999, T0), 1);
        assert_eq!(remaining_minutes(T0, T0 + 1), 1);
    }

    #[tokio::test]
    async fn minutes_outside_the_range_add_no_event() {
        let (dir, session) = temp_session("bounds");
        let schedules = Schedules::at(&session, Clock::at(T0));
        assert!(schedules.create("t1", 0, "later").is_err());
        assert!(schedules.create("t1", 1441, "later").is_err());
        assert!(session.events().expect("log").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_note_over_two_hundred_characters_is_refused() {
        let (dir, session) = temp_session("note");
        let schedules = Schedules::at(&session, Clock::at(T0));
        let long = "a".repeat(201);
        assert!(schedules.create("t1", 10, &long).is_err());
        assert!(session.events().expect("log").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_ninth_pending_schedule_is_refused() {
        let (dir, session) = temp_session("cap");
        let schedules = Schedules::at(&session, Clock::at(T0));
        for i in 0..8 {
            schedules
                .create("t1", 10, &format!("n{i}"))
                .expect("pending");
        }
        assert!(schedules.create("t1", 10, "ninth").is_err());
        assert_eq!(schedules.pending().expect("pending").len(), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cancel_drops_a_pending_schedule() {
        let (dir, session) = temp_session("cancel");
        let schedules = Schedules::at(&session, Clock::at(T0));
        let id = schedules.create("t1", 10, "later").expect("id");
        assert_eq!(schedules.pending().expect("pending").len(), 1);
        schedules.cancel("t1", &id).expect("cancel");
        assert!(schedules.pending().expect("pending").is_empty());
        assert_eq!(session.events().expect("log").len(), 2);
        assert!(schedules.cancel("t1", &id).is_err());
        assert_eq!(session.events().expect("log").len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn advancing_the_clock_wakes_without_sleeping() {
        let (dir, session) = temp_session("clock");
        let clock = Clock::at(T0);
        let schedules = Schedules::at(&session, clock.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        schedules.listen("91bc", tx);
        let id = schedules
            .create("t1", 10, "See if the deploy is up.")
            .expect("id");
        clock.advance(Duration::from_secs(10 * 60));
        let wake = rx.recv().await.expect("wake");
        assert_eq!(wake.id, id);
        assert_eq!(wake.note, "See if the deploy is up.");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
