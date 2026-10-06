use super::*;

pub(super) struct HistoryCursor {
    pub active_start: usize,
    pub compact_id: Option<String>,
}

pub(super) fn spawn_compact(
    session: Session,
    slot: Arc<CompactSlot>,
    client: ChatClient,
    config: Config,
    preserve_turn: Option<String>,
    on_finish: impl FnOnce() + Send + 'static,
) {
    let (job, started) = slot.begin();
    if !started {
        return;
    }
    let mut cancel = job.cancel.subscribe();
    tokio::spawn(async move {
        let _ = run_compact(
            &session,
            &client,
            &config,
            &mut cancel,
            preserve_turn.as_deref(),
        )
        .await;
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
        result = run_compact(session, client, config, &mut job_cancel, None) => result,
    };
    slot.finish(&job);
    result
}

pub(super) async fn run_compact(
    session: &Session,
    client: &ChatClient,
    config: &Config,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    preserve_turn: Option<&str>,
) -> Result<(), TurnError> {
    if *cancel.borrow() {
        return Ok(());
    }
    let all_events = session.events()?;
    let previous_compact = crate::compact::latest_compact(&all_events).cloned();
    let events = match preserve_turn
        .and_then(|turn| all_events.iter().position(|event| event.turn_id == turn))
    {
        Some(index) => &all_events[..index],
        None => &all_events,
    };
    if previous_compact
        .as_ref()
        .and_then(|event| event.body_as::<CompactBody>().ok())
        .and_then(|body| {
            all_events
                .iter()
                .position(|event| event.id == body.through_event_id)
        })
        .is_some_and(|index| index >= events.len())
    {
        return Ok(());
    }
    if !events.iter().any(|event| {
        matches!(
            event.kind,
            EventKind::ModelMessage | EventKind::ToolResult | EventKind::Result
        )
    }) {
        return Ok(());
    }
    let Some(through) = crate::compact::last_event_id(events) else {
        return Ok(());
    };
    let mut history = events.to_vec();
    if let Some(compact) =
        previous_compact.filter(|compact| !history.iter().any(|event| event.id == compact.id))
    {
        history.push(compact);
    }
    let mut messages = crate::compact::compact_request_messages(&history);
    let meta = session.meta()?;
    let window = crate::compact::resolve_window(client, config, session).await?;
    if let Some(window) = window {
        crate::compact::fit_compact_messages(&mut messages, window.saturating_mul(85) / 100);
        if crate::compact::estimate_tokens(&messages) > window.saturating_mul(85) / 100 {
            return Ok(());
        }
    }
    let reply = tokio::select! {
        _ = cancel.changed() => return Ok(()),
        result = client.compact(&messages) => result,
    };
    let reply = if reply
        .as_ref()
        .is_err_and(|error| error.is_context_overflow())
    {
        let budget = window
            .unwrap_or_else(|| crate::compact::estimate_tokens(&messages))
            .saturating_mul(40)
            / 100;
        let budget = budget.min(crate::compact::estimate_tokens(&messages) / 2);
        crate::compact::fit_compact_messages(&mut messages, budget);
        tokio::select! {
            _ = cancel.changed() => return Ok(()),
            result = client.compact(&messages) => result,
        }
    } else {
        reply
    };
    let Ok(reply) = reply else {
        return Ok(());
    };
    let summary = reply.text().trim().to_string();
    if summary.is_empty() || *cancel.borrow() {
        return Ok(());
    }
    let turn_id = events
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
    history.push(
        Event::new("compact-candidate", &now(), &turn_id, EventKind::Compact).with_body(&body)?,
    );
    let after = crate::compact::estimate_tokens(&crate::compact::projected_messages(
        "",
        &history,
        &meta.workspace,
    ));
    if after >= before {
        return Ok(());
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
    let Some(window) = window else {
        return Ok(());
    };
    if !*attempted && crate::compact::at_or_above(tokens, window, turn.compact_percent) {
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
    if estimated > window.saturating_mul(95) / 100 && !*cancel.borrow() {
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

pub(super) async fn maybe_prefire(turn: &Turn) {
    let Ok(meta) = turn.session.meta() else {
        return;
    };
    let (Some(tokens), Some(window)) = (meta.prompt_tokens, meta.context_length) else {
        return;
    };
    if !crate::compact::at_or_above(tokens, window, turn.prefire_percent) {
        return;
    }
    let runner = Arc::clone(&turn.runner);
    let state = Arc::clone(&turn.state);
    spawn_compact(
        turn.session.clone(),
        Arc::clone(&turn.compact),
        turn.client.clone(),
        turn.config.clone(),
        None,
        move || runner.after_idle(&state),
    );
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
