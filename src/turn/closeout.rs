use super::*;

pub(super) async fn run_closeout(
    tools: &Tools,
    id: &str,
    turn_id: &str,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    closeout: &mut CloseoutState,
) -> Result<String, TurnError> {
    refresh_closeout(tools, turn_id, closeout, &[])?;
    let Some(file) = closeout.file.as_ref() else {
        return Ok(format!("unknown closeout id: {id}"));
    };
    if !file
        .setup
        .iter()
        .chain(&file.items)
        .any(|item| item.id == id)
    {
        return Ok(format!("unknown closeout id: {id}"));
    }
    let setup: Vec<_> = file
        .setup
        .iter()
        .take_while(|step| step.id != id)
        .filter(|step| closeout.is_required(step))
        .map(|step| step.id.clone())
        .collect();
    for step in setup {
        if closeout.item_mut(&step).passed {
            continue;
        }
        let result = execute_closeout(tools, &step, turn_id, cancel, closeout).await?;
        if !closeout.item_mut(&step).passed || *cancel.borrow() {
            return Ok(result);
        }
    }
    execute_closeout(tools, id, turn_id, cancel, closeout).await
}

async fn execute_closeout(
    tools: &Tools,
    id: &str,
    turn_id: &str,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    closeout: &mut CloseoutState,
) -> Result<String, TurnError> {
    refresh_closeout(tools, turn_id, closeout, &[])?;
    let (item, max_failures) = match closeout.file.as_ref().and_then(|file| {
        file.setup
            .iter()
            .chain(&file.items)
            .find(|item| item.id == id)
            .map(|item| (item.clone(), file.max_failures))
    }) {
        Some(found) => found,
        None => return Ok(format!("unknown closeout id: {id}")),
    };

    if closeout.item_mut(id).failures >= max_failures {
        let gate = tools.gate().clone();
        let turn_id = turn_id.to_string();
        let question = format!("Check {id} used all {max_failures} failed attempts.");
        let answer = tokio::task::spawn_blocking(move || {
            gate.ask_question(
                &turn_id,
                &question,
                &["continue".to_string(), "stop".to_string()],
            )
        })
        .await??;
        if answer == "stop" {
            closeout.stop = Some(id.to_string());
            return Ok(format!("Check {id} did not pass. The turn is stopping."));
        }
        closeout.item_mut(id).failures = 0;
        return Ok(format!(
            "The failure count for {id} was cleared. Run it again."
        ));
    }

    let (argv, timeout) = closeout.file.as_ref().unwrap().execution(&item);
    let allowed = {
        let tools = tools.clone();
        let turn_id = turn_id.to_string();
        let argv = argv.clone();
        let id = id.to_string();
        let result =
            tokio::task::spawn_blocking(move || tools.run_closeout_allowed(&turn_id, &id, &argv))
                .await?;
        match result {
            Ok(allowed) => allowed,
            Err(error) => return Ok(error.to_string()),
        }
    };
    if !allowed {
        return Ok("Not allowed, so the check did not run.".to_string());
    }

    let attempt = closeout.item_mut(id).attempts + 1;
    append_with_body(
        tools.session(),
        turn_id,
        EventKind::CloseoutStarted,
        &crate::events::CloseoutStartedBody {
            id: id.to_string(),
            attempt,
        },
    )?;
    let output_error = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink: crate::tools::OutputSink = {
        let tools = tools.clone();
        let turn_id = turn_id.to_string();
        let id = id.to_string();
        let output_error = output_error.clone();
        std::sync::Arc::new(move |stderr, bytes| {
            let mut error_slot = output_error.lock().unwrap();
            let result = append_with_body(
                tools.session(),
                &turn_id,
                EventKind::CloseoutOutput,
                &crate::events::CloseoutOutputBody {
                    id: id.clone(),
                    attempt,
                    stderr,
                    bytes: bytes.to_vec(),
                },
            );
            if let Err(error) = result {
                *error_slot = Some(error);
            }
        })
    };
    let head = git_output(tools.workspace(), &["rev-parse", "HEAD"]);
    let mut output = match tools
        .execute_streaming(&argv, timeout, cancel, Some(sink))
        .await
    {
        Ok(output) => output,
        Err(error) => RunOutput {
            argv: argv.clone(),
            exit: None,
            stdout: String::new(),
            stderr: error.to_string(),
            timed_out: false,
            truncated: false,
            denied: false,
        },
    };
    if let Some(error) = output_error.lock().unwrap().take() {
        return Err(error.into());
    }
    let head_changed = head != git_output(tools.workspace(), &["rev-parse", "HEAD"]);
    if head_changed {
        output
            .stderr
            .push_str("\nCloseout failed because the command changed HEAD.");
    }
    refresh_closeout(tools, turn_id, closeout, &[])?;
    let exit = output.exit.unwrap_or(-1);
    let tail = crate::closeout::tail_of(&output);
    let (attempt, passed, unchanged) = {
        let state = closeout.item_mut(id);
        state.attempts += 1;
        let attempt = state.attempts;
        let passed =
            output.exit == Some(0) && !output.timed_out && !head_changed && !*cancel.borrow();
        let unchanged = !passed
            && state
                .last_failure
                .as_ref()
                .is_some_and(|previous| previous.0 == exit && previous.1 == tail);
        state.passed = passed;
        if passed {
            state.last_failure = None;
        } else {
            state.failures += 1;
            state.last_failure = Some((exit, tail));
        }
        (attempt, passed, unchanged)
    };

    let session = tools.session();
    let transcript = format!("Check {id} · attempt {attempt}\n{}\nexit: {:?}\ntimed out: {}\ntruncated: {}\n\nstdout:\n{}\n\nstderr:\n{}",
        output.argv.join(" "), output.exit, output.timed_out, output.truncated, output.stdout, output.stderr);
    let name = format!("{id}-attempt-{attempt}.txt");
    let file =
        crate::proof::store_bytes(session, &name, transcript.as_bytes()).map_err(|source| {
            ToolError::Io {
                path: session.dir().join("proof"),
                source,
            }
        })?;
    let mut body = crate::closeout::run_body(id, attempt, &output);
    body.transcript = Some(file.clone());
    body.passed = Some(passed);
    let event = Event::new(
        &session.next_event_id()?,
        &now(),
        turn_id,
        EventKind::CloseoutRun,
    )
    .with_body(&body)?;
    session.append(&event)?;

    let mut proof = crate::closeout::proof_item(
        id,
        passed,
        argv,
        output.exit.unwrap_or(-1),
        crate::closeout::tail_of(&output),
    );
    proof.kind = item.kind.label().to_string();
    closeout.record_run(proof);

    let mut text = if passed {
        format!("Check {id} passed on attempt {attempt}.")
    } else {
        let mut text = format!(
            "Check {id} failed on attempt {attempt}. Hint: {}. Fix this and run_closeout again, or ask if you are stuck.",
            item.hint
        );
        if unchanged {
            text.push_str(" This output is unchanged.");
        }
        text
    };
    text.push_str(&format!(
        "\nRetained transcript: {}\nPublish only if useful with attach_artifact file_id={}",
        serde_json::to_string(&file)?,
        file.id
    ));
    Ok(text)
}

pub(super) fn refresh_closeout(
    tools: &Tools,
    turn_id: &str,
    closeout: &mut CloseoutState,
    written: &[String],
) -> Result<(), TurnError> {
    if closeout.file.is_none() {
        return Ok(());
    }
    let mut paths = closeout.refresh_workspace(tools.workspace());
    for path in written {
        if !Path::new(path).is_absolute() && !paths.contains(path) {
            closeout.record_write(path);
            paths.push(path.clone());
        }
    }
    if !paths.is_empty() {
        append_with_body(
            tools.session(),
            turn_id,
            EventKind::CloseoutChanged,
            &crate::events::CloseoutChangedBody { paths },
        )?;
    }
    Ok(())
}

pub(super) fn guard_pull_request(
    tools: &Tools,
    turn_id: &str,
    closeout: &mut CloseoutState,
    argv: &[String],
) -> Result<Option<String>, TurnError> {
    if !Tools::is_gh_pr_create(argv) {
        return Ok(None);
    }
    refresh_closeout(tools, turn_id, closeout, &[])?;
    Ok(closeout
        .required_blocker()
        .map(|reason| format!("Cannot open a pull request. {reason}")))
}
