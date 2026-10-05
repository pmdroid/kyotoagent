use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::chat::ChatClient;
use crate::events::{
    now, open_enhance, EnhanceAnswerBody, EnhanceBody, EnhanceChoice, EnhanceRequestBody, Event,
    EventKind,
};
use crate::screen::Status;
use crate::session::SessionError;

use super::{append_with_body, next_turn_id, set_status, Runner, SessionState, TurnError};

const INSTRUCTION: &str = "Rewrite the request into the prompt the coding agent will follow. Keep the user's goal. Pull in constraints the project instructions already state. Leave out work the user did not ask for. Reply with the prompt only.";

const RECENT_ASKS: usize = 3;
const RECENT_CAP: usize = 500;
const EMPTY_REWRITE: &str = "the rewrite was empty";

pub(super) struct EnhanceJob {
    cancel: tokio::sync::watch::Sender<bool>,
    done: tokio::sync::watch::Sender<bool>,
    finished: AtomicBool,
}

pub(super) struct EnhanceSlot {
    job: Mutex<Option<Arc<EnhanceJob>>>,
}

impl EnhanceSlot {
    pub(super) fn new() -> Arc<EnhanceSlot> {
        Arc::new(EnhanceSlot {
            job: Mutex::new(None),
        })
    }

    pub(super) fn is_running(&self) -> bool {
        self.job
            .lock()
            .expect("the enhance slot is not poisoned")
            .as_ref()
            .is_some_and(|job| !job.finished.load(Ordering::SeqCst))
    }

    fn begin(&self) -> (Arc<EnhanceJob>, bool) {
        let mut slot = self.job.lock().expect("the enhance slot is not poisoned");
        if let Some(job) = slot.as_ref() {
            if !job.finished.load(Ordering::SeqCst) {
                return (Arc::clone(job), false);
            }
        }
        let (cancel, _) = tokio::sync::watch::channel(false);
        let (done, _) = tokio::sync::watch::channel(false);
        let job = Arc::new(EnhanceJob {
            cancel,
            done,
            finished: AtomicBool::new(false),
        });
        *slot = Some(Arc::clone(&job));
        (job, true)
    }

    fn finish(&self, job: &Arc<EnhanceJob>) {
        job.finished.store(true, Ordering::SeqCst);
        let _ = job.done.send(true);
        let mut slot = self.job.lock().expect("the enhance slot is not poisoned");
        if slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, job))
        {
            *slot = None;
        }
    }

    pub(super) fn cancel(&self) {
        if let Some(job) = self
            .job
            .lock()
            .expect("the enhance slot is not poisoned")
            .clone()
        {
            let _ = job.cancel.send(true);
        }
    }

    pub(super) async fn cancel_and_wait(&self) {
        let mut done = {
            let slot = self.job.lock().expect("the enhance slot is not poisoned");
            let Some(job) = slot.as_ref() else {
                return;
            };
            let _ = job.cancel.send(true);
            if job.finished.load(Ordering::SeqCst) {
                return;
            }
            job.done.subscribe()
        };
        if *done.borrow() {
            return;
        }
        while done.changed().await.is_ok() {
            if *done.borrow() {
                return;
            }
        }
    }
}

#[derive(Debug)]
pub enum EnhanceError {
    Missing,
    Settled,
    Choice,
    EmptyRevise,
    NoDraft,
    Busy,
    Failed(String),
}

impl std::fmt::Display for EnhanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnhanceError::Missing => write!(f, "no such session"),
            EnhanceError::Settled => write!(f, "that card is already settled"),
            EnhanceError::Choice => {
                write!(f, "an enhance card takes use, revise, discard, or retry")
            }
            EnhanceError::EmptyRevise => write!(f, "revise needs text"),
            EnhanceError::NoDraft => write!(f, "use needs a draft"),
            EnhanceError::Busy => write!(f, "the session is already working"),
            EnhanceError::Failed(message) => write!(f, "{message}"),
        }
    }
}

pub(super) fn recent_asks(events: &[Event]) -> Vec<String> {
    let asks: Vec<String> = events
        .iter()
        .filter(|event| event.kind == EventKind::UserAsk)
        .filter_map(|event| event.body_as::<crate::events::AskBody>().ok())
        .map(|body| cap_chars(&body.text, RECENT_CAP))
        .collect();
    let skip = asks.len().saturating_sub(RECENT_ASKS);
    asks.into_iter().skip(skip).collect()
}

pub(super) fn rewrite_messages(
    workspace: &str,
    agents: &str,
    recent: &[String],
    text: &str,
) -> Vec<crate::chat::Message> {
    let mut user = format!("Workspace: {workspace}\n");
    if !agents.is_empty() {
        user.push_str("\nProject instructions (AGENTS.md):\n\n");
        user.push_str(agents);
        if !agents.ends_with('\n') {
            user.push('\n');
        }
    }
    if !recent.is_empty() {
        user.push_str("\nRecent requests:\n");
        for ask in recent {
            user.push_str(ask);
            user.push('\n');
        }
    }
    user.push_str("\nRequest:\n");
    user.push_str(text);
    vec![
        crate::chat::Message::System {
            content: INSTRUCTION.to_string(),
        },
        crate::chat::Message::User {
            content: user.into(),
        },
    ]
}

fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

fn parse_choice(choice: &str) -> Option<EnhanceChoice> {
    match choice {
        "use" => Some(EnhanceChoice::Use),
        "revise" => Some(EnhanceChoice::Revise),
        "discard" => Some(EnhanceChoice::Discard),
        "retry" => Some(EnhanceChoice::Retry),
        _ => None,
    }
}

pub(super) fn rewrite_in_flight(events: &[Event]) -> bool {
    for event in events.iter().rev() {
        match event.kind {
            EventKind::EnhanceRequest => return true,
            EventKind::Enhance | EventKind::UserAsk | EventKind::Result => return false,
            _ => {}
        }
    }
    false
}

impl Runner {
    pub(super) fn enhance_running(&self, state: &SessionState) -> bool {
        state.enhance.is_running()
    }

    pub(super) fn enhance_open(&self, state: &SessionState) -> bool {
        state
            .session
            .events()
            .ok()
            .as_deref()
            .and_then(open_enhance)
            .is_some()
    }

    pub(super) fn launch_enhance(
        &self,
        state: &Arc<SessionState>,
        text: &str,
        prior_card: Option<String>,
    ) -> Result<String, TurnError> {
        let (job, started) = state.enhance.begin();
        if !started {
            return Err(TurnError::Busy);
        }
        let launched = self.launch_held(state, text, prior_card, &job);
        if launched.is_err() {
            state.enhance.finish(&job);
        }
        launched
    }

    fn launch_held(
        &self,
        state: &Arc<SessionState>,
        text: &str,
        prior_card: Option<String>,
        job: &Arc<EnhanceJob>,
    ) -> Result<String, TurnError> {
        let config = self.config_for_turn();
        let root = self
            .config_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(PathBuf::from);
        let client = ChatClient::in_root(&config, root.as_deref())?;
        let meta = state.session.meta()?;
        let model = config.title_model().unwrap_or(&meta.model).to_string();
        let workspace = state.tools.workspace().to_path_buf();
        let agents = crate::agents_doc::load(&workspace);
        let events = state.session.events()?;
        let recent = recent_asks(&events);
        let turn_id = next_turn_id(&state.session);
        let request_id = if prior_card.is_none() {
            let id = state.session.next_event_id()?;
            let event = Event::new(&id, &now(), &turn_id, EventKind::EnhanceRequest)
                .with_body(&EnhanceRequestBody {
                    text: text.to_string(),
                    model: model.clone(),
                })
                .map_err(|source| SessionError::Json {
                    path: state.session.events_path(),
                    source,
                })?;
            state.session.append(&event)?;
            set_status(&state.session, Status::Working)?;
            id
        } else {
            String::new()
        };
        let mut cancel = job.cancel.subscribe();
        let session = state.session.clone();
        let slot = Arc::clone(&state.enhance);
        let job = Arc::clone(job);
        let source = text.to_string();
        let workspace_text = workspace.to_string_lossy().to_string();
        let card_id = prior_card.clone();
        tokio::spawn(async move {
            let messages = rewrite_messages(&workspace_text, &agents, &recent, &source);
            let outcome = tokio::select! {
                biased;
                _ = cancel.changed() => None,
                result = client.rewrite(&model, &messages) => Some(result),
            };
            let aborted = outcome.is_none() || *cancel.borrow();
            if aborted || !card_still(card_id.as_deref(), &session) {
                settle_after_stop(&session);
            } else if let Some(result) = outcome {
                let (draft, error) = match result {
                    Ok(reply) => {
                        let draft = reply.text().trim().to_string();
                        if draft.is_empty() {
                            (String::new(), Some(EMPTY_REWRITE.to_string()))
                        } else {
                            (draft, None)
                        }
                    }
                    Err(error) => (String::new(), Some(error.to_string())),
                };
                if *cancel.borrow() || !card_still(card_id.as_deref(), &session) {
                    settle_after_stop(&session);
                } else {
                    let _ = append_with_body(
                        &session,
                        &turn_id,
                        EventKind::Enhance,
                        &EnhanceBody {
                            text: draft,
                            source,
                            model,
                            error,
                        },
                    );
                    let _ = set_status(&session, Status::Waiting);
                }
            }
            slot.finish(&job);
        });
        Ok(if request_id.is_empty() {
            prior_card.unwrap_or_default()
        } else {
            request_id
        })
    }

    pub async fn answer_enhance(
        &self,
        session_id: &str,
        event_id: &str,
        choice: &str,
        text: &str,
    ) -> Result<(), EnhanceError> {
        let state = self
            .sessions
            .lock()
            .expect("the session map is not poisoned")
            .get(session_id)
            .cloned()
            .ok_or(EnhanceError::Missing)?;
        let choice = parse_choice(choice).ok_or(EnhanceError::Choice)?;
        let events = state
            .session
            .events()
            .map_err(|source| EnhanceError::Failed(source.to_string()))?;
        let open = open_enhance(&events).ok_or(EnhanceError::Settled)?;
        if open.id != event_id {
            return Err(EnhanceError::Settled);
        }
        let card: EnhanceBody = open
            .body_as()
            .map_err(|source| EnhanceError::Failed(source.to_string()))?;
        let turn_id = open.turn_id.clone();
        match choice {
            EnhanceChoice::Use => {
                if card.error.is_some() || card.text.trim().is_empty() {
                    return Err(EnhanceError::NoDraft);
                }
                let draft = card.text.clone();
                self.finish_rewrite(&state, event_id).await?;
                note_answer(&state.session, &turn_id, event_id, choice, None)?;
                self.ask_with(session_id, &draft, Some(false))
                    .map_err(|source| EnhanceError::Failed(source.to_string()))?;
                Ok(())
            }
            EnhanceChoice::Revise => {
                let revised = text.trim();
                if revised.is_empty() {
                    return Err(EnhanceError::EmptyRevise);
                }
                let revised = revised.to_string();
                self.finish_rewrite(&state, event_id).await?;
                note_answer(
                    &state.session,
                    &turn_id,
                    event_id,
                    choice,
                    Some(revised.clone()),
                )?;
                self.ask_with(session_id, &revised, Some(false))
                    .map_err(|source| EnhanceError::Failed(source.to_string()))?;
                Ok(())
            }
            EnhanceChoice::Discard => {
                self.finish_rewrite(&state, event_id).await?;
                note_answer(&state.session, &turn_id, event_id, choice, None)?;
                set_status(&state.session, Status::Idle)
                    .map_err(|source| EnhanceError::Failed(source.to_string()))?;
                if let Some(runner) = self.me.upgrade() {
                    runner.after_idle(&state);
                }
                Ok(())
            }
            EnhanceChoice::Retry => {
                if state.enhance.is_running() {
                    return Err(EnhanceError::Busy);
                }
                note_answer(&state.session, &turn_id, event_id, choice, None)?;
                self.launch_enhance(&state, &card.source, Some(event_id.to_string()))
                    .map_err(|source| match source {
                        TurnError::Busy => EnhanceError::Busy,
                        other => EnhanceError::Failed(other.to_string()),
                    })?;
                Ok(())
            }
        }
    }
}

impl Runner {
    async fn finish_rewrite(
        &self,
        state: &SessionState,
        event_id: &str,
    ) -> Result<(), EnhanceError> {
        state.enhance.cancel_and_wait().await;
        let events = state
            .session
            .events()
            .map_err(|source| EnhanceError::Failed(source.to_string()))?;
        let open = open_enhance(&events).ok_or(EnhanceError::Settled)?;
        if open.id != event_id {
            return Err(EnhanceError::Settled);
        }
        Ok(())
    }
}

fn note_answer(
    session: &crate::session::Session,
    turn_id: &str,
    event_id: &str,
    choice: EnhanceChoice,
    text: Option<String>,
) -> Result<(), EnhanceError> {
    append_with_body(
        session,
        turn_id,
        EventKind::EnhanceAnswer,
        &EnhanceAnswerBody {
            enhance_id: event_id.to_string(),
            choice,
            text,
        },
    )
    .map_err(|source| EnhanceError::Failed(source.to_string()))
}

fn card_still(card_id: Option<&str>, session: &crate::session::Session) -> bool {
    let open = session
        .events()
        .ok()
        .and_then(|events| open_enhance(&events).map(|event| event.id.clone()));
    match card_id {
        None => open.is_none(),
        Some(id) => open.as_deref() == Some(id),
    }
}

fn settle_after_stop(session: &crate::session::Session) {
    let open = session
        .events()
        .ok()
        .as_deref()
        .and_then(open_enhance)
        .is_some();
    let status = if open { Status::Waiting } else { Status::Idle };
    let _ = set_status(session, status);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AskBody, Event, EventKind, ToolResultBody};

    fn ask(id: &str, text: &str) -> Event {
        Event::new(id, "2026-09-29T00:00:00.000Z", "t1", EventKind::UserAsk)
            .with_body(&AskBody {
                images: Vec::new(),
                text: text.to_string(),
                context: String::new(),
                skill: String::new(),
                silent: false,
            })
            .expect("ask")
    }

    #[test]
    fn the_rewrite_sees_the_workspace_the_agents_block_and_three_asks() {
        let long = "n".repeat(600);
        let events = vec![
            ask("e1", "one"),
            ask("e2", "two"),
            ask("e3", "three"),
            ask("e4", &long),
            Event::new(
                "e5",
                "2026-09-29T00:00:00.000Z",
                "t1",
                EventKind::ToolResult,
            )
            .with_body(&ToolResultBody {
                is_error: false,
                images: Vec::new(),
                tool: "read_file".into(),
                output: "SECRET_TOOL_OUTPUT".into(),
            })
            .expect("tool"),
        ];
        let recent = recent_asks(&events);
        assert_eq!(
            recent,
            vec!["two".to_string(), "three".to_string(), "n".repeat(500)]
        );
        let messages = rewrite_messages("/work/kyotoagent", "No comments.\n", &recent, "ship it");
        let blob = serde_json::to_string(&messages).expect("messages");
        assert!(blob.contains(INSTRUCTION), "{blob}");
        assert!(blob.contains("Workspace: /work/kyotoagent"), "{blob}");
        assert!(blob.contains("Project instructions (AGENTS.md):"), "{blob}");
        assert!(blob.contains("No comments."), "{blob}");
        assert!(blob.contains("ship it"), "{blob}");
        assert!(blob.contains("two"), "{blob}");
        assert!(
            !blob.contains("one\n") && !blob.contains("\"one\""),
            "{blob}"
        );
        assert!(!blob.contains("SECRET_TOOL_OUTPUT"), "{blob}");
        assert!(!blob.contains("\"tools\""));
        assert_eq!(recent[2].chars().count(), 500);
    }
}
