use super::*;

/// Run one turn: the model loop until it finishes, the user cancels, or a
/// model request fails.
pub(super) async fn run_turn(
    turn: &Turn,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<(), TurnError> {
    let session = &turn.session;
    let tools = &turn.tools;
    let client = &turn.client;
    let turn_id = turn.turn_id.as_str();
    let system_prompt = turn.system_prompt();
    let text = turn.text.as_str();
    let workspace = tools.workspace().to_path_buf();
    let prior_events = session.events()?;

    // The ask opens the turn: a user_ask event and the working status.
    let ask = Event::new(
        &session.next_event_id()?,
        &now(),
        turn_id,
        EventKind::UserAsk,
    )
    .with_body(&AskBody {
        images: turn.images.clone(),
        text: text.to_string(),
        context: turn.context.clone(),
        skill: turn.skill.clone(),
        silent: turn.silent,
    })?;
    let workspace_text = workspace.to_string_lossy().to_string();
    session.append(&ask)?;
    set_status(session, Status::Working)?;
    maybe_title(turn);

    let requested = session.meta()?.requested_workspace;
    let policy = if session.meta()?.parent_id.is_some() {
        Ok(None)
    } else {
        turn.config.closeout_for(&workspace, requested.as_deref())
    };
    let mut closeout = match policy {
        Ok(file) => CloseoutState::with_file(&workspace, file),
        Err(error) => {
            let result_text = error.to_string();
            goal::pause_unfinished(turn, &result_text)?;
            append_result(session, turn_id, &result_text, "")?;
            turn.flight.clear();
            set_status(session, Status::Idle)?;
            append_proof(
                session,
                turn_id,
                &ProofBody {
                    files: Vec::new(),
                    text: String::new(),
                    wrote: Vec::new(),
                    head: git_output(&workspace, &["rev-parse", "HEAD"]),
                    workspace_fingerprint: current_workspace_fingerprint(&workspace),
                    status: git_output(&workspace, &["status", "--porcelain"]),
                    diff_stat: git_output(&workspace, &["diff", "--stat"]),
                    note: String::new(),
                    failures: Vec::new(),
                    items: Vec::new(),
                },
            )?;
            return Ok(());
        }
    };

    closeout.base_ref_name = session.meta()?.base_ref_name;
    closeout.replay(&prior_events);
    if let Some(proof) = prior_events
        .iter()
        .rev()
        .filter(|event| event.kind == EventKind::Proof)
        .find_map(|event| event.body_as::<ProofBody>().ok())
    {
        if !proof.workspace_fingerprint.is_empty()
            && proof.workspace_fingerprint != current_workspace_fingerprint(&workspace)
        {
            let mut paths: Vec<String> =
                git_output(&workspace, &["diff", "--name-only", "-z", &proof.head])
                    .split('\0')
                    .filter(|path| !path.is_empty())
                    .map(str::to_string)
                    .collect();
            paths.extend(
                git_output(
                    &workspace,
                    &[
                        "ls-files",
                        "-z",
                        "--modified",
                        "--deleted",
                        "--others",
                        "--exclude-standard",
                    ],
                )
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_string),
            );
            paths.sort();
            paths.dedup();
            if paths.is_empty() {
                paths.push(".".into());
            }
            for path in &paths {
                closeout.record_write(path);
            }
            append_with_body(
                session,
                turn_id,
                EventKind::CloseoutChanged,
                &crate::events::CloseoutChangedBody { paths },
            )?;
        }
    }

    let initial_events = session.event_snapshot()?.1;
    let mut transcript =
        crate::compact::projected_messages(&system_prompt, &initial_events, &workspace_text);
    let mut history_cursor = HistoryCursor {
        active_start: transcript.len() - 1,
        compact_id: crate::compact::latest_compact(&initial_events).map(|event| event.id.clone()),
    };
    let profile = turn.session.meta().ok().and_then(|meta| meta.profile);
    let mut tool_defs = tool_definitions_for(&turn.config, turn.child, profile.as_deref());
    if session.meta()?.closeout_reviewer {
        tool_defs.retain(|tool| reviewer_tool(&tool.name));
    }
    let tools_json = serde_json::to_string(&tool_defs)?;

    let mut result_text = String::new();
    let mut note = String::new();
    let mut proof_text = String::new();
    let mut proof_files = Vec::new();
    let mut finished = false;
    let mut writes: Vec<String> = Vec::new();
    let mut failures: Vec<ProofFailure> = Vec::new();
    let mut live_compact_attempted = false;
    let mut usage = None;
    let mut overflow_retried = false;

    let mut goal_prepared = !turn.goal_run;

    loop {
        if let Some(reason) = goal::goal_stop(turn)? {
            result_text = reason;
            break;
        }
        if *cancel.borrow() || tools.gate().rejected() {
            result_text = "Stopped.".to_string();
            break;
        }

        if let Err(error) = maybe_live_compact(
            turn,
            &mut cancel,
            &mut transcript,
            &tools_json,
            &mut live_compact_attempted,
            &mut history_cursor,
            &mut usage,
        )
        .await
        {
            if matches!(
                error,
                TurnError::ContextLimit { .. } | TurnError::Compaction(_)
            ) {
                result_text = error.to_string();
                break;
            }
            return Err(error);
        }
        if *cancel.borrow() {
            result_text = "Stopped.".to_string();
            break;
        }

        if !goal_prepared {
            goal::prepare(turn, &transcript, &mut cancel).await?;
            transcript[0] = Message::System {
                content: turn.system_prompt(),
            };
            goal_prepared = true;
            continue;
        }
        turn.flight.begin_thinking();
        let reply = tokio::select! {
            biased;
            _ = cancel.changed() => {
                turn.flight.clear();
                result_text = "Stopped.".to_string();
                break;
            }
            result = client.complete_with_status(
                &transcript,
                &tool_defs,
                Some(&turn.flight.thoughts),
                Some(&turn.flight.retry_status),
            ) => result,
        };
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                turn.flight.clear();
                if !overflow_retried && error.is_context_overflow() {
                    overflow_retried = true;
                    wait_and_run_compact(&turn.compact, session, client, &turn.config, &mut cancel)
                        .await?;
                    if refresh_compacted_history(turn, &mut transcript, &mut history_cursor)? {
                        usage = None;
                        live_compact_attempted = false;
                        continue;
                    }
                }
                result_text = error.to_string();
                break;
            }
        };
        goal::charge_goal(turn, &transcript, &reply)?;
        if let Some(reason) = goal::goal_stop(turn)? {
            result_text = reason;
            break;
        }
        let tokens = reply
            .prompt_tokens
            .unwrap_or_else(|| crate::compact::estimate_request(&transcript, &tools_json));
        usage = Some((
            crate::compact::estimate_request(&transcript, &tools_json),
            tokens,
        ));
        crate::compact::store_prompt_tokens(session, tokens)?;
        let _ = crate::compact::resolve_window(&turn.client, &turn.config, session).await;

        // The model message goes to the log for the transcript, never a card.
        append_model_message(session, turn_id, &reply)?;
        transcript.push(Message::assistant_reply(&reply));

        // Text and no tool call is the result.
        if !reply.wants_tools() {
            turn.flight.clear();
            if let Some(reason) =
                completion_blocker(turn, reply.text(), &transcript, &mut cancel, &mut closeout)
                    .await?
            {
                transcript.push(Message::User {
                    content: reason.into(),
                });
                continue;
            }
            result_text = goal::verified_result(turn, reply.text())?;
            break;
        }
        turn.flight.begin_tools();

        let mut stopped = false;
        let mut turn_stop = false;
        let mut read_images = Vec::new();
        let mut calls = reply.tool_calls.iter().peekable();
        while let Some(call) = calls.next() {
            turn.flight.tool_action(&call.name);
            if *cancel.borrow() || tools.gate().rejected() {
                stopped = true;
            }
            if session.meta().is_ok_and(|meta| meta.archived) {
                stopped = true;
            }
            if !finished {
                if let Some(reason) = goal::goal_stop(turn)? {
                    result_text = reason;
                    turn_stop = true;
                }
            }
            if stopped || finished || turn_stop {
                let args = parse_args(&call.arguments).unwrap_or_else(|_| serde_json::json!({}));
                let output = "Skipped because the turn ended.";
                append_tool_call(session, turn_id, call, &args)?;
                append_tool_result(session, turn_id, call, output)?;
                transcript.push(Message::tool_result(&call.id, output));
                continue;
            }
            if batch::parallel(&call.name) && !(closeout.file.is_some() && call.name == "run") {
                let mut group = vec![call];
                while calls.peek().is_some_and(|next| {
                    batch::parallel(&next.name) && !(closeout.file.is_some() && next.name == "run")
                }) {
                    group.push(calls.next().unwrap());
                }
                let outcomes = batch::execute(turn, &group, &cancel, &closeout).await?;
                let written: Vec<String> = outcomes
                    .iter()
                    .filter_map(|outcome| outcome.wrote.clone())
                    .collect();
                refresh_closeout(tools, turn_id, &mut closeout, &written)?;
                for (call, outcome) in group.into_iter().zip(outcomes) {
                    if let Some(path) = outcome.wrote {
                        writes.push(path);
                    }
                    if let Some(failure) = outcome.failure {
                        failures.push(failure);
                    }
                    let output = append_tool_output(
                        session,
                        turn_id,
                        call,
                        &outcome.summary,
                        &outcome.images,
                        outcome.is_error,
                    )?;
                    read_images.extend(outcome.images);
                    transcript.push(Message::tool_result(&call.id, &output));
                }
                stopped = *cancel.borrow() || tools.gate().rejected();
                continue;
            }
            let args = match parse_args(&call.arguments) {
                Ok(args) => args,
                Err(error) => {
                    let output = bad_arguments_result(&error, &call.arguments);
                    append_tool_call(session, turn_id, call, &serde_json::json!({}))?;
                    append_tool_output(session, turn_id, call, &output, &[], true)?;
                    transcript.push(Message::tool_result(&call.id, &output));
                    continue;
                }
            };
            append_tool_call(session, turn_id, call, &args)?;

            if !turn.config.tool_allowed(profile.as_deref(), &call.name) {
                let output = format!("unknown tool: {}", call.name);
                append_tool_output(session, turn_id, call, &output, &[], true)?;
                transcript.push(Message::tool_result(&call.id, &output));
                continue;
            }

            if call.name == "attach_artifact" {
                let check = match args.get("check") {
                    None | Some(Value::Null) => None,
                    Some(value)
                        if value.is_object()
                            && value
                                .get("id")
                                .and_then(Value::as_str)
                                .is_some_and(|id| id.trim().is_empty()) =>
                    {
                        None
                    }
                    Some(value) => {
                        let candidate =
                            serde_json::from_value::<crate::events::ArtifactCheck>(value.clone());
                        let check = candidate.ok().filter(|check| {
                            session.events().ok().is_some_and(|events| {
                                events.iter().any(|event| {
                                    event.kind == EventKind::CloseoutRun
                                        && event.body["id"] == check.id
                                        && event.body["attempt"] == check.attempt
                                        && check.event_id.as_ref().is_none_or(|id| id == &event.id)
                                        && (args
                                            .get("file_id")
                                            .and_then(Value::as_str)
                                            .is_some_and(|id| !id.trim().is_empty())
                                            || event.turn_id == turn_id)
                                })
                            })
                        });
                        if check.is_none() {
                            let output = "Attachment check must name an actual closeout attempt on this turn. Omit check for an ordinary file attachment.";
                            append_tool_output(session, turn_id, call, output, &[], true)?;
                            transcript.push(Message::tool_result(&call.id, output));
                            continue;
                        }
                        check
                    }
                };
                let attachment_tools = tools.clone();
                let attachment_turn = turn_id.to_string();
                let attachment_args = args.clone();
                let mut is_error = false;
                let output = match tokio::task::spawn_blocking(move || {
                    artifact_file(&attachment_tools, &attachment_turn, &attachment_args)
                })
                .await?
                {
                    Ok(Some(file)) => {
                        let archived_check = session.events()?.iter().find_map(|event| {
                            if event.kind != EventKind::CloseoutRun {
                                return None;
                            }
                            let run = event.body_as::<crate::events::CloseoutRunBody>().ok()?;
                            (run.transcript.as_ref()?.id == file.id).then_some(
                                crate::events::ArtifactCheck {
                                    id: run.id,
                                    attempt: run.attempt,
                                    event_id: Some(event.id.clone()),
                                },
                            )
                        });
                        if check.as_ref().zip(archived_check.as_ref()).is_some_and(
                            |(given, actual)| {
                                given.id != actual.id
                                    || given.attempt != actual.attempt
                                    || given
                                        .event_id
                                        .as_ref()
                                        .is_some_and(|id| Some(id) != actual.event_id.as_ref())
                            },
                        ) {
                            is_error = true;
                            "Attachment check does not match the archived transcript.".to_string()
                        } else {
                            crate::proof::publish(
                                session,
                                turn_id,
                                &crate::events::ArtifactBody {
                                    source: crate::events::ArtifactSource::Agent,
                                    file: file.clone(),
                                    caption: string_arg(&args, "caption"),
                                    check: archived_check.or(check),
                                },
                            )?;
                            let output = serde_json::to_string(&file)?;
                            proof_files.push(file);
                            output
                        }
                    }
                    Ok(None) => {
                        is_error = true;
                        "Artifact attachment denied.".to_string()
                    }
                    Err(error) => {
                        is_error = true;
                        error
                    }
                };
                append_tool_output(session, turn_id, call, &output, &[], is_error)?;
                transcript.push(Message::tool_result(&call.id, &output));
                stopped = *cancel.borrow() || tools.gate().rejected();
                continue;
            }

            if call.name == "finish" {
                let (text, finish_note, proof) = finish_args(&args)?;
                if let Some(reason) =
                    completion_blocker(turn, &text, &transcript, &mut cancel, &mut closeout).await?
                {
                    append_tool_result(session, turn_id, call, &reason)?;
                    transcript.push(Message::tool_result(&call.id, &reason));
                    continue;
                }
                result_text = goal::verified_result(turn, &text)?;
                note = finish_note;
                proof_text = proof.trim().to_string();
                append_tool_result(session, turn_id, call, &result_text)?;
                transcript.push(Message::tool_result(&call.id, &result_text));
                append_result(session, turn_id, &result_text, &note)?;
                finished = true;
                continue;
            }

            if hooks::fires(&call.name) {
                if let Some(reason) = hooks::load(&workspace)
                    .pre_tool_use(&workspace, &call.name, &args)
                    .await
                {
                    append_tool_result(session, turn_id, call, &reason)?;
                    transcript.push(Message::tool_result(&call.id, &reason));
                    continue;
                }
            }

            let mut outcome =
                execute_tool(turn, tools, call, &args, &mut cancel, &mut closeout).await?;
            if hooks::fires(&call.name) {
                if let Some(feedback) = hooks::load(&workspace)
                    .post_tool_use(&workspace, &call.name, &args, &outcome.summary)
                    .await
                {
                    if !outcome.summary.is_empty() && !feedback.is_empty() {
                        outcome.summary.push_str("\n\n");
                    }
                    outcome.summary.push_str(&feedback);
                }
            }
            refresh_closeout(tools, turn_id, &mut closeout, outcome.wrote.as_slice())?;
            if let Some(path) = outcome.wrote {
                writes.push(path);
            }
            if let Some(failure) = outcome.failure {
                failures.push(failure);
            }
            let output = append_tool_output(
                session,
                turn_id,
                call,
                &outcome.summary,
                &outcome.images,
                outcome.is_error,
            )?;
            read_images.extend(outcome.images);
            transcript.push(Message::tool_result(&call.id, &output));
            stopped = *cancel.borrow() || tools.gate().rejected();
            if let Some(id) = closeout.stop.take() {
                result_text = format!("Check {id} did not pass.");
                turn_stop = true;
            }
        }

        if let Some(images) = Message::tool_images(&read_images) {
            transcript.push(images);
        }

        if stopped {
            result_text = "Stopped.".to_string();
            break;
        }
        if turn_stop {
            break;
        }
        if finished {
            break;
        }
    }

    goal::pause_unfinished(turn, &result_text)?;

    // A turn that ended any way but finish still owes a result event.
    if !finished {
        append_result(session, turn_id, &result_text, &note)?;
    }
    turn.flight.clear();
    if turn.compact.requested.swap(false, Ordering::SeqCst) && !*cancel.borrow() {
        if let Err(error) =
            wait_and_run_compact(&turn.compact, session, client, &turn.config, &mut cancel).await
        {
            append_result(session, turn_id, &error.to_string(), "")?;
        }
    }
    set_status(session, Status::Idle)?;

    // The proof event: what the turn wrote, the git state of the workspace, and
    // every command that did not succeed. The note is the one from `finish`;
    // cancel and model-error turns leave it empty.
    for event in session
        .events()?
        .iter()
        .filter(|event| event.turn_id == turn_id && event.kind == EventKind::Artifact)
    {
        let artifact: crate::events::ArtifactBody = event.body_as()?;
        if !proof_files.iter().any(|file| file.id == artifact.file.id) {
            proof_files.push(artifact.file);
        }
    }
    let proof = ProofBody {
        files: proof_files,
        text: proof_text,
        wrote: writes,
        head: git_output(&workspace, &["rev-parse", "HEAD"]),
        workspace_fingerprint: current_workspace_fingerprint(&workspace),
        status: git_output(&workspace, &["status", "--porcelain"]),
        diff_stat: git_output(&workspace, &["diff", "--stat"]),
        note,
        failures,
        items: closeout.proof_items_with_carried_passes(),
    };
    append_proof(session, turn_id, &proof)?;

    Ok(())
}

fn current_workspace_fingerprint(workspace: &Path) -> String {
    crate::closeout::workspace_fingerprint(
        workspace,
        &git_output(workspace, &["rev-parse", "HEAD"]),
        &git_output(workspace, &["status", "--porcelain"]),
    )
}

async fn completion_blocker(
    turn: &Turn,
    text: &str,
    transcript: &[Message],
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    closeout: &mut CloseoutState,
) -> Result<Option<String>, TurnError> {
    if let Some(reason) = goal::evaluate(turn, text, transcript, cancel).await? {
        return Ok(Some(reason));
    }
    let workspace = turn.tools.workspace();
    refresh_closeout(&turn.tools, &turn.turn_id, closeout, &[])?;
    if let Some(error) = sync_retry(turn, closeout, cancel).await {
        return Ok(Some(error));
    }
    if let Some(reason) = closeout.cannot_finish() {
        return Ok(Some(reason));
    }
    if let Some(id) = turn.tools.tasks().open_id_on_turn(&turn.turn_id)? {
        return Ok(Some(format!(
            "Task {id} from this turn is still running. Use check_task."
        )));
    }
    let hooks = hooks::load(workspace);
    let reason = tokio::select! {
        biased;
        _ = cancel.changed() => Some("Stopped.".to_string()),
        reason = hooks.stop(workspace) => reason,
    };
    if reason.is_some() {
        return Ok(reason);
    }
    refresh_closeout(&turn.tools, &turn.turn_id, closeout, &[])?;
    if let Some(error) = sync_retry(turn, closeout, cancel).await {
        return Ok(Some(error));
    }
    if let Some(reason) = closeout.cannot_finish() {
        return Ok(Some(reason));
    }
    goal::verify_goal(turn, text, transcript, cancel, closeout).await
}

fn artifact_file(
    tools: &Tools,
    turn_id: &str,
    args: &serde_json::Value,
) -> Result<Option<crate::proof::ProofFile>, String> {
    let path = optional_string(args, "path")?;
    let file_id = optional_string(args, "file_id")?;
    if path.is_some() == file_id.is_some() {
        return Err(
            "attach_artifact requires exactly one of path or file_id. Omit unused optional fields."
                .into(),
        );
    }
    let git_sha = optional_string(args, "git_sha")?
        .map(|reference| crate::proof::resolve_git_sha(tools.workspace(), &reference))
        .transpose()?;
    let file = if let Some(path) = path {
        tools
            .attach_artifact(turn_id, &path)
            .map_err(|error| error.to_string())?
    } else {
        let events = tools
            .session()
            .events()
            .map_err(|error| error.to_string())?;
        Some(crate::proof::downloadable_files(&events).map_err(|error| error.to_string())?
            .into_iter().find(|file| Some(file.id.as_str()) == file_id.as_deref())
            .ok_or("file_id must identify a retained file in this session. Use the file_id returned by run_closeout.")?)
    };
    Ok(file.map(|mut file| {
        if git_sha.is_some() {
            file.git_sha = git_sha;
        }
        file
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archived_check_files_require_an_explicit_attachment_and_reuse_their_bytes() {
        let root = std::env::temp_dir().join(format!("ka-explicit-check-{}", std::process::id()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let session = Session::at(&root.join("session"));
        session
            .create(&crate::session::SessionMeta::new(
                "check",
                &workspace,
                "model",
                "2026-10-04T00:00:00.000Z",
            ))
            .unwrap();
        let file =
            crate::proof::store_bytes(&session, "test-attempt-1.txt", b"failed check").unwrap();
        session.append(&Event::new("e1", "2026-10-04T00:00:00.000Z", "t1", EventKind::CloseoutRun)
            .with_body(&serde_json::json!({"id":"test", "attempt":1, "exit":1, "tail":"failed check", "transcript":file})).unwrap()).unwrap();
        let tools = Tools::at(&session).unwrap();
        assert!(crate::proof::artifact_history(&session.events().unwrap())
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(&workspace).unwrap();
        let attached = artifact_file(&tools, "t2", &serde_json::json!({"file_id":file.id}))
            .unwrap()
            .unwrap();
        assert_eq!(attached, file);
        assert_eq!(
            std::fs::read(session.dir().join("proof").join(&attached.id)).unwrap(),
            b"failed check"
        );
        for args in [
            serde_json::json!({"file_id":"unknown"}),
            serde_json::json!({"path":"report.md", "file_id":file.id}),
            serde_json::json!({"file_id":file.id, "path":42}),
        ] {
            assert!(artifact_file(&tools, "t2", &args).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
