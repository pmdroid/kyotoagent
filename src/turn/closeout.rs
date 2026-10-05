use super::*;

pub(super) async fn run_closeout(
    tools: &Tools,
    id: &str,
    turn_id: &str,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    closeout: &mut CloseoutState,
) -> Result<String, TurnError> {
    closeout.refresh_workspace(tools.workspace());
    let (item, max_failures) = match closeout.file.as_ref().and_then(|file| {
        file.items
            .iter()
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

    let argv = vec!["sh".to_string(), "-c".to_string(), item.run.clone()];
    let allowed = {
        let tools = tools.clone();
        let turn_id = turn_id.to_string();
        let run = item.run.clone();
        let id = id.to_string();
        let result =
            tokio::task::spawn_blocking(move || tools.run_closeout_allowed(&turn_id, &id, &run))
                .await?;
        match result {
            Ok(allowed) => allowed,
            Err(error) => return Ok(error.to_string()),
        }
    };
    if !allowed {
        return Ok("Not allowed, so the check did not run.".to_string());
    }

    let output = tools.execute_cancellable(&argv, None, cancel).await?;
    closeout.refresh_workspace(tools.workspace());
    let exit = output.exit.unwrap_or(-1);
    let tail = crate::closeout::tail_of(&output);
    let (attempt, passed, unchanged) = {
        let state = closeout.item_mut(id);
        state.attempts += 1;
        let attempt = state.attempts;
        let passed = output.exit == Some(0) && !output.timed_out;
        let unchanged = !passed
            && state
                .last_failure
                .as_ref()
                .is_some_and(|previous| previous.0 == exit && previous.1 == tail);
        if passed {
            state.passed = true;
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
    let event = Event::new(
        &session.next_event_id()?,
        &now(),
        turn_id,
        EventKind::CloseoutRun,
    )
    .with_body(&body)?;
    session.append(&event)?;

    closeout.record_run(crate::closeout::proof_item(
        id,
        passed,
        argv,
        output.exit.unwrap_or(-1),
        crate::closeout::tail_of(&output),
    ));

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
