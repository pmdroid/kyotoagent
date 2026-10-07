use super::*;
use std::time::Duration;

pub(super) struct HistoryCursor {
    pub active_start: usize,
    pub compact_id: Option<String>,
}

pub(super) fn spawn_compact(
    session: Session,
    slot: Arc<CompactSlot>,
    client: ChatClient,
    config: Config,
    on_finish: impl FnOnce() + Send + 'static,
) {
    let (job, started) = slot.begin();
    if !started {
        return;
    }
    let mut cancel = job.cancel.subscribe();
    tokio::spawn(async move {
        let result = run_compact(&session, &client, &config, &mut cancel).await;
        if let Err(error) = result {
            let turn_id = session
                .events()
                .ok()
                .and_then(|events| events.last().map(|event| event.turn_id.clone()))
                .unwrap_or_else(|| "t1".to_string());
            let _ = append_result(&session, &turn_id, &error.to_string(), "");
        }
        slot.finish(&job);
        on_finish();
    });
}

pub(super) async fn wait_and_run_compact(
    slot: &Arc<CompactSlot>,
    session: &Session,
    client: &ChatClient,
    config: &Config,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), TurnError> {
    let (job, started) = slot.begin();
    if !started {
        wait_compact(slot).await;
        return Ok(());
    }
    let mut job_cancel = job.cancel.subscribe();
    let result = tokio::select! {
        _ = cancel.changed() => {
            let _ = job.cancel.send(true);
            Ok(())
        }
        result = run_compact(session, client, config, &mut job_cancel) => result,
    };
    slot.finish(&job);
    result
}

pub(super) async fn run_compact(
    session: &Session,
    client: &ChatClient,
    config: &Config,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), TurnError> {
    if *cancel.borrow() {
        return Ok(());
    }
    let history = session.events()?;
    if !history.iter().any(|event| {
        matches!(
            event.kind,
            EventKind::ModelMessage | EventKind::ToolResult | EventKind::Result
        )
    }) {
        return Ok(());
    }
    let Some(through) = crate::compact::last_event_id(&history) else {
        return Ok(());
    };
    let mut messages = crate::compact::compact_request_messages(&history);
    let meta = session.meta()?;
    let window = crate::compact::resolve_window(client, config, session).await?;
    if let Some(window) = window {
        crate::compact::fit_compact_messages(&mut messages, window.saturating_mul(85) / 100);
        if crate::compact::estimate_tokens(&messages) > window.saturating_mul(85) / 100 {
            return Ok(());
        }
    }
    let mut summary = None;
    let mut failure = String::from("no usable summary was returned");
    for attempt in 0..3 {
        let reply = tokio::select! {
            _ = cancel.changed() => return Ok(()),
            result = tokio::time::timeout(Duration::from_secs(120), client.compact(&messages)) => result,
        };
        match reply {
            Ok(Ok(reply)) => {
                let text = reply.text().trim().to_string();
                if text.chars().count() >= crate::compact::MIN_SUMMARY_CHARS {
                    summary = Some(text);
                    break;
                }
                failure = String::from("the summary was empty or too short to preserve the task");
            }
            Ok(Err(error)) if error.is_context_overflow() => {
                let budget = crate::compact::estimate_tokens(&messages) / 2;
                crate::compact::fit_compact_messages(&mut messages, budget);
                failure = error.to_string();
                continue;
            }
            Ok(Err(error)) => {
                let retryable = matches!(
                    &error,
                    ChatError::Transport(_)
                        | ChatError::Status {
                            status: 429 | 500..=599,
                            ..
                        }
                        | ChatError::Idle { .. }
                );
                failure = error.to_string();
                if !retryable {
                    break;
                }
            }
            Err(_) => failure = String::from("the summary request timed out"),
        }
        if attempt < 2 {
            tokio::select! {
                _ = cancel.changed() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_secs(3)) => {},
            }
        }
    }
    if *cancel.borrow() {
        return Ok(());
    }
    let summary = summary.ok_or(TurnError::Compaction(failure))?;
    let turn_id = history
        .last()
        .map(|event| event.turn_id.clone())
        .unwrap_or_else(|| "t1".to_string());
    let body = CompactBody {
        summary,
        through_event_id: through,
    };
    let before = crate::compact::estimate_tokens(&crate::compact::projected_messages(
        "",
        &history,
        &meta.workspace,
    ));
    let mut candidate = history.clone();
    candidate.push(
        Event::new("compact-candidate", &now(), &turn_id, EventKind::Compact).with_body(&body)?,
    );
    let after = crate::compact::estimate_tokens(&crate::compact::projected_messages(
        "",
        &candidate,
        &meta.workspace,
    ));
    if after >= before {
        return Err(TurnError::Compaction(
            "the summary did not reduce context usage".to_string(),
        ));
    }
    append_with_body(session, &turn_id, EventKind::Compact, &body)?;
    let events = session.events()?;
    let workspace = session
        .meta()
        .map(|meta| meta.workspace)
        .unwrap_or_default();
    let projected = crate::compact::projected_messages("", &events, &workspace);
    crate::compact::store_prompt_tokens(session, crate::compact::estimate_tokens(&projected))?;
    Ok(())
}

pub(super) async fn maybe_live_compact(
    turn: &Turn,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    transcript: &mut Vec<Message>,
    tools_json: &str,
    attempted: &mut bool,
    cursor: &mut HistoryCursor,
    usage: &mut Option<(u64, u64)>,
) -> Result<(), TurnError> {
    let window = crate::compact::resolve_window(&turn.client, &turn.config, &turn.session).await?;
    if refresh_compacted_history(turn, transcript, cursor)? {
        *usage = None;
        *attempted = false;
    }
    transcript[0] = Message::System {
        content: turn.system_prompt(),
    };
    let estimated = crate::compact::estimate_request(transcript, tools_json);
    let tokens =
        request_tokens(estimated, *usage).max(turn.session.meta()?.prompt_tokens.unwrap_or(0));
    let requested = turn.compact.requested.swap(false, Ordering::SeqCst);
    let threshold_reached = window
        .is_some_and(|window| crate::compact::at_or_above(tokens, window, turn.compact_percent));
    if requested || (!*attempted && threshold_reached) {
        *attempted = true;
        wait_and_run_compact(
            &turn.compact,
            &turn.session,
            &turn.client,
            &turn.config,
            cancel,
        )
        .await?;
        if refresh_compacted_history(turn, transcript, cursor)? {
            *usage = None;
            *attempted = false;
        }
    }
    let estimated = request_tokens(
        crate::compact::estimate_request(transcript, tools_json),
        *usage,
    );
    if let Some(window) =
        window.filter(|window| estimated > window.saturating_mul(95) / 100 && !*cancel.borrow())
    {
        return Err(TurnError::ContextLimit {
            estimated,
            limit: window,
        });
    }
    Ok(())
}

pub(super) fn refresh_compacted_history(
    turn: &Turn,
    transcript: &mut Vec<Message>,
    cursor: &mut HistoryCursor,
) -> Result<bool, TurnError> {
    let events = turn.session.event_snapshot()?.1;
    let latest = crate::compact::latest_compact(&events);
    let latest_id = latest.map(|event| event.id.clone());
    if latest_id == cursor.compact_id {
        return Ok(false);
    }
    let covers_active = latest
        .and_then(|event| event.body_as::<CompactBody>().ok())
        .and_then(|body| {
            events
                .iter()
                .position(|event| event.id == body.through_event_id)
        })
        .zip(
            events
                .iter()
                .position(|event| event.turn_id == turn.turn_id),
        )
        .is_some_and(|(through, start)| through >= start);
    if covers_active {
        *transcript = crate::compact::projected_messages(
            &turn.system_prompt(),
            &events,
            &turn.tools.workspace().to_string_lossy(),
        );
        cursor.active_start = transcript
            .iter()
            .rposition(|message| matches!(message, Message::User { .. }))
            .unwrap_or(1);
        cursor.compact_id = latest_id;
        return Ok(true);
    }
    let mut history = events
        .iter()
        .take_while(|event| event.turn_id != turn.turn_id)
        .cloned()
        .collect::<Vec<_>>();
    if let Some(compact) =
        latest.filter(|compact| !history.iter().any(|event| event.id == compact.id))
    {
        history.push(compact.clone());
    }
    let active = transcript.split_off(cursor.active_start);
    let workspace = turn.tools.workspace().to_string_lossy();
    *transcript = crate::compact::projected_messages(&turn.system_prompt(), &history, &workspace);
    cursor.active_start = transcript.len();
    transcript.extend(active);
    cursor.compact_id = latest_id;
    Ok(true)
}

fn request_tokens(estimated: u64, usage: Option<(u64, u64)>) -> u64 {
    usage
        .map(|(baseline, reported)| reported.saturating_add(estimated.saturating_sub(baseline)))
        .unwrap_or(estimated)
        .max(estimated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_usage_tracks_growth_and_resets_after_replacement() {
        assert_eq!(request_tokens(1000, Some((1000, 80000))), 80000);
        assert_eq!(request_tokens(9000, Some((1000, 80000))), 88000);
        assert_eq!(request_tokens(1000, None), 1000);
    }
}
