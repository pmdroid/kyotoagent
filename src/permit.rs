//! The gate a write, an outside read, and every command have to pass.
//!
//! Reading a file inside the workspace never comes here. Anything that reaches
//! the machine another way does: a write, a read or a listing that resolved
//! outside the workspace, and every command. The gate appends a `permission`
//! event, stops the turn, and waits for a `permission_answer`. The answer is
//! one of three: do it once, do it and remember it for the rest of the
//! session, or do not do it at all.
//!
//! Remembering is [`crate::session::AllowList`], on `meta.json`, and every entry
//! is exact. `allow_session` on one path allows that path and no other, and it
//! is this session's memory and nobody else's.
//!
//! The wait is a condvar rather than a poll, and the answer can be queued
//! before the question is asked. That is what lets a test drive the whole gate
//! with canned calls in one thread, and it is the same path the server's
//! `POST /answers` takes later: the answer does not care who put it there.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::events::{
    now, Decision, Event, EventKind, PermissionAnswerBody, PermissionBody, QuestionAnswerBody,
    QuestionBody,
};
use crate::screen::Status;
use crate::session::{Session, SessionError};

/// The largest diff a permission body keeps whole.
pub const FULL_DIFF_BYTES: usize = 64 * 1024;
/// The number of lines a card keeps when the change is larger than that.
pub const TRIMMED_DIFF_LINES: usize = 400;

/// A short digest of a file's bytes, as the permission records it.
///
/// The gate does not compute line diffs twice. It stores a digest of the bytes
/// that were on disk when the card went up, and compares that digest after the
/// answer comes back. A different digest means the user answered about a file
/// that is no longer the one on disk, so the answer is spent and the write
/// asks again with the diff the file has now.
///
/// This is FNV-1a, written out here rather than pulled in as a crate: it is
/// eight lines, it is deterministic across runs and processes, and it only has
/// to notice a change, not resist an attacker who is also writing files.
pub fn fingerprint(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The permission body for a write: what it proposes, and enough about the
/// file as it is now to tell a later answer from a stale one.
///
/// `old` is the bytes on disk, empty for a file that is not there yet.
pub fn write_permission(action: &str, path: &str, old: &[u8], new: &[u8]) -> PermissionBody {
    let mut body = PermissionBody {
        action: action.to_string(),
        path: Some(path.to_string()),
        diff: Some(diff(old, new)),
        old_hash: Some(fingerprint(old)),
        bytes: Some(new.len() as u64),
        contents: Some(String::from_utf8_lossy(new).into_owned()),
        ..PermissionBody::default()
    };
    trim(&mut body);
    body
}

/// Keep a card's diff short. A change up to [`FULL_DIFF_BYTES`] is kept whole,
/// because the user is deciding on it. A larger one keeps its first
/// [`TRIMMED_DIFF_LINES`] lines; the path and the byte size still say how big
/// it is, and the log keeps every byte of what was proposed.
pub fn trim(body: &mut PermissionBody) {
    let Some(diff) = body.diff.as_ref() else {
        return;
    };
    let size: usize = diff.iter().map(|line| line.len() + 1).sum();
    if size <= FULL_DIFF_BYTES {
        return;
    }
    body.diff = Some(diff.iter().take(TRIMMED_DIFF_LINES).cloned().collect());
}

/// The lines a write would change, as a card shows them.
///
/// This is a summary, not a patch: the hunk header counts the lines the change
/// touches, and only the lines between the shared beginning and the shared end
/// are drawn. A card is read at a glance, and a whole-file diff of a large
/// file is not read at a glance.
pub fn diff(old: &[u8], new: &[u8]) -> Vec<String> {
    let old_lines = lines(old);
    let new_lines = lines(new);
    let prefix = shared(&old_lines, &new_lines, true);
    // The lines the two files share at the end cannot reach back past the ones
    // they share at the front, or the same line would be drawn twice.
    let suffix = shared(&old_lines, &new_lines, false)
        .min(old_lines.len() - prefix)
        .min(new_lines.len() - prefix);
    let mut out = vec![hunk(
        prefix + 1,
        old_lines.len() - prefix - suffix,
        new_lines.len() - prefix - suffix,
    )];
    for line in &old_lines[prefix..old_lines.len() - suffix] {
        out.push(format!("-{line}"));
    }
    for line in &new_lines[prefix..new_lines.len() - suffix] {
        out.push(format!("+{line}"));
    }
    out
}

/// The file's lines, with the trailing newline not counted as an empty one.
fn lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    if text.is_empty() && bytes.is_empty() {
        return Vec::new();
    }
    text.split('\n').map(|line| line.to_string()).collect()
}

/// How many lines two files agree on, from the front when `from_front`, else
/// from the back.
fn shared(old: &[String], new: &[String], from_front: bool) -> usize {
    let mut count = 0;
    while count < old.len() && count < new.len() {
        let (a, b) = if from_front {
            (&old[count], &new[count])
        } else {
            (&old[old.len() - 1 - count], &new[new.len() - 1 - count])
        };
        if a != b {
            break;
        }
        count += 1;
    }
    count
}

/// A hunk header, with the count left off when it is nothing or one line, the
/// way a diff spells it.
fn hunk(start: usize, old_count: usize, new_count: usize) -> String {
    format!(
        "@@ -{}{} +{}{} @@",
        start,
        count(old_count),
        start,
        count(new_count)
    )
}

fn count(lines: usize) -> String {
    if lines > 1 {
        format!(",{lines}")
    } else {
        String::new()
    }
}

/// One answer, for a permission that is open or about to be.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    /// The permission it settles. `None` means whichever one is open, which is
    /// the only one a user could have been looking at.
    pub permission_id: Option<String>,
    pub decision: Decision,
}

impl Answer {
    /// Do this once, and remember nothing.
    pub fn allow_once() -> Answer {
        Answer::new(Decision::AllowOnce)
    }

    /// Do this, and anything exactly like it, for the rest of the session.
    pub fn allow_session() -> Answer {
        Answer::new(Decision::AllowSession)
    }

    /// Do not. The tool result is the denial and nothing on the machine moved.
    pub fn deny() -> Answer {
        Answer::new(Decision::Deny)
    }

    /// An answer with no permission named.
    pub fn new(decision: Decision) -> Answer {
        Answer {
            permission_id: None,
            decision,
        }
    }

    /// The same answer, naming the permission it settles.
    pub fn for_permission(mut self, id: &str) -> Answer {
        self.permission_id = Some(id.to_string());
        self
    }
}

/// What the gate came back with: which permission was answered, and how.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    /// The `permission` event this verdict settles. It is the card the answer
    /// belongs to, and a caller that wants to talk about it can.
    pub permission_id: String,
    pub decision: Decision,
}

impl Verdict {
    /// Whether the thing may go ahead. A `deny` may not, and nothing else is
    /// a failure: an allow that only had to be asked once is still an allow.
    pub fn allowed(&self) -> bool {
        self.decision != Decision::Deny
    }
}

/// An answer that does not belong to anything.
#[derive(Debug)]
pub enum AnswerError {
    /// Nothing is waiting, so there is nothing to answer. A second answer to a
    /// card that is already settled lands here.
    NothingOpen,
    /// An answer named a permission that is not the one on screen.
    NotThisOne { open: String, named: String },
    /// A second permission was asked while one was already open. A turn holds
    /// one open card at a time because it stops and waits there.
    AlreadyOpen { open: String },
    /// The log could not be written, so the answer could not settle the card.
    Session(SessionError),
}

impl std::fmt::Display for AnswerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnswerError::NothingOpen => write!(f, "no permission is open"),
            AnswerError::NotThisOne { open, named } => {
                write!(f, "permission {open} is open, not {named}")
            }
            AnswerError::AlreadyOpen { open } => write!(f, "permission {open} is still open"),
            AnswerError::Session(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for AnswerError {}

impl From<SessionError> for AnswerError {
    fn from(source: SessionError) -> AnswerError {
        AnswerError::Session(source)
    }
}

/// Something the gate could not do.
#[derive(Debug)]
pub enum GateError {
    /// The log or the meta could not be read or written.
    Session(SessionError),
    /// A card or an answer would not serialize into a line of the log.
    Body(serde_json::Error),
    /// The wait was answered with an answer that did not belong to it.
    Answer(AnswerError),
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::Session(source) => write!(f, "{source}"),
            GateError::Body(source) => write!(f, "a card would not serialize: {source}"),
            GateError::Answer(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for GateError {}

impl From<SessionError> for GateError {
    fn from(source: SessionError) -> GateError {
        GateError::Session(source)
    }
}

impl From<serde_json::Error> for GateError {
    fn from(source: serde_json::Error) -> GateError {
        GateError::Body(source)
    }
}

impl From<AnswerError> for GateError {
    fn from(source: AnswerError) -> GateError {
        GateError::Answer(source)
    }
}

/// The permission gate for one session.
///
/// Cheap to clone, and the clones share the open permission: that is what lets
/// the turn loop hold one and the server's answer route hold another. The
/// session is read and written through [`Gate::ask`], so the `permission` and
/// `permission_answer` events and the session allows all land in the same log
/// the view reads.
#[derive(Clone, Debug)]
pub struct Gate {
    session: Session,
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    asking: Mutex<()>,
    answered: Condvar,
}

#[derive(Debug, Default)]
struct State {
    cancelled: bool,
    rejected: bool,
    /// The permission the turn is stopped on, if any.
    open: Option<String>,
    /// Whether the open permission was left by a reload rather than asked by a
    /// turn that is now waiting on it. A held-open permission has no waiter,
    /// so its answer settles the card itself.
    held: bool,
    /// Answers put here before they were asked for: a test's canned calls, and
    /// an answer that landed in the moment between the card and the wait.
    canned: VecDeque<Answer>,
    /// An answer that arrived while the turn was stopped.
    live: Option<Answer>,

    /// The question the turn is stopped on, if any. A turn holds one open card
    /// at a time, so this is `None` whenever a permission is open.
    open_question: Option<String>,
    /// The answer to the open question, when it arrives.
    live_question: Option<String>,
}

impl Gate {
    /// The gate for `session`.
    pub fn at(session: &Session) -> Gate {
        Gate {
            session: session.clone(),
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                asking: Mutex::new(()),
                answered: Condvar::new(),
            }),
        }
    }

    /// The session this gate writes to.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The session's facts, which is where the remembered allows are.
    pub fn meta(&self) -> Result<crate::session::SessionMeta, SessionError> {
        self.session.meta()
    }

    /// The permission event id of the card the turn is stopped on, if any.
    pub fn open_permission(&self) -> Option<String> {
        self.lock().open.clone()
    }

    /// Whether the open permission was left by a reload, so no turn is waiting
    /// on it. The answer to such a permission settles the card itself, because
    /// the turn that asked it is gone.
    pub fn take_live_answer(&self) -> Option<Answer> {
        self.lock().live.take()
    }

    pub fn is_held_open(&self) -> bool {
        let state = self.lock();
        state.open.is_some() && state.held
    }

    /// Put an answer in the gate.
    ///
    /// An answer put in before its card is up is kept until that card is asked,
    /// which is how a test scripts a turn without a thread. An answer put in
    /// while a card is open wakes the turn. An answer for nothing, or for a
    /// card that is not the open one, is refused: the first answer wins.
    ///
    /// The card is let go as the answer lands, not when the turn wakes up: the
    /// answer settles it, so a second answer to the same card is refused, and a
    /// turn that goes on to ask something else is not still holding this one.
    pub fn answer(&self, answer: Answer) -> Result<(), AnswerError> {
        let mut state = self.lock();
        let Some(open) = state.open.clone() else {
            return Err(AnswerError::NothingOpen);
        };
        if let Some(named) = answer.permission_id.as_deref() {
            if named != open {
                return Err(AnswerError::NotThisOne {
                    open,
                    named: named.to_string(),
                });
            }
        }
        state.live = Some(answer);
        state.held = false;
        state.open = None;
        self.shared.answered.notify_all();
        Ok(())
    }

    /// Put an answer in the gate without a card to be waiting on one.
    ///
    /// This is the test and script path: the answer is kept for the next
    /// permission this gate asks. The server's answer route uses
    /// [`Gate::answer`], because a card it cannot name is a card that is not
    /// there.
    pub fn queue(&self, answer: Answer) {
        self.lock().canned.push_back(answer);
    }

    pub fn cancel(&self) {
        let mut state = self.lock();
        state.cancelled = true;
        state.open = None;
        state.open_question = None;
        state.canned.clear();
        state.live = None;
        state.live_question = None;
        self.shared.answered.notify_all();
    }

    pub fn reset_cancel(&self) {
        let mut state = self.lock();
        state.cancelled = false;
        state.rejected = false;
    }

    pub(crate) fn rejected(&self) -> bool {
        self.lock().rejected
    }

    /// The question event id the turn is stopped on, if any.
    pub fn open_question(&self) -> Option<String> {
        self.lock().open_question.clone()
    }

    /// Mark a permission as open, for a reload that found it unanswered on
    /// disk. The card is still on screen and the answer still lands, so the
    /// gate has to know the permission is open even though nobody asked it in
    /// this process.
    pub fn hold_open(&self, id: &str) {
        let mut state = self.lock();
        if state.open.is_none() {
            state.open = Some(id.to_string());
            state.held = true;
        }
    }

    pub fn hold_open_question(&self, id: &str) {
        let mut state = self.lock();
        if state.open.is_none() && state.open_question.is_none() {
            state.open_question = Some(id.to_string());
            state.held = true;
        }
    }

    pub fn is_held_open_question(&self) -> bool {
        let state = self.lock();
        state.open_question.is_some() && state.held
    }

    /// Answer the open question.
    ///
    /// The answer is the reply text the user typed or the choice they picked.
    /// It wakes the turn that is stopped on the question, and it is refused
    /// when no question is open: a permission is a different card with a
    /// different answer. Like a permission, the question is let go as the
    /// answer lands, so a second answer to it is refused too.
    pub fn answer_question(&self, answer: String) -> Result<(), AnswerError> {
        self.answer_question_for(None, answer)
    }

    pub fn answer_question_for(&self, id: Option<&str>, answer: String) -> Result<(), AnswerError> {
        let mut state = self.lock();
        if state.open_question.is_none()
            || id.is_some_and(|id| state.open_question.as_deref() != Some(id))
        {
            return Err(AnswerError::NothingOpen);
        }
        state.live_question = Some(answer);
        state.open_question = None;
        self.shared.answered.notify_all();
        Ok(())
    }

    /// Ask the user, and wait.
    ///
    /// The `permission` event goes to the log first, so a card is on screen
    /// before anyone can answer it, and the session reads `waiting` for as long
    /// as the answer takes. The `permission_answer` follows, and the session
    /// goes back to `working` before this returns. Whether the answer is `deny`
    /// is not this function's problem: the tool result is the denial.
    pub fn ask(&self, turn_id: &str, body: &PermissionBody) -> Result<Verdict, GateError> {
        let _asking = self.shared.asking.lock().expect("permission order");
        if self.rejected() {
            return Ok(Verdict {
                permission_id: String::new(),
                decision: Decision::Deny,
            });
        }
        let id = self.session.next_event_id()?;
        let asked = Event::new(&id, &now(), turn_id, EventKind::Permission).with_body(body)?;
        self.session.append(&asked)?;
        if self.session.meta()?.yolo {
            let answered = Event::new(
                &self.session.next_event_id()?,
                &now(),
                turn_id,
                EventKind::PermissionAnswer,
            )
            .with_body(&PermissionAnswerBody {
                permission_id: Some(id.clone()),
                decision: crate::events::Decision::AllowOnce,
            })?;
            self.session.append(&answered)?;
            return Ok(Verdict {
                permission_id: id,
                decision: crate::events::Decision::AllowOnce,
            });
        }
        self.open(&id)?;
        self.set_status(Status::Waiting)?;

        let answer = self.wait();
        if answer.decision == Decision::Deny {
            let mut state = self.lock();
            state.rejected = !state.cancelled;
        }
        self.close(&id);
        let answered = Event::new(
            &self.session.next_event_id()?,
            &now(),
            turn_id,
            EventKind::PermissionAnswer,
        )
        .with_body(&PermissionAnswerBody {
            permission_id: Some(id.clone()),
            decision: answer.decision,
        })?;
        self.session.append(&answered)?;
        self.set_status(Status::Working)?;

        Ok(Verdict {
            permission_id: id,
            decision: answer.decision,
        })
    }

    /// Ask the user a question, and wait for the answer.
    ///
    /// The `question` event goes to the log first, so a card is on screen
    /// before anyone can answer it, and the session reads `waiting` for as long
    /// as the answer takes. The `question_answer` follows, and the session goes
    /// back to `working` before this returns. The answer is the tool result the
    /// turn hands back to the model, and it stays on the question card.
    pub fn ask_question(
        &self,
        turn_id: &str,
        text: &str,
        choices: &[String],
    ) -> Result<String, GateError> {
        self.ask_visual_question(
            turn_id,
            QuestionBody {
                text: text.to_string(),
                choices: choices.to_vec(),
                visuals: Vec::new(),
            },
        )
    }

    pub fn ask_visual_question(
        &self,
        turn_id: &str,
        question: QuestionBody,
    ) -> Result<String, GateError> {
        let id = self.session.next_event_id()?;
        let asked = Event::new(&id, &now(), turn_id, EventKind::Question).with_body(&question)?;
        self.session.append(&asked)?;
        self.stop_on_question(&id)?;
        self.set_status(Status::Waiting)?;

        let answer = self.wait_question();
        self.close_question(&id);
        let answered = Event::new(
            &self.session.next_event_id()?,
            &now(),
            turn_id,
            EventKind::QuestionAnswer,
        )
        .with_body(&QuestionAnswerBody {
            question_id: Some(id.clone()),
            answer: answer.clone(),
        })?;
        self.session.append(&answered)?;
        self.set_status(Status::Working)?;

        Ok(answer)
    }

    /// Remember a write for the rest of the session.
    pub fn remember_write(&self, path: &str) -> Result<(), SessionError> {
        self.remember(|allow| allow.remember(Some(path), None, None))
    }

    /// Remember a read outside the workspace for the rest of the session.
    pub fn remember_outside_read(&self, path: &str) -> Result<(), SessionError> {
        self.remember(|allow| allow.remember(None, Some(path), None))
    }

    /// Remember one exact argv for the rest of the session.
    pub fn remember_argv(&self, argv: &[String]) -> Result<(), SessionError> {
        self.remember(|allow| allow.remember(None, None, Some(argv)))
    }

    pub fn remember_fetch(&self, origin: &str) -> Result<(), SessionError> {
        self.remember(|allow| allow.remember_fetch(origin))
    }

    pub fn remember_search(&self) -> Result<(), SessionError> {
        self.remember(|allow| allow.remember_search())
    }

    /// Put one exact thing on the session's own list. A turn that only reads
    /// `meta.json` and writes it back is not two turns racing, because a turn
    /// is stopped on a card while it waits, and the answer is what wakes it.
    fn remember(
        &self,
        add: impl FnOnce(&mut crate::session::AllowList),
    ) -> Result<(), SessionError> {
        self.session.update(|meta| {
            add(&mut meta.allow);
            true
        })
    }

    fn set_status(&self, status: Status) -> Result<(), SessionError> {
        self.session.update(|meta| {
            if meta.status == status {
                return false;
            }
            meta.status = status;
            meta.updated_at = now();
            true
        })
    }

    /// Stop the turn on this permission.
    fn open(&self, id: &str) -> Result<(), AnswerError> {
        let mut state = self.lock();
        if let Some(open) = state.open.as_deref() {
            return Err(AnswerError::AlreadyOpen {
                open: open.to_string(),
            });
        }
        state.open = Some(id.to_string());
        state.held = false;
        Ok(())
    }

    /// Let the turn off the card.
    fn close(&self, id: &str) {
        let mut state = self.lock();
        if state.open.as_deref() == Some(id) {
            state.open = None;
        }
    }

    /// Stop until an answer turns up. Every answer in the gate was put there by
    /// somebody who meant it, so this cannot be answered with an error: an
    /// answer for a card that is not this one is refused at the door instead.
    fn wait(&self) -> Answer {
        let mut state = self.lock();
        loop {
            if state.cancelled {
                return Answer::deny();
            }
            if let Some(answer) = state.canned.pop_front() {
                return answer;
            }
            if let Some(answer) = state.live.take() {
                return answer;
            }
            state = self
                .shared
                .answered
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Stop the turn on this question. A question and a permission are never
    /// open at once, because a turn holds one open card at a time.
    fn stop_on_question(&self, id: &str) -> Result<(), AnswerError> {
        let mut state = self.lock();
        if state.open.is_some() {
            return Err(AnswerError::AlreadyOpen {
                open: state.open.clone().unwrap_or_default(),
            });
        }
        if let Some(open) = state.open_question.as_deref() {
            return Err(AnswerError::AlreadyOpen {
                open: open.to_string(),
            });
        }
        state.open_question = Some(id.to_string());
        state.held = false;
        Ok(())
    }

    /// Let the turn off the question.
    fn close_question(&self, id: &str) {
        let mut state = self.lock();
        if state.open_question.as_deref() == Some(id) {
            state.open_question = None;
        }
    }

    /// Stop until the question is answered. The answer is free text, so there is
    /// nothing to refuse: whatever the user typed is the answer.
    fn wait_question(&self) -> String {
        let mut state = self.lock();
        loop {
            if state.cancelled {
                return "Stopped.".to_string();
            }
            if let Some(answer) = state.live_question.take() {
                return answer;
            }
            state = self
                .shared
                .answered
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// The lock is taken through here because a test that panics while holding
    /// it should not take the gate down with it: the state behind the lock is
    /// still a consistent open permission and a queue of answers.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_notices_a_change_and_survives_a_run() {
        assert_eq!(fingerprint(b""), fingerprint(b""));
        assert_ne!(fingerprint(b"one\n"), fingerprint(b"two\n"));
        // A digest is stored in the log and compared after a reload, so it has
        // to be the same number twice, not just a different one.
        assert_eq!(fingerprint(b"kyotoagent\n"), "62b3f649d9b56152");
    }

    #[test]
    fn a_diff_names_the_lines_that_change() {
        let lines = diff(b"kyotoagent\n", b"kyotoagent\nkyotoagent is the binary.\n");
        assert_eq!(lines, vec!["@@ -2 +2 @@", "+kyotoagent is the binary."]);

        let replaced = diff(b"one\ntwo\n", b"one\nthree\n");
        assert_eq!(replaced, vec!["@@ -2 +2 @@", "-two", "+three"]);
    }

    #[test]
    fn a_change_larger_than_a_card_keeps_its_first_lines() {
        let new: String = (0..500)
            .map(|n| format!("{n}: {}\n", "x".repeat(200)))
            .collect();
        let body = write_permission("Create big.txt", "/w/big.txt", b"", new.as_bytes());
        assert!(new.len() > FULL_DIFF_BYTES, "the change is over the limit");
        assert_eq!(body.bytes, Some(new.len() as u64));

        let diff = body.diff.expect("a diff");
        assert_eq!(
            diff.len(),
            TRIMMED_DIFF_LINES,
            "the hunk header and the lines under it, up to the limit"
        );
        assert!(diff[0].starts_with("@@"));
        assert!(diff[1].starts_with("+0: xxx"));
        // The log still has every byte of what was proposed, and the digest of
        // what was there.
        assert_eq!(body.contents.as_deref(), Some(new.as_str()));
        assert_eq!(body.old_hash.as_deref(), Some(fingerprint(b"").as_str()));
    }

    #[test]
    fn a_change_that_fits_a_card_is_kept_whole() {
        let body = write_permission("Create a.md", "/w/a.md", b"", b"# a\n");
        assert_eq!(
            body.diff,
            Some(vec!["@@ -1 +1 @@".to_string(), "+# a".to_string()])
        );
    }
}
