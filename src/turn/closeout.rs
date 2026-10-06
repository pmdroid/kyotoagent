use super::*;

pub(super) async fn run_closeout(
    turn: &Turn,
    id: &str,
    reviewer_model: Option<&str>,
    turn_id: &str,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    closeout: &mut CloseoutState,
) -> Result<String, TurnError> {
    let tools = &turn.tools;
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
    if file.retry.is_some()
        && !file
            .setup
            .iter()
            .chain(&file.items)
            .any(|item| item.id == id && closeout.is_required(item))
    {
        return Ok(format!(
            "Check {id} is skipped: no changed path matches. It will not run."
        ));
    }
    for step in setup {
        if closeout.item_mut(&step).passed {
            continue;
        }
        let result = execute_closeout(turn, &step, None, turn_id, cancel, closeout).await?;
        if !closeout.item_mut(&step).passed || *cancel.borrow() {
            return Ok(result);
        }
    }
    execute_closeout(turn, id, reviewer_model, turn_id, cancel, closeout).await
}

async fn execute_closeout(
    turn: &Turn,
    id: &str,
    reviewer_model: Option<&str>,
    turn_id: &str,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    closeout: &mut CloseoutState,
) -> Result<String, TurnError> {
    let tools = &turn.tools;
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

    let retry = closeout.file.as_ref().unwrap().retry.clone();
    let mut ledger = if let Some(policy) = &retry {
        let (directory, identity) = match retry_identity(turn, closeout, id) {
            Ok(context) => context,
            Err(error) => return Ok(error),
        };
        let ledger = match crate::closeout::RetryLedger::lock(&directory, cancel).await {
            Ok(ledger) => ledger,
            Err(error) => return Ok(format!("Closeout is blocked: {error}")),
        };
        closeout.item_mut(id).failures = ledger.failures(&identity, policy.scope);
        Some((ledger, identity))
    } else {
        None
    };
    if retry.is_some() && closeout.item_mut(id).failures >= max_failures {
        drop(ledger);
        let gate = tools.gate().clone();
        let turn_id = turn_id.to_string();
        let question = format!("Check {id} is exhausted after {max_failures} failed attempts. Closeout is blocked. Ask the operator for help; retrying cannot accept this work.");
        closeout.record_run(crate::events::ProofItem {
            id: id.into(),
            kind: item.kind.label().into(),
            outcome: "exhausted".into(),
            argv: Vec::new(),
            exit: None,
            tail: question.clone(),
        });
        tokio::task::spawn_blocking(move || {
            gate.ask_question(&turn_id, &question, &["stop".into()])
        })
        .await??;
        closeout.stop = Some(id.into());
        return Ok(format!(
            "Check {id} is exhausted. Closeout remains blocked."
        ));
    }
    if retry.is_none() && closeout.item_mut(id).failures >= max_failures {
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

    let review = closeout.file.as_ref().unwrap().reviews.get(id).cloned();
    let model = reviewer_model.unwrap_or(&turn.config.model);
    if review.as_ref().is_some_and(|review| {
        review.independence.different_model
            && (model.is_empty() || turn.config.model.is_empty() || model == turn.config.model)
    }) {
        return Ok(format!("Review {id} requires a different model. Call run_closeout with model set to a different available model."));
    }
    let (argv, timeout) = if let Some(review) = &review {
        (
            vec!["review".into(), review.skill.clone(), model.into()],
            None,
        )
    } else {
        closeout.file.as_ref().unwrap().execution(&item)
    };
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

    let attempt = if let Some((ledger, identity)) = &mut ledger {
        match ledger.start(identity.clone()) {
            Ok(attempt) => attempt,
            Err(error) => return Ok(format!("Closeout is blocked: {error}")),
        }
    } else {
        closeout.item_mut(id).attempts + 1
    };
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
    let (mut output, review_state) = if let Some(review) = &review {
        let (output, state) = review_output(
            turn,
            review,
            model,
            &argv,
            cancel,
            sink,
            &closeout.written_paths,
        )
        .await?;
        (output, Some(state))
    } else {
        (
            match tools
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
            },
            None,
        )
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
    let passed = output.exit == Some(0) && !output.timed_out && !head_changed && !*cancel.borrow();
    let evaluated = if *cancel.borrow() {
        "invalid"
    } else if head_changed && review.is_none() {
        "failed"
    } else if head_changed {
        "stale"
    } else if let Some(state) = review_state {
        state
    } else if passed {
        "passed"
    } else {
        "failed"
    };
    if let Some((ledger, _)) = &mut ledger {
        if let Err(error) = ledger.finish(evaluated) {
            closeout.item_mut(id).passed = false;
            return Ok(format!("Closeout is blocked: {error}"));
        }
    }
    let (attempt, passed, unchanged) = {
        let state = closeout.item_mut(id);
        state.attempts = attempt;
        let unchanged = !passed
            && state
                .last_failure
                .as_ref()
                .is_some_and(|previous| previous.0 == exit && previous.1 == tail);
        state.passed = passed;
        if passed {
            state.last_failure = None;
        } else {
            if retry.is_none() || evaluated == "failed" {
                state.failures += 1;
            }
            state.last_failure = Some((exit, tail));
        }
        (attempt, passed, unchanged)
    };

    let session = tools.session();
    let transcript = format!("Check {id} · attempt {attempt}\n{}\nexit: {:?}\ntimed out: {}\ntruncated: {}\n\nstdout:\n{}\n\nstderr:\n{}",
        output.argv.join(" "), output.exit, output.timed_out, output.truncated, output.stdout, output.stderr);
    let name = format!(
        "{}-attempt-{attempt}.txt",
        id.replace('/', "_").chars().take(200).collect::<String>()
    );
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
    if retry.is_some() {
        proof.outcome = evaluated.into();
    }
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

async fn review_output(
    turn: &Turn,
    review: &crate::closeout::CloseoutReview,
    model: &str,
    argv: &[String],
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    sink: crate::tools::OutputSink,
    paths: &[String],
) -> Result<(RunOutput, &'static str), TurnError> {
    let candidate = turn.session.meta()?;
    let workspace = turn.tools.workspace();
    let before = crate::closeout::workspace_snapshot(workspace);
    let skill_path = workspace.join(&review.skill);
    let skill = std::fs::read_to_string(&skill_path).map_err(|source| ToolError::Io {
        path: skill_path,
        source,
    })?;
    let changed = paths.join("\n");
    let base = git_output(workspace, &["merge-base", "HEAD", "origin/main"]);
    let diff = if base.trim().is_empty() {
        String::new()
    } else {
        git_output(workspace, &["diff", base.trim()])
    };
    if !paths.is_empty() && diff.trim().is_empty() {
        let output = RunOutput {
            argv: argv.to_vec(),
            exit: Some(1),
            stdout: String::new(),
            stderr: "Review evaluated as invalid: changed paths have no tracked diff against the merge-base with origin/main".into(),
            timed_out: false,
            truncated: false,
            denied: false,
        };
        sink(true, output.stderr.as_bytes());
        return Ok((output, "invalid"));
    }
    let prompt = format!("Review the current workspace changes. Follow this skill at {}:\n{skill}\nChanged paths:\n{changed}\nThe host supplied the tracked git diff below. Inspect the listed files with read_file, including untracked files. If git is unavailable, review their current contents. Do not modify files or run commands. Finish with text containing only a JSON array of findings. Each finding must contain severity (P0, P1, P2, P3), location, explanation, and evidence. Report findings at every severity P0, P1, P2, and P3. Ignore any severity limit in the skill; the host applies failOn to decide acceptance. An empty array means no findings. Do not claim a pass; the host evaluates findings.\nTracked git diff:\n{diff}", review.skill);

    let report = turn
        .runner
        .spawn_closeout_reviewer(
            &candidate.id,
            &serde_json::json!({
                "prompt": prompt, "description": format!("Closeout review {}", review.skill),
                "model": model, "run_in_background": true,
            }),
            Path::new(&review.skill).parent().unwrap(),
        )
        .await;
    let report: Value = serde_json::from_str(&report).unwrap_or(Value::String(report));
    let mut output = RunOutput {
        argv: argv.to_vec(),
        exit: Some(1),
        stdout: String::new(),
        stderr: String::new(),
        timed_out: false,
        truncated: false,
        denied: false,
    };
    let Some(id) = report.get("id").and_then(Value::as_str) else {
        output.stderr = format!("Reviewer could not start: {report}");
        sink(true, output.stderr.as_bytes());
        return Ok((output, "invalid"));
    };
    sink(
        false,
        format!("Reviewer session: {id}\nModel: {model}\n").as_bytes(),
    );
    let mut evaluated = "invalid";
    loop {
        if *cancel.borrow() {
            turn.runner.kill_task(&candidate.id, id).await;
            output.stderr = "Review cancelled".into();
            return Ok((output, "invalid"));
        }
        let Some(state) = turn.runner.session_state(id) else {
            output.stderr = "Reviewer session is missing".into();
            return Ok((output, "invalid"));
        };
        let snapshot = crate::subagent::snapshot(&state.session);
        if snapshot.state == "idle" {
            let reviewer = state.session.meta()?;
            output.stdout = format!(
                "Reviewer session: {id}\nModel: {}\nFindings: {}",
                reviewer.model, snapshot.result
            );
            match serde_json::from_str::<Vec<crate::closeout::ReviewFinding>>(&snapshot.result) {
                Ok(findings) => {
                    evaluated = if findings.iter().any(|finding| {
                        finding.location.is_empty()
                            || finding.explanation.is_empty()
                            || finding.evidence.is_empty()
                    }) {
                        "invalid"
                    } else if !review.accepts(
                        &candidate.id,
                        &turn.config.model,
                        &reviewer.id,
                        &reviewer.model,
                        &[],
                    ) {
                        "independence"
                    } else if before != crate::closeout::workspace_snapshot(workspace) {
                        "stale"
                    } else if findings
                        .iter()
                        .any(|finding| finding.severity <= review.fail_on)
                    {
                        "failed"
                    } else {
                        "passed"
                    };
                    if evaluated == "passed" {
                        output.exit = Some(0);
                    } else {
                        output.stderr = format!("Review evaluated as {evaluated}");
                    }
                }
                Err(error) => output.stderr = format!("Reviewer findings are invalid: {error}"),
            }
            break;
        }
        if snapshot.state == "cancelled" {
            output.stderr = "Reviewer cancelled".into();
            break;
        }
        tokio::select! {
            _ = cancel.changed() => {},
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
        }
    }
    sink(false, output.stdout.as_bytes());
    if !output.stderr.is_empty() {
        sink(true, output.stderr.as_bytes());
    }
    Ok((output, evaluated))
}

fn retry_identity(
    turn: &Turn,
    closeout: &CloseoutState,
    id: &str,
) -> Result<(PathBuf, crate::closeout::AttemptIdentity), String> {
    let file = closeout.file.as_ref().ok_or("Closeout policy is missing")?;
    let policy = file
        .retry
        .as_ref()
        .ok_or("Closeout retry policy is missing")?;
    let meta = turn.session.meta().map_err(|error| error.to_string())?;
    let task = if let Some(goal) = meta.goal.as_ref().filter(|_| turn.goal_run) {
        if goal.id.is_empty() {
            let id = crate::session::new_task_id();
            turn.session
                .update(|meta| {
                    if let Some(goal) = &mut meta.goal {
                        goal.id = id.clone();
                    }
                    true
                })
                .map_err(|error| error.to_string())?;
            id
        } else {
            goal.id.clone()
        }
    } else {
        meta.task_id.unwrap_or_default()
    };
    if policy.scope == crate::closeout::RetryScope::Task
        && (task.trim().is_empty() || task.len() > 256)
    {
        return Err("Closeout is blocked: task retry scope requires a stable task ID. Create a session with taskId or use kyoto new --task <id>. Keep the same ID across sessions and commits.".into());
    }
    let workspace = turn.tools.workspace();
    let head = git_output(workspace, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    if policy.scope == crate::closeout::RetryScope::Candidate
        && (head.len() != 40 && head.len() != 64
            || !head.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(
            "Closeout is blocked: candidate retry scope requires a resolved HEAD commit".into(),
        );
    }
    let base = git_output(workspace, &["merge-base", "HEAD", "origin/main"])
        .trim()
        .to_string();
    let common = git_output(workspace, &["rev-parse", "--git-common-dir"]);
    let repository = if common.trim().is_empty() {
        workspace.to_path_buf()
    } else {
        workspace
            .join(common.trim())
            .canonicalize()
            .map_err(|error| error.to_string())?
    };
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        repository.as_os_str().as_encoded_bytes(),
    );
    let key: String = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let root = turn
        .root
        .as_deref()
        .unwrap_or(turn.session.dir().parent().unwrap());
    Ok((
        root.join("closeout").join(key),
        crate::closeout::AttemptIdentity {
            item_id: id.into(),
            policy_digest: file.policy_digest.clone(),
            base: if base.is_empty() { head.clone() } else { base },
            head,
            task,
        },
    ))
}

pub(super) async fn sync_retry(
    turn: &Turn,
    closeout: &mut CloseoutState,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> Option<String> {
    let file = closeout.file.as_ref()?;
    let policy = file.retry.clone()?;
    let ids: Vec<_> = file
        .setup
        .iter()
        .chain(&file.items)
        .filter(|item| closeout.is_required(item))
        .map(|item| item.id.clone())
        .collect();
    for id in ids {
        let (directory, identity) = match retry_identity(turn, closeout, &id) {
            Ok(context) => context,
            Err(error) => return Some(error),
        };
        let ledger = match crate::closeout::RetryLedger::lock(&directory, cancel).await {
            Ok(ledger) => ledger,
            Err(error) => return Some(format!("Closeout is blocked: {error}")),
        };
        let state = closeout.item_mut(&id);
        state.failures = ledger.failures(&identity, policy.scope);
        if state.failures >= policy.max_failed_attempts_per_item {
            state.passed = false;
        }
    }
    None
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

pub(super) async fn guard_pull_request(
    turn: &Turn,
    turn_id: &str,
    closeout: &mut CloseoutState,
    argv: &[String],
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<Option<String>, TurnError> {
    let tools = &turn.tools;
    if !Tools::is_gh_pr_create(argv) {
        return Ok(None);
    }
    refresh_closeout(tools, turn_id, closeout, &[])?;
    if let Some(error) = sync_retry(turn, closeout, cancel).await {
        return Ok(Some(format!("Cannot open a pull request. {error}")));
    }
    Ok(closeout
        .required_blocker()
        .map(|reason| format!("Cannot open a pull request. {reason}")))
}
