use super::*;

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
    preserve_turn: &str,
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
        result = run_compact(session, client, config, &mut job_cancel, Some(preserve_turn)) => result,
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
    let events = session.events()?;
    let previous_compact = crate::compact::latest_compact(&events).cloned();
    let events = match preserve_turn
        .and_then(|turn| events.iter().position(|event| event.turn_id == turn))
    {
        Some(index) => &events[..index],
        None => &events,
    };
    if events.is_empty() {
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
    let messages = crate::compact::compact_request_messages(&history);
    let meta = session.meta()?;
    let window = crate::compact::resolve_window(client, config, session).await?;
    if window.is_some_and(|window| crate::compact::estimate_tokens(&messages) > window) {
        return Ok(());
    }
    let reply = tokio::select! {
        _ = cancel.changed() => return Ok(()),
        result = client.compact(&messages) => result,
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
    active_start: &mut usize,
    compact_id: &mut Option<String>,
) -> Result<(), TurnError> {
    let window = crate::compact::resolve_window(&turn.client, &turn.config, &turn.session).await?;
    refresh_compacted_history(turn, transcript, active_start, compact_id)?;
    transcript[0] = Message::System {
        content: turn.system_prompt(),
    };
    let tokens = crate::compact::estimate_request(transcript, tools_json)
        .max(turn.session.meta()?.prompt_tokens.unwrap_or(0));
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
            &turn.turn_id,
        )
        .await?;
        refresh_compacted_history(turn, transcript, active_start, compact_id)?;
    }
    let estimated = crate::compact::estimate_request(transcript, tools_json);
    if estimated > window && !*cancel.borrow() {
        return Err(TurnError::ContextLimit {
            estimated,
            limit: window,
        });
    }
    Ok(())
}

fn refresh_compacted_history(
    turn: &Turn,
    transcript: &mut Vec<Message>,
    active_start: &mut usize,
    compact_id: &mut Option<String>,
) -> Result<(), TurnError> {
    let events = turn.session.event_snapshot()?.1;
    let latest = crate::compact::latest_compact(&events);
    let latest_id = latest.map(|event| event.id.clone());
    if latest_id == *compact_id {
        return Ok(());
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
    let active = transcript.split_off(*active_start);
    let workspace = turn.tools.workspace().to_string_lossy();
    *transcript = crate::compact::projected_messages(&turn.system_prompt(), &history, &workspace);
    *active_start = transcript.len();
    transcript.extend(active);
    *compact_id = latest_id;
    Ok(())
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
