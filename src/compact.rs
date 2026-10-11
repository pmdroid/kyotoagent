use crate::chat::{ChatClient, Message, ToolCall};
use crate::config::Config;
use serde::{Deserialize, Serialize};

use crate::events::{
    AskBody, CompactBody, Event, EventKind, ModelMessageBody, ResultBody, ToolCallBody,
    ToolResultBody,
};
use crate::session::{Session, SessionError};

pub const DEFAULT_COMPACT_PERCENT: u32 = 85;
pub const MIN_SUMMARY_CHARS: usize = 500;

pub fn at_or_above(tokens: u64, window: u64, percent: u32) -> bool {
    window > 0 && tokens.saturating_mul(100) >= window.saturating_mul(u64::from(percent))
}

pub fn estimate_tokens(messages: &[Message]) -> u64 {
    message_bytes(messages) / 4
}

pub fn estimate_request(messages: &[Message], tools_json: &str) -> u64 {
    estimate_tokens(messages).saturating_add((tools_json.len() as u64) / 4)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BucketId {
    System,
    Tools,
    Skills,
    Messages,
    Free,
}

impl BucketId {
    pub fn as_str(self) -> &'static str {
        match self {
            BucketId::System => "system",
            BucketId::Tools => "tools",
            BucketId::Skills => "skills",
            BucketId::Messages => "messages",
            BucketId::Free => "free",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBucket {
    pub id: BucketId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<u32>,
    pub buckets: Vec<ContextBucket>,
}

pub fn context_usage(
    system_without_skills: &str,
    skills_block: &str,
    tools_json: &str,
    messages: &[Message],
    prompt_tokens: Option<u64>,
    window: Option<u64>,
) -> ContextUsage {
    let estimates = [
        (system_without_skills.len() as u64) / 4,
        (tools_json.len() as u64) / 4,
        (skills_block.len() as u64) / 4,
        conversation_bytes(messages) / 4,
    ];
    let used = estimates.iter().sum::<u64>();
    let window = window.filter(|len| *len > 0);
    let percent =
        window.map(|len| u32::try_from(used.saturating_mul(100) / len).unwrap_or(u32::MAX));
    let free = window.map(|len| len.saturating_sub(used));
    let ids = [
        BucketId::System,
        BucketId::Tools,
        BucketId::Skills,
        BucketId::Messages,
    ];
    let mut buckets: Vec<ContextBucket> = ids
        .into_iter()
        .zip(estimates)
        .map(|(id, tokens)| ContextBucket {
            id,
            tokens: Some(tokens),
        })
        .collect();
    buckets.push(ContextBucket {
        id: BucketId::Free,
        tokens: free,
    });
    ContextUsage {
        used,
        reported_prompt_tokens: prompt_tokens,
        window,
        percent,
        buckets,
    }
}

fn conversation_bytes(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|message| match message {
            Message::System { .. } => 0,
            other => one_message_bytes(other),
        })
        .sum()
}

fn message_bytes(messages: &[Message]) -> u64 {
    messages.iter().map(one_message_bytes).sum()
}

fn one_message_bytes(message: &Message) -> u64 {
    match message {
        Message::System { content } | Message::Tool { content, .. } => content.len() as u64,
        Message::User { content } => content.estimated_bytes(),
        Message::Assistant {
            content,
            tool_calls,
        } => content
            .as_ref()
            .map(|c| c.len() as u64)
            .unwrap_or(0)
            .saturating_add(
                tool_calls
                    .iter()
                    .map(|call| {
                        (call.id.len() + call.kind.len() + call.name.len() + call.arguments.len())
                            as u64
                    })
                    .sum::<u64>(),
            ),
    }
}

pub fn compact_instruction() -> &'static str {
    include_str!("prompts/compact.md")
}

pub fn latest_compact(events: &[Event]) -> Option<&Event> {
    events
        .iter()
        .rev()
        .find(|event| event.kind == EventKind::Compact)
}

pub fn projected_messages(system: &str, events: &[Event], workspace: &str) -> Vec<Message> {
    projected_messages_with_goal(system, events, workspace, None)
}

pub fn projected_messages_with_goal(
    system: &str,
    events: &[Event],
    workspace: &str,
    goal: Option<&crate::goal::Goal>,
) -> Vec<Message> {
    let mut out = vec![Message::System {
        content: system.to_string(),
    }];
    let mut first_user = true;
    if let Some(event) = latest_compact(events) {
        if let Ok(body) = event.body_as::<CompactBody>() {
            if !body.summary.trim().is_empty() {
                first_user = false;
                let through = events
                    .iter()
                    .position(|event| event.id == body.through_event_id);
                let prior = through.map(|index| &events[..=index]).unwrap_or(events);
                out.push(Message::User {
                    content: crate::chat::UserContent::with_images(
                        format!(
                            "{}\nThis session continues after context compaction.\n{}",
                            user_info(workspace),
                            body.summary
                        ),
                        &recent_images(prior),
                    ),
                });
                let anchor_in_tail = goal.is_none()
                    && events_after(events, &body.through_event_id)
                        .iter()
                        .any(|event| event.kind == EventKind::UserAsk);
                if !anchor_in_tail {
                    if let Some(anchor) = continuation_anchor(prior, workspace, goal) {
                        out.push(Message::User { content: anchor });
                    }
                }
                let state = handoff_state(events);
                if !state.is_empty() {
                    out.push(Message::User {
                        content: state.into(),
                    });
                }
                project_slice(
                    events_after(events, &body.through_event_id),
                    &mut out,
                    &mut first_user,
                    workspace,
                );
                return out;
            }
        }
    }
    project_slice(events, &mut out, &mut first_user, workspace);
    out
}

pub(crate) fn handoff_state(events: &[Event]) -> String {
    let mut sections = Vec::new();
    let todos = crate::view::latest_todos(events)
        .into_iter()
        .filter(|item| item.status != crate::events::TodoStatus::Done)
        .collect::<Vec<_>>();
    if !todos.is_empty() {
        sections.push(format!(
            "Pending TODOs: {}",
            serde_json::to_string(&todos).unwrap_or_default()
        ));
    }
    let tasks = crate::view::running_tasks(events);
    if !tasks.is_empty() {
        sections.push(format!(
            "Running commands: {}",
            serde_json::to_string(&tasks).unwrap_or_default()
        ));
    }
    let mut paths = std::collections::BTreeSet::new();
    for event in events
        .iter()
        .filter(|event| event.kind == EventKind::ToolCall)
    {
        if let Ok(body) = event.body_as::<ToolCallBody>() {
            if matches!(body.tool.as_str(), "write_file" | "search_replace") {
                if let Some(path) = body.args.get("path").and_then(serde_json::Value::as_str) {
                    paths.insert(path.to_string());
                }
            }
        }
    }
    if !paths.is_empty() {
        sections.push(format!(
            "File edit targets (check the workspace for actual changes): {}",
            paths.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!(
            "<system-reminder>\n{}\n</system-reminder>",
            sections.join("\n")
        )
    }
}

struct Pending {
    text: Option<String>,
    calls: Vec<ToolCall>,
    results: Vec<ToolResultBody>,
}

fn project_slice(events: &[Event], out: &mut Vec<Message>, first_user: &mut bool, workspace: &str) {
    let mut pending: Option<Pending> = None;
    for event in events {
        match event.kind {
            EventKind::UserAsk => {
                flush_pending(out, &mut pending);
                if let Ok(body) = event.body_as::<AskBody>() {
                    let lead = *first_user;
                    *first_user = false;
                    out.push(Message::User {
                        content: crate::chat::UserContent::with_images(
                            ask_content(&body, lead, workspace),
                            &body.images,
                        ),
                    });
                }
            }
            EventKind::ModelMessage => {
                flush_pending(out, &mut pending);
                if let Ok(body) = event.body_as::<ModelMessageBody>() {
                    pending = Some(Pending {
                        text: Some(body.text),
                        calls: Vec::new(),
                        results: Vec::new(),
                    });
                }
            }
            EventKind::ToolCall => {
                if let Ok(body) = event.body_as::<ToolCallBody>() {
                    let slot = pending.get_or_insert_with(|| Pending {
                        text: None,
                        calls: Vec::new(),
                        results: Vec::new(),
                    });
                    let arguments =
                        serde_json::to_string(&body.args).unwrap_or_else(|_| "{}".to_string());
                    slot.calls.push(ToolCall {
                        id: format!("call_{}", event.id),
                        kind: "function".to_string(),
                        name: body.tool,
                        arguments,
                    });
                }
            }
            EventKind::ToolResult => {
                if let Ok(body) = event.body_as::<ToolResultBody>() {
                    let slot = pending.get_or_insert_with(|| Pending {
                        text: None,
                        calls: Vec::new(),
                        results: Vec::new(),
                    });
                    slot.results.push(body);
                }
            }
            EventKind::Result => {
                if let (Some(slot), Ok(body)) = (pending.as_mut(), event.body_as::<ResultBody>()) {
                    if slot.results.len() < slot.calls.len() {
                        let name = slot.calls[slot.results.len()].name.clone();
                        slot.results.push(ToolResultBody {
                            is_error: false,
                            tool: name,
                            output: body.text,
                            images: Vec::new(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    flush_pending(out, &mut pending);
}

fn flush_pending(out: &mut Vec<Message>, pending: &mut Option<Pending>) {
    let Some(pending) = pending.take() else {
        return;
    };
    let mut calls = pending.calls;
    let results = pending.results;
    while calls.len() < results.len() {
        let name = results[calls.len()].tool.clone();
        let index = calls.len();
        calls.push(ToolCall {
            id: format!("call_orphan_{index}"),
            kind: "function".to_string(),
            name,
            arguments: "{}".to_string(),
        });
    }
    if calls.len() > results.len() {
        calls.truncate(results.len());
    }
    let content = pending.text.filter(|text| !text.is_empty());
    if content.is_none() && calls.is_empty() {
        return;
    }
    out.push(Message::Assistant {
        content,
        tool_calls: calls.clone(),
    });
    let mut read_images = Vec::new();
    for (call, result) in calls.iter().zip(results) {
        out.push(Message::tool_result(&call.id, &cap_dump(&result.output)));
        read_images.extend(result.images);
    }
    if let Some(images) = Message::tool_images(&read_images) {
        out.push(images);
    }
}

pub fn meter_messages(system: &str, events: &[Event], workspace: &str) -> Vec<Message> {
    meter_messages_with_goal(system, events, workspace, None)
}

pub fn meter_messages_with_goal(
    system: &str,
    events: &[Event],
    workspace: &str,
    goal: Option<&crate::goal::Goal>,
) -> Vec<Message> {
    projected_messages_with_goal(system, events, workspace, goal)
}

fn events_after<'a>(events: &'a [Event], through_id: &str) -> &'a [Event] {
    match events.iter().position(|event| event.id == through_id) {
        Some(index) => &events[index + 1..],
        None => events,
    }
}

pub fn compact_request_messages(events: &[Event]) -> Vec<Message> {
    compact_request_messages_with_goal(events, "", None)
}

pub fn compact_request_messages_with_goal(
    events: &[Event],
    workspace: &str,
    goal: Option<&crate::goal::Goal>,
) -> Vec<Message> {
    let images = recent_images(events);
    let transcript = latest_compact(events)
        .and_then(|event| event.body_as::<CompactBody>().ok())
        .filter(|body| !body.summary.trim().is_empty())
        .map(|body| {
            format!(
                "Previous summary:\n{}\n\nNew events:\n{}",
                body.summary,
                transcript_dump(events_after(events, &body.through_event_id))
            )
        })
        .unwrap_or_else(|| transcript_dump_without_anchor(events));
    let mut messages = vec![
        Message::System {
            content: compact_instruction().to_string(),
        },
        Message::User {
            content: crate::chat::UserContent::with_images(transcript, &images),
        },
    ];
    if let Some(anchor) = continuation_anchor(events, workspace, goal) {
        messages.push(Message::User { content: anchor });
    }
    messages
}

fn continuation_anchor(
    events: &[Event],
    workspace: &str,
    goal: Option<&crate::goal::Goal>,
) -> Option<crate::chat::UserContent> {
    if let Some(goal) = goal.filter(|goal| {
        matches!(
            goal.status,
            crate::goal::GoalStatus::Active | crate::goal::GoalStatus::Paused
        ) && !goal.objective.trim().is_empty()
    }) {
        return Some(
            format!(
                "<user_query>\n{}\n</user_query>",
                neutralize(goal.objective.trim(), "</user_query>")
            )
            .into(),
        );
    }
    let ask = events.iter().rev().find_map(|event| {
        if event.kind != EventKind::UserAsk {
            return None;
        }
        let body = event.body_as::<AskBody>().ok()?;
        (!body.silent && !body.text.trim().is_empty()).then_some(body)
    })?;
    let mut ask = ask;
    if ask.text.len() > 64 * 1024 {
        ask.text = truncate_dump(&ask.text, 64 * 1024);
    }
    Some(crate::chat::UserContent::with_images(
        ask_content(&ask, false, workspace),
        &ask.images,
    ))
}

fn recent_images(events: &[Event]) -> Vec<crate::attachment::ImageAttachment> {
    events
        .iter()
        .rev()
        .filter(|event| event.kind == EventKind::UserAsk)
        .filter_map(|event| event.body_as::<AskBody>().ok())
        .find(|ask| !ask.images.is_empty())
        .map(|ask| ask.images)
        .unwrap_or_default()
}

pub(crate) const DUMP_LIMIT: usize = 32 * 1024;

pub(crate) fn cap_dump(text: &str) -> String {
    truncate_dump(text, DUMP_LIMIT)
}

pub(crate) fn truncate_dump(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let marker = "\n[output omitted]\n";
    let budget = limit.saturating_sub(marker.len() + "\ntruncated".len());
    let mut head = budget / 2;
    let mut tail = text.len().saturating_sub(budget - head);
    while head > 0 && !text.is_char_boundary(head) {
        head -= 1;
    }
    while tail < text.len() && !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}{marker}{}\ntruncated", &text[..head], &text[tail..])
}

pub(crate) fn fit_compact_messages(messages: &mut [Message], window: u64) {
    let anchor = messages.len().saturating_sub(1).max(1);
    let overhead = messages
        .iter()
        .enumerate()
        .map(|(index, message)| match message {
            Message::User { content } if index == anchor && anchor > 1 => {
                content.estimated_bytes() / 4
            }
            Message::User { content } => {
                content
                    .estimated_bytes()
                    .saturating_sub(content.as_str().len() as u64)
                    / 4
            }
            other => one_message_bytes(other) / 4,
        })
        .sum::<u64>();
    let budget =
        usize::try_from(window.saturating_sub(overhead).saturating_mul(2)).unwrap_or(usize::MAX);
    if let Some(Message::User { content }) = messages.get_mut(1) {
        match content {
            crate::chat::UserContent::Text(text) => *text = truncate_dump(text, budget),
            crate::chat::UserContent::Parts(parts) => {
                for part in parts {
                    if let crate::chat::UserPart::Text { text } = part {
                        *text = truncate_dump(text, budget);
                    }
                }
            }
        }
    }
}

fn neutralize(text: &str, close: &str) -> String {
    let Some(head) = close.strip_suffix('>') else {
        return text.to_string();
    };
    let broken = format!("{head}\u{200b}>");
    text.replace(close, &broken)
}

fn user_info(workspace: &str) -> String {
    let shell = std::env::var("SHELL").unwrap_or_default();
    format!(
        "<user_info>\nOS Version: {}\nShell: {}\nWorkspace Path: {}\nToday's date: {}\n</user_info>",
        std::env::consts::OS,
        shell,
        neutralize(workspace, "</user_info>"),
        local_date(),
    )
}

fn local_date() -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    let ptr = unsafe { libc::localtime_r(&stamp, tm.as_mut_ptr()) };
    if ptr.is_null() {
        return String::from("1970-01-01");
    }
    let tm = unsafe { tm.assume_init() };
    format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    )
}

fn ask_content(body: &AskBody, lead: bool, workspace: &str) -> String {
    let mut out = String::new();
    if lead {
        out.push_str(&user_info(workspace));
        out.push('\n');
    }
    out.push_str("<user_query>\n");
    out.push_str(&neutralize(&body.text, "</user_query>"));
    out.push_str("\n</user_query>");
    if !body.context.is_empty() {
        out.push_str("\n<context>\n");
        out.push_str(&neutralize(&body.context, "</context>"));
        out.push_str("\n</context>");
    }
    if !body.skill.is_empty() {
        out.push('\n');
        out.push_str(&seal_skill(&body.skill));
    }
    out
}

fn seal_skill(skill: &str) -> String {
    let trimmed = skill.trim_end();
    if let Some(head) = trimmed.strip_suffix("</skill>") {
        let mut out = neutralize(head, "</skill>");
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("</skill>");
        return out;
    }
    let mut out = String::from("<skill>\n");
    out.push_str(&neutralize(trimmed, "</skill>"));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("</skill>");
    out
}

fn transcript_dump_without_anchor(events: &[Event]) -> String {
    let anchor = events.iter().rposition(|event| {
        event.kind == EventKind::UserAsk
            && event
                .body_as::<AskBody>()
                .is_ok_and(|body| !body.silent && !body.text.trim().is_empty())
    });
    match anchor {
        Some(index) => transcript_dump(&events[..index]),
        None => transcript_dump(events),
    }
}

fn transcript_dump(events: &[Event]) -> String {
    let mut lines = Vec::new();
    for event in events {
        match event.kind {
            EventKind::UserAsk => {
                if let Ok(body) = event.body_as::<AskBody>() {
                    if !body.silent {
                        lines.push(format!("user: {}", ask_content(&body, false, "")));
                    }
                }
            }
            EventKind::ModelMessage => {
                if let Ok(body) = event.body_as::<ModelMessageBody>() {
                    lines.push(format!("assistant: {}", body.text));
                }
            }
            EventKind::Result => {
                if let Ok(body) = event.body_as::<ResultBody>() {
                    lines.push(format!("completed turn: {}", body.text));
                }
            }
            EventKind::Question | EventKind::QuestionAnswer => {
                lines.push(format!("{}: {}", event.kind.label(), event.body));
            }
            EventKind::ToolCall => {
                if let Ok(body) = event.body_as::<ToolCallBody>() {
                    lines.push(format!("tool {}: {}", body.tool, body.args));
                }
            }
            EventKind::ToolResult => {
                if let Ok(body) = event.body_as::<ToolResultBody>() {
                    lines.push(format!("result {}: {}", body.tool, cap_dump(&body.output)));
                }
            }
            _ => {}
        }
    }
    lines.join("\n")
}

pub async fn resolve_window(
    client: &ChatClient,
    config: &Config,
    session: &Session,
) -> Result<Option<u64>, SessionError> {
    let model = client.model();
    let meta = session.meta()?;
    if meta.model == model && meta.context_length.is_some() {
        return Ok(meta.context_length);
    }
    let length = client
        .model_length(model)
        .await
        .or_else(|| config.provider_context_window());
    session.update(|meta| {
        if meta.model == model && (meta.context_length.is_some() || length.is_none()) {
            return false;
        }
        meta.model = model.to_string();
        meta.context_length = length;
        true
    })?;
    Ok(length)
}

pub fn store_prompt_tokens(session: &Session, tokens: u64) -> Result<(), SessionError> {
    session.update(|meta| {
        meta.prompt_tokens = Some(tokens);
        true
    })
}

pub fn last_event_id(events: &[Event]) -> Option<String> {
    events.last().map(|event| event.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Event;

    #[test]
    fn live_tool_results_keep_bounded_utf8_head_and_tail() {
        let text = format!("start:{}:exit=1", "日".repeat(40000));
        let Message::Tool { content, .. } = Message::tool_result("call", &text) else {
            panic!("tool message")
        };
        assert!(content.len() <= DUMP_LIMIT);
        assert!(content.starts_with("start:"));
        assert!(content.contains(":exit=1"));
        assert!(content.contains("output omitted"));
    }

    fn event(id: &str, kind: EventKind, body: serde_json::Value) -> Event {
        Event::new(id, "2026-09-29T00:00:00.000Z", "t1", kind)
            .with_body(&body)
            .expect("a body serializes")
    }

    #[test]
    fn ninety_percent_of_a_thousand_is_at_the_compact_percent() {
        assert!(at_or_above(900, 1000, 85));
        assert!(!at_or_above(900, 200000, 85));
        assert!(at_or_above(750, 1000, 70));
        assert!(!at_or_above(699, 1000, 70));
        assert!(!at_or_above(900, 0, 85));
    }

    #[test]
    fn recompaction_keeps_the_summary_when_its_boundary_is_missing() {
        let events = vec![
            event(
                "e1",
                EventKind::UserAsk,
                serde_json::json!({"text": "still readable evidence"}),
            ),
            event(
                "e2",
                EventKind::Compact,
                serde_json::json!({"summary": "critical user correction", "throughEventId": "missing"}),
            ),
        ];
        let text = serde_json::to_string(&compact_request_messages(&events)).unwrap();
        assert!(text.contains("critical user correction"));
        assert!(text.contains("still readable evidence"));
    }

    #[test]
    fn recompaction_uses_the_summary_and_only_new_events() {
        let old_text = "already summarized history ".repeat(10000);
        let events = vec![
            event(
                "e1",
                EventKind::UserAsk,
                serde_json::json!({"text":old_text}),
            ),
            event(
                "e2",
                EventKind::Compact,
                serde_json::json!({"summary":"preserve this prior decision", "throughEventId":"e1"}),
            ),
            event(
                "e3",
                EventKind::UserAsk,
                serde_json::json!({"text":"new request"}),
            ),
            event(
                "e4",
                EventKind::ToolResult,
                serde_json::json!({"tool":"read_file", "output":"new evidence"}),
            ),
        ];
        let messages = compact_request_messages(&events);
        let Message::User { content } = &messages[1] else {
            panic!("a user message");
        };
        assert!(content.as_str().contains("preserve this prior decision"));
        assert!(content.as_str().contains("new request"));
        assert!(content.as_str().contains("new evidence"));
        assert!(!content.as_str().contains("already summarized history"));
        assert!(estimate_tokens(&messages) < 500);
    }

    #[test]
    fn a_compact_event_replaces_older_asks_in_the_projection() {
        let events = vec![
            event(
                "e1",
                EventKind::UserAsk,
                serde_json::to_value(AskBody {
                    images: Vec::new(),
                    text: "first ask".into(),
                    context: String::new(),
                    skill: String::new(),
                    silent: false,
                })
                .unwrap(),
            ),
            event(
                "e2",
                EventKind::Compact,
                serde_json::to_value(CompactBody {
                    summary: "the summary".into(),
                    through_event_id: "e1".into(),
                })
                .unwrap(),
            ),
            event(
                "e3",
                EventKind::UserAsk,
                serde_json::to_value(AskBody {
                    images: Vec::new(),
                    text: "later ask".into(),
                    context: String::new(),
                    skill: String::new(),
                    silent: false,
                })
                .unwrap(),
            ),
        ];
        let messages = projected_messages("sys", &events, "/w");
        assert!(matches!(messages.first(), Some(Message::System { content }) if content == "sys"));
        let users: Vec<&str> = messages
            .iter()
            .filter_map(|message| match message {
                Message::User { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(users.len(), 2, "{users:?}");
        assert!(users[0].starts_with("<user_info>\n"), "{}", users[0]);
        assert!(users[0].contains("the summary"), "{}", users[0]);
        assert!(users[0].contains("Workspace Path: /w\n"), "{}", users[0]);
        assert!(
            !users[1].contains("<user_info>"),
            "the tail ask is not a new session: {}",
            users[1]
        );
        assert!(
            users[1].contains("<user_query>\nlater ask\n</user_query>"),
            "{}",
            users[1]
        );
        assert!(users.iter().all(|text| !text.contains("first ask")));
    }

    #[test]
    fn a_failed_compact_leaves_the_full_ask() {
        let events = vec![event(
            "e1",
            EventKind::UserAsk,
            serde_json::to_value(AskBody {
                images: Vec::new(),
                text: "keep this".into(),
                context: String::new(),
                skill: String::new(),
                silent: false,
            })
            .unwrap(),
        )];
        let messages = projected_messages("sys", &events, "/w");
        assert_eq!(messages.len(), 2, "{messages:?}");
        let Message::User { content } = &messages[1] else {
            panic!("the ask is a user message: {messages:?}");
        };
        assert!(content.starts_with("<user_info>\n"), "{content}");
        assert!(content.contains("Workspace Path: /w\n"), "{content}");
        assert!(content.contains("OS Version: "), "{content}");
        assert!(content.contains("Shell: "), "{content}");
        let date = content
            .lines()
            .find(|line| line.starts_with("Today's date: "))
            .expect("a date line");
        let date = date.trim_start_matches("Today's date: ");
        assert_eq!(date.len(), 10, "{date}");
        assert_eq!(date.as_bytes()[4], b'-');
        assert_eq!(date.as_bytes()[7], b'-');
        assert!(date.bytes().filter(|byte| *byte == b'-').count() == 2);
        assert!(
            content.contains("<user_query>\nkeep this\n</user_query>"),
            "{content}"
        );
    }

    #[test]
    fn estimate_divides_utf8_bytes_by_four() {
        let messages = vec![Message::User {
            content: "abcdabcd".into(),
        }];
        assert_eq!(estimate_tokens(&messages), 2);
    }

    #[test]
    fn a_missing_usage_estimate_includes_the_tools_json() {
        let messages = vec![Message::User {
            content: "abcdabcd".into(),
        }];
        let tools = "xxxxxxxx";
        assert_eq!(estimate_request(&messages, tools), 4);
    }

    fn bucket(usage: &ContextUsage, id: BucketId) -> u64 {
        usage
            .buckets
            .iter()
            .find(|bucket| bucket.id == id)
            .and_then(|bucket| bucket.tokens)
            .unwrap_or(0)
    }

    #[test]
    fn the_five_buckets_estimate_the_current_request_independently_of_reported_usage() {
        let system = "abcd";
        let skills = "efgh";
        let tools = "ijkl";
        let body = "SECRET SKILL BODY!!!!";
        let messages = vec![
            Message::System {
                content: format!("{system}{skills}"),
            },
            Message::User {
                content: "abcd".into(),
            },
            Message::tool_result("use_skill", body),
        ];
        let usage = context_usage(system, skills, tools, &messages, None, Some(1_000));
        assert_eq!(
            usage
                .buckets
                .iter()
                .map(|bucket| bucket.id.as_str())
                .collect::<Vec<_>>(),
            vec!["system", "tools", "skills", "messages", "free"]
        );
        assert_eq!(bucket(&usage, BucketId::System), 1);
        assert_eq!(bucket(&usage, BucketId::Tools), 1);
        assert_eq!(bucket(&usage, BucketId::Skills), 1);
        assert_eq!(bucket(&usage, BucketId::Messages), 6);
        assert!(bucket(&usage, BucketId::Tools) > 0);
        assert_eq!(usage.used, 9);
        assert_eq!(usage.used + bucket(&usage, BucketId::Free), 1_000);
        assert_eq!(usage.percent, Some(0));
        let scaled = context_usage(system, skills, tools, &messages, Some(90), Some(1_000));
        let first_four: u64 = scaled
            .buckets
            .iter()
            .filter(|bucket| bucket.id != BucketId::Free)
            .map(|bucket| bucket.tokens.unwrap_or(0))
            .sum();
        assert_eq!(scaled.used, 9);
        assert_eq!(scaled.reported_prompt_tokens, Some(90));
        assert_eq!(first_four, 9);
        assert_eq!(scaled.buckets, usage.buckets);
        assert_eq!(scaled.used + bucket(&scaled, BucketId::Free), 1_000);
        assert_eq!(scaled.percent, Some(0));
        assert!(bucket(&scaled, BucketId::Messages) > bucket(&scaled, BucketId::System));
    }

    #[test]
    fn fixed_context_buckets_stay_constant_as_conversation_and_reported_usage_grow() {
        let initial = vec![Message::User {
            content: "a".repeat(120).into(),
        }];
        let mut longer = initial.clone();
        longer.push(Message::User {
            content: "b".repeat(400).into(),
        });
        let before = context_usage(
            &"s".repeat(400),
            &"k".repeat(200),
            &"t".repeat(800),
            &initial,
            Some(380),
            Some(10_000),
        );
        let after = context_usage(
            &"s".repeat(400),
            &"k".repeat(200),
            &"t".repeat(800),
            &longer,
            Some(760),
            Some(10_000),
        );
        for id in [BucketId::System, BucketId::Tools, BucketId::Skills] {
            assert_eq!(bucket(&before, id), bucket(&after, id));
        }
        assert_eq!(bucket(&before, BucketId::Messages), 30);
        assert_eq!(bucket(&after, BucketId::Messages), 130);
        assert_eq!(before.used, 380);
        assert_eq!(after.used, 480);
        assert_eq!(after.reported_prompt_tokens, Some(760));
        let changed = context_usage(
            &"s".repeat(800),
            &"k".repeat(400),
            &"t".repeat(1600),
            &initial,
            None,
            None,
        );
        assert_eq!(bucket(&changed, BucketId::System), 200);
        assert_eq!(bucket(&changed, BucketId::Tools), 400);
        assert_eq!(bucket(&changed, BucketId::Skills), 100);
    }

    #[test]
    fn tool_call_arguments_count_even_without_assistant_text() {
        let messages = vec![Message::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call".into(),
                kind: "function".into(),
                name: "read".into(),
                arguments: "x".repeat(400),
            }],
        }];
        assert_eq!(estimate_tokens(&messages), 104);
        let usage = context_usage("", "", "", &messages, None, Some(1_000));
        assert_eq!(bucket(&usage, BucketId::Messages), 104);
        assert_eq!(usage.used, 104);
    }

    #[test]
    fn a_missing_window_omits_the_percent_and_leaves_free_empty() {
        let usage = context_usage("abcd", "", "xxxx", &[], None, None);
        assert!(usage.percent.is_none());
        assert!(usage.window.is_none());
        let free = usage
            .buckets
            .iter()
            .find(|bucket| bucket.id == BucketId::Free)
            .expect("free");
        assert!(free.tokens.is_none());
        let json = serde_json::to_value(&usage).expect("json");
        assert!(json.get("percent").is_none());
        assert!(json.get("window").is_none());
        assert!(json["buckets"][4].get("tokens").is_none());
    }

    #[test]
    fn a_use_skill_body_counts_in_messages_and_the_catalog_counts_in_skills() {
        let events = vec![
            event(
                "e1",
                EventKind::UserAsk,
                serde_json::to_value(AskBody {
                    images: Vec::new(),
                    text: "load it".into(),
                    context: String::new(),
                    skill: String::new(),
                    silent: false,
                })
                .unwrap(),
            ),
            event(
                "e2",
                EventKind::ToolResult,
                serde_json::to_value(crate::events::ToolResultBody {
                    is_error: false,
                    images: Vec::new(),
                    tool: "use_skill".into(),
                    output: "abcd".into(),
                })
                .unwrap(),
            ),
        ];
        let catalog = "wxyz";
        let messages = meter_messages("system-text", &events, "/w");
        let usage = context_usage(
            "system-text",
            catalog,
            "tooljson!",
            &messages,
            None,
            Some(400),
        );
        assert!(messages.iter().any(|message| matches!(
            message,
            Message::Tool { content, .. } if content == "abcd"
        )));
        assert_eq!(bucket(&usage, BucketId::Skills), 1);
        assert_eq!(
            bucket(&usage, BucketId::Messages),
            conversation_bytes(&messages) / 4
        );
        assert!(bucket(&usage, BucketId::Messages) > 1);
        assert_eq!(bucket(&usage, BucketId::System), 2);
        let tools = serde_json::to_string(&crate::turn::tool_definitions(
            &crate::config::Config::default(),
            false,
        ))
        .expect("tools json");
        let live = context_usage("sys", "", &tools, &messages, None, Some(8_000));
        assert!(bucket(&live, BucketId::Tools) > 0);
    }

    fn ask(id: &str, text: &str) -> Event {
        event(
            id,
            EventKind::UserAsk,
            serde_json::to_value(AskBody {
                images: Vec::new(),
                text: text.into(),
                context: String::new(),
                skill: String::new(),
                silent: false,
            })
            .unwrap(),
        )
    }

    #[test]
    fn two_asks_keep_the_tool_result_between_them() {
        let events = vec![
            ask("e1", "first ask"),
            event(
                "e2",
                EventKind::ModelMessage,
                serde_json::json!({ "text": "Looking." }),
            ),
            event(
                "e3",
                EventKind::ToolCall,
                serde_json::json!({ "tool": "list_dir", "args": { "path": "." } }),
            ),
            event(
                "e4",
                EventKind::ToolResult,
                serde_json::json!({ "tool": "list_dir", "output": "MARKER.txt" }),
            ),
            ask("e5", "second ask"),
        ];
        let messages = projected_messages("sys", &events, "/w");
        let users: Vec<&str> = messages
            .iter()
            .filter_map(|message| match message {
                Message::User { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(users.len(), 2, "{users:?}");
        assert!(users[0].contains("<user_query>\nfirst ask\n</user_query>"));
        assert!(users[0].starts_with("<user_info>\n"));
        assert!(users[1].contains("<user_query>\nsecond ask\n</user_query>"));
        assert!(!users[1].contains("<user_info>"));
        assert!(messages.iter().any(|message| matches!(
            message,
            Message::Tool { content, .. } if content.contains("MARKER.txt")
        )));
        let assistant = messages.iter().find(|message| {
            matches!(message, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty())
        });
        let Some(Message::Assistant { tool_calls, .. }) = assistant else {
            panic!("the tool call is on the assistant message: {messages:?}");
        };
        assert_eq!(tool_calls[0].name, "list_dir");
        assert_eq!(tool_calls[0].id, "call_e3");
    }

    #[test]
    fn a_finish_without_a_tool_result_stays_ahead_of_the_next_ask() {
        let events = vec![
            ask("e1", "do the child work"),
            event(
                "e2",
                EventKind::ToolCall,
                serde_json::json!({
                    "tool": "finish",
                    "args": { "text": "alpha result", "proof": "alpha proof" }
                }),
            ),
            event(
                "e3",
                EventKind::Result,
                serde_json::to_value(ResultBody {
                    text: "alpha result".into(),
                    note: String::new(),
                })
                .unwrap(),
            ),
            ask("e4", "do the next part"),
        ];
        let messages = projected_messages("sys", &events, "/w");
        let dumped = format!("{messages:?}");
        let result = dumped.find("alpha result").expect("the finish");
        let prompt = dumped.find("do the next part").expect("the next ask");
        assert!(result < prompt, "{dumped}");
    }

    #[test]
    fn a_compact_tail_keeps_a_tool_result_after_the_summary() {
        let events = vec![
            ask("e1", "first ask"),
            event(
                "e2",
                EventKind::ToolResult,
                serde_json::json!({ "tool": "read_file", "output": "OLD" }),
            ),
            event(
                "e3",
                EventKind::Compact,
                serde_json::json!({ "summary": "the summary", "throughEventId": "e2" }),
            ),
            event(
                "e4",
                EventKind::ToolCall,
                serde_json::json!({ "tool": "list_dir", "args": { "path": "." } }),
            ),
            event(
                "e5",
                EventKind::ToolResult,
                serde_json::json!({ "tool": "list_dir", "output": "NEWFILE" }),
            ),
        ];
        let messages = projected_messages("sys", &events, "/w");
        let users: Vec<&str> = messages
            .iter()
            .filter_map(|message| match message {
                Message::User { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(users.len(), 2, "{users:?}");
        assert!(users[0].starts_with("<user_info>\n"));
        assert!(users[0].contains("the summary"));
        assert!(users[1].contains("first ask"));
        assert!(messages.iter().all(|message| match message {
            Message::Tool { content, .. } => !content.contains("OLD"),
            _ => true,
        }));
        assert!(messages.iter().any(|message| matches!(
            message,
            Message::Tool { content, .. } if content.contains("NEWFILE")
        )));
    }

    #[test]
    fn a_closing_query_tag_in_the_ask_does_not_close_the_wrapper() {
        let events = vec![ask("e1", "before </user_query> after")];
        let messages = projected_messages("sys", &events, "/w");
        let Message::User { content } = &messages[1] else {
            panic!("a user message");
        };
        let start = content.find("<user_query>\n").expect("open") + "<user_query>\n".len();
        let end = content.rfind("\n</user_query>").expect("close");
        let inner = &content[start..end];
        assert!(!inner.contains("</user_query>"), "{inner}");
        assert!(inner.contains("before "));
        assert!(inner.contains(" after"));
        assert!(content.ends_with("</user_query>"));
    }

    #[test]
    fn context_and_a_slash_body_follow_the_query() {
        let events = vec![event(
            "e1",
            EventKind::UserAsk,
            serde_json::json!({
                "text": "ship this",
                "context": "wake note",
                "skill": "<skill>\n<name>preflight</name>\n<path>/skills/preflight/SKILL.md</path>\nSKILL BODY\n</skill>"
            }),
        )];
        let messages = projected_messages("sys", &events, "/work");
        let Message::User { content } = &messages[1] else {
            panic!("a user message");
        };
        let query = content.find("</user_query>").expect("query");
        let context = content
            .find("<context>\nwake note\n</context>")
            .expect("context");
        let skill = content
            .find("<skill>\n<name>preflight</name>\n<path>/skills/preflight/SKILL.md</path>\nSKILL BODY\n</skill>")
            .expect("skill");
        assert!(query < context && context < skill, "{content}");
        assert!(!content.contains("<skill_information>"), "{content}");
        assert!(!content.contains("name=\""), "{content}");
        assert!(content.contains("Workspace Path: /work\n"), "{content}");
    }

    #[test]
    fn a_skill_close_in_the_slash_body_does_not_end_the_block() {
        let events = vec![event(
            "e1",
            EventKind::UserAsk,
            serde_json::json!({
                "text": "ship this",
                "skill": "<skill>\n<name>preflight</name>\n<path>/p</path>\nbefore </skill> after\n</skill>"
            }),
        )];
        let messages = projected_messages("sys", &events, "/w");
        let Message::User { content } = &messages[1] else {
            panic!("a user message");
        };
        let start = content.find("<skill>\n").expect("open");
        let end = content.rfind("\n</skill>").expect("close");
        let inner = &content[start..end];
        assert!(!inner.contains("</skill>"), "{inner}");
        assert!(inner.contains("before "));
        assert!(inner.contains(" after"));
        assert!(content.contains("</user_query>"));
        assert!(!content.contains("<skill_information>"));
    }

    #[test]
    fn a_huge_tool_result_is_capped_on_the_way_into_the_transcript() {
        let huge = "x".repeat(DUMP_LIMIT + 32);
        let events = vec![
            ask("e1", "read it"),
            event(
                "e2",
                EventKind::ToolCall,
                serde_json::json!({ "tool": "read_file", "args": { "path": "big" } }),
            ),
            event(
                "e3",
                EventKind::ToolResult,
                serde_json::json!({ "tool": "read_file", "output": huge }),
            ),
        ];
        let messages = projected_messages("sys", &events, "/w");
        let Message::Tool { content, .. } = messages
            .iter()
            .find(|message| matches!(message, Message::Tool { .. }))
            .expect("a tool message")
        else {
            panic!("a tool message");
        };
        assert!(content.len() < huge.len(), "{}", content.len());
        assert!(content.ends_with("\ntruncated"), "{content}");
        let dump = transcript_dump(&events);
        assert!(dump.contains("truncated"));
        assert!(!dump.contains(&huge));
    }

    #[test]
    fn an_enhance_draft_stays_out_of_the_transcript() {
        let events = vec![
            event(
                "e1",
                EventKind::UserAsk,
                serde_json::json!({ "text": "older request" }),
            ),
            event(
                "e2",
                EventKind::EnhanceRequest,
                serde_json::json!({ "text": "ship it", "model": "m" }),
            ),
            event(
                "e3",
                EventKind::Enhance,
                serde_json::json!({
                    "text": "DRAFT TEXT",
                    "source": "ship it",
                    "model": "m"
                }),
            ),
            event(
                "e4",
                EventKind::EnhanceAnswer,
                serde_json::json!({ "enhanceId": "e3", "choice": "retry" }),
            ),
        ];
        let blob = serde_json::to_string(&projected_messages("sys", &events, "/w")).expect("json");
        assert!(blob.contains("older request"), "{blob}");
        assert!(!blob.contains("ship it"), "{blob}");
        assert!(!blob.contains("DRAFT TEXT"), "{blob}");
        let mut followed = events;
        followed.push(event(
            "e6",
            EventKind::UserAsk,
            serde_json::json!({ "text": "the accepted prompt" }),
        ));
        let messages = projected_messages("sys", &followed, "/w");
        let users: Vec<&str> = messages
            .iter()
            .filter_map(|message| match message {
                Message::User { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(users.len(), 2);
        assert!(users[0].contains("older request"));
        assert!(users[1].contains("the accepted prompt"));
        assert!(!users[1].contains("DRAFT TEXT"));
        assert!(!users[1].contains("ship it"));
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;

    fn event(id: &str, kind: EventKind, body: serde_json::Value) -> Event {
        Event::new(id, "2026-09-29T00:00:00.000Z", "t1", kind)
            .with_body(&body)
            .expect("a body serializes")
    }

    #[test]
    fn a_ten_thousand_token_window_still_sends_a_fitted_compact() {
        let events = vec![
            event(
                "e1",
                EventKind::UserAsk,
                serde_json::json!({"text": "h".repeat(80_000)}),
            ),
            event(
                "e2",
                EventKind::ModelMessage,
                serde_json::json!({"text": "done"}),
            ),
        ];
        let mut messages = compact_request_messages(&events);
        fit_compact_messages(&mut messages, 8_500);
        let estimated = estimate_tokens(&messages[..2]);
        assert!(estimated <= 8_500, "{estimated}");
        assert!(estimated > 0);
    }

    #[test]
    fn fitting_keeps_the_last_real_ask_outside_the_cut() {
        let middle = "implement the issues, verify in isolation, and open the PRs";
        let filler = "tool output ".repeat(200_000);
        let events = vec![
            event(
                "e1",
                EventKind::UserAsk,
                serde_json::json!({"text": "audit the sessions and do not change code"}),
            ),
            event(
                "e2",
                EventKind::ToolResult,
                serde_json::json!({"tool": "read_file", "output": filler}),
            ),
            event(
                "e3",
                EventKind::UserAsk,
                serde_json::json!({"text": middle, "silent": false}),
            ),
            event(
                "e4",
                EventKind::UserAsk,
                serde_json::json!({"text": "background title rewrite", "silent": true}),
            ),
            event(
                "e5",
                EventKind::ModelMessage,
                serde_json::json!({"text": "progress continues"}),
            ),
        ];
        for window in [272_000_u64, 500_000, 872_000] {
            let mut messages = compact_request_messages(&events);
            fit_compact_messages(&mut messages, window);
            let rendered = serde_json::to_string(&messages).unwrap();
            assert!(
                rendered.contains(middle),
                "window {window} dropped the last real ask: {rendered}"
            );
            assert_eq!(
                rendered.matches(middle).count(),
                1,
                "window {window} repeated the anchor"
            );
            assert!(
                !rendered.contains("background title rewrite"),
                "a silent ask is not the anchor"
            );
        }
    }

    #[test]
    fn a_live_goal_replaces_the_last_ask_after_compaction() {
        let events = vec![
            event(
                "e1",
                EventKind::UserAsk,
                serde_json::json!({"text": "audit only and do not implement"}),
            ),
            event(
                "e2",
                EventKind::ModelMessage,
                serde_json::json!({"text": "the audit is underway"}),
            ),
            event(
                "e3",
                EventKind::Compact,
                serde_json::json!({"summary": "Only a read-only audit was authorized.", "throughEventId": "e2"}),
            ),
        ];
        let mut goal = crate::goal::Goal::new(
            "implement the issues, verify in isolation, and open the PRs",
            None,
        );
        goal.pause("Paused by the user.");
        let messages = projected_messages_with_goal("sys", &events, "/w", Some(&goal));
        let rendered = serde_json::to_string(&messages).unwrap();
        assert!(rendered.contains("implement the issues, verify in isolation, and open the PRs"));
        assert!(!rendered.contains("audit only and do not implement"));
        let complete = {
            let mut goal = goal;
            goal.status = crate::goal::GoalStatus::Complete;
            goal
        };
        let completed = serde_json::to_string(&projected_messages_with_goal(
            "sys",
            &events,
            "/w",
            Some(&complete),
        ))
        .unwrap();
        assert!(completed.contains("audit only and do not implement"));
        assert!(!completed.contains("implement the issues, verify in isolation, and open the PRs"));
    }

    #[test]
    fn compacted_history_reinjects_todos_and_running_commands() {
        let events = vec![
            Event::new("e1", "now", "t1", EventKind::UserAsk).with_body(&serde_json::json!({"text": "Keep the requirements"})).unwrap(),
            Event::new("e2", "now", "t1", EventKind::Todos).with_body(&serde_json::json!({"items": [{"id": "verify", "title": "Verify the archive tool", "status": "in_progress"}]})).unwrap(),
            Event::new("e3", "now", "t1", EventKind::TaskStart).with_body(&serde_json::json!({"id": "task-1", "argv": ["cargo", "test"]})).unwrap(),
            Event::new("e4", "now", "t1", EventKind::Compact).with_body(&CompactBody {summary: "Summary of earlier work".into(), through_event_id: "e3".into()}).unwrap(),
        ];
        let messages = serde_json::to_string(&projected_messages("system", &events, "/w")).unwrap();
        assert!(messages.contains("Keep the requirements"));
        assert!(messages.contains("Verify the archive tool"));
        assert!(messages.contains("task-1"));
        assert!(messages.contains("cargo"));
    }
}
