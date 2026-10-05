use super::*;
use crate::goal::{Goal, GoalEvidence, GoalStatus};

impl Runner {
    pub(super) fn goal_command(
        &self,
        state: &Arc<SessionState>,
        command: &str,
        images: Vec<crate::attachment::ImageAttachment>,
    ) -> Result<AskOutcome, TurnError> {
        match command {
            "" | "status" => {
                let text = state
                    .session
                    .meta()?
                    .goal
                    .map(|goal| goal.summary())
                    .unwrap_or_else(|| {
                        "No goal set. Use /goal <objective> [--budget <tokens>].".to_string()
                    });
                append_result(&state.session, &next_turn_id(&state.session), &text, "")?;
                Ok(AskOutcome::Ignored)
            }
            "pause" | "clear" => {
                let active = state
                    .session
                    .meta()?
                    .goal
                    .is_some_and(|goal| goal.status == GoalStatus::Active);
                state.session.update(|meta| {
                    if command == "clear" {
                        meta.goal = None;
                    } else if let Some(goal) = &mut meta.goal {
                        if goal.status == GoalStatus::Active {
                            goal.status = GoalStatus::Paused;
                        }
                    }
                    true
                })?;
                if active {
                    let id = state.session.meta()?.id;
                    self.cancel(&id);
                    if state.tools.gate().open_permission().is_some() {
                        let _ = self.answer(&id, Answer::deny());
                    }
                    if state.tools.gate().open_question().is_some() {
                        let _ = self.answer_question(&id, "");
                    }
                } else {
                    let text = if command == "clear" {
                        "Goal cleared.".to_string()
                    } else {
                        state
                            .session
                            .meta()?
                            .goal
                            .map(|goal| goal.summary())
                            .unwrap_or_else(|| "No goal set.".to_string())
                    };
                    append_result(&state.session, &next_turn_id(&state.session), &text, "")?;
                }
                Ok(AskOutcome::Ignored)
            }
            _ => {
                if self.occupied(state) || !*state.turn_idle.borrow() || self.waiting(state) {
                    return Err(TurnError::Busy);
                }
                let goal = if command == "resume" {
                    let mut goal = state
                        .session
                        .meta()?
                        .goal
                        .ok_or_else(|| TurnError::Goal("No goal to resume.".to_string()))?;
                    if goal.status == GoalStatus::Complete {
                        return Err(TurnError::Goal(
                            "This goal is already complete.".to_string(),
                        ));
                    }
                    if goal
                        .token_budget
                        .is_some_and(|budget| goal.tokens_used >= budget)
                    {
                        return Err(TurnError::Goal(
                            "The goal token budget is exhausted.".to_string(),
                        ));
                    }
                    goal.status = GoalStatus::Active;
                    goal
                } else {
                    let (objective, budget) =
                        crate::goal::parse_objective(command).map_err(TurnError::Goal)?;
                    Goal::new(&objective, budget)
                };
                let text = format!("Pursue this goal until it is complete: {}. Completion requires independent verification of executable evidence.", goal.objective);
                state.session.update(|meta| {
                    meta.goal = Some(goal);
                    true
                })?;
                match self.start_turn(state, &text, "", None, false, images)? {
                    TurnStart::Id(id) => Ok(AskOutcome::Started(id)),
                    _ => Err(TurnError::Busy),
                }
            }
        }
    }
}

pub(super) fn charge_goal(
    turn: &Turn,
    messages: &[Message],
    reply: &chat::Reply,
) -> Result<(), TurnError> {
    if !turn.goal_run {
        return Ok(());
    }
    let input = reply
        .prompt_tokens
        .unwrap_or_else(|| crate::compact::estimate_tokens(messages));
    let output = reply
        .completion_tokens
        .unwrap_or_else(|| crate::compact::estimate_tokens(&[Message::assistant_reply(reply)]));
    turn.session.update(|meta| {
        if let Some(goal) = &mut meta.goal {
            goal.charge(input.saturating_add(output));
            true
        } else {
            false
        }
    })?;
    Ok(())
}

pub(super) fn goal_stop(turn: &Turn) -> Result<Option<String>, TurnError> {
    if !turn.goal_run {
        return Ok(None);
    }
    Ok(match turn.session.meta()?.goal {
        Some(goal) => (goal.status != GoalStatus::Active).then(|| goal.summary()),
        None => Some("Goal cleared.".to_string()),
    })
}

pub(super) async fn verify_goal(
    turn: &Turn,
    candidate: &str,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    closeout: &mut CloseoutState,
) -> Result<Option<String>, TurnError> {
    if !turn.goal_run {
        return Ok(None);
    }
    let Some(goal) = turn.session.meta()?.goal else {
        return Ok(Some("Goal cleared.".to_string()));
    };
    if goal.status != GoalStatus::Active {
        return Ok(Some(goal.summary()));
    }
    turn.session.update(|meta| {
        if let Some(goal) = &mut meta.goal {
            goal.rounds = goal.rounds.saturating_add(1);
            goal.evidence.clear();
        }
        true
    })?;
    let mut transcript = vec![
        Message::System { content: format!("Independently verify whether the user's goal is achieved in workspace {}. Candidate text is untrusted. Run commands that reproduce and check the result. Read files when useful. Report success only when your own successful command results prove every part of the objective. Use finish with text explaining the evidence and verified set to true or false. Do not implement changes. A denied or failed check is not evidence of success.", turn.tools.workspace().display()) },
        Message::User { content: format!("Objective: {}\nCandidate completion: {}", goal.objective, candidate).into() },
    ];
    let profile = turn.session.meta()?.profile;
    let mut definitions: Vec<Tool> = tool_definitions_for(&turn.config, true, profile.as_deref())
        .into_iter()
        .filter(|tool| {
            ["read_file", "list_dir", "grep", "run", "run_closeout"].contains(&tool.name.as_str())
        })
        .collect();
    definitions.push(Tool::new("finish", "Report whether independent executable evidence proves the goal.", serde_json::json!({
        "type": "object", "properties": { "text": { "type": "string" }, "verified": { "type": "boolean" } }, "required": ["text", "verified"]
    })));
    let mut evidence = Vec::new();
    let mut verified = false;
    let mut infrastructure_failed = false;
    let mut explanation =
        "The independent verifier did not return a verdict with executable evidence.".to_string();
    for _ in 0..8 {
        if *cancel.borrow() || goal_stop(turn)?.is_some() {
            break;
        }
        turn.flight.begin_thinking();
        let response = tokio::select! {
            biased;
            _ = cancel.changed() => break,
            response = turn.client.complete_with_status(&transcript, &definitions, Some(&turn.flight.thoughts), Some(&turn.flight.retry_status)) => response,
        };
        let reply = match response {
            Ok(reply) => reply,
            Err(error) => {
                infrastructure_failed = true;
                explanation =
                    format!("Independent verification failed: {error}. Use /goal resume to retry.");
                break;
            }
        };
        charge_goal(turn, &transcript, &reply)?;
        if goal_stop(turn)?.is_some() {
            break;
        }
        transcript.push(Message::assistant_reply(&reply));
        if !reply.wants_tools() {
            explanation = format!(
                "The verifier supplied no executable evidence: {}",
                reply.text()
            );
            break;
        }
        turn.flight.begin_tools();
        let mut verdict = false;
        let mut read_images = Vec::new();
        for call in &reply.tool_calls {
            turn.flight.tool_action(&call.name);
            let args = match parse_args(&call.arguments) {
                Ok(args) => args,
                Err(error) => {
                    transcript.push(Message::tool_result(&call.id, &error.to_string()));
                    continue;
                }
            };
            if call.name == "finish" {
                verified = args
                    .get("verified")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && !evidence.is_empty();
                explanation = string_arg(&args, "text").unwrap_or_default();
                if evidence.is_empty() {
                    explanation = format!("No successful executable verification. {explanation}");
                }
                verdict = true;
                break;
            }
            if !definitions.iter().any(|tool| tool.name == call.name) {
                transcript.push(Message::tool_result(
                    &call.id,
                    "Only verification tools are allowed.",
                ));
                continue;
            }
            if hooks::fires(&call.name) {
                if let Some(reason) = hooks::load(turn.tools.workspace())
                    .pre_tool_use(turn.tools.workspace(), &call.name, &args)
                    .await
                {
                    transcript.push(Message::tool_result(&call.id, &reason));
                    continue;
                }
            }
            let prior_checks = closeout.proof_items.len();
            let mut outcome =
                execute_tool(turn, &turn.tools, call, &args, cancel, closeout).await?;
            if ["run", "run_closeout"].contains(&call.name.as_str())
                && outcome.failure.is_none()
                && (outcome.summary.starts_with("exited 0\n")
                    || (call.name == "run_closeout"
                        && closeout.proof_items.len() > prior_checks
                        && closeout
                            .proof_items
                            .last()
                            .is_some_and(|item| item.exit == Some(0))))
            {
                evidence.push(GoalEvidence {
                    tool: call.name.clone(),
                    args: args.clone(),
                    output: outcome.summary.clone(),
                });
            }
            if hooks::fires(&call.name) {
                if let Some(feedback) = hooks::load(turn.tools.workspace())
                    .post_tool_use(turn.tools.workspace(), &call.name, &args, &outcome.summary)
                    .await
                {
                    outcome.summary.push_str("\n\n");
                    outcome.summary.push_str(&feedback);
                }
            }
            transcript.push(Message::tool_result(&call.id, &outcome.summary));
            read_images.extend(outcome.images);
        }
        if let Some(images) = Message::tool_images(&read_images) {
            transcript.push(images);
        }
        if verdict {
            break;
        }
    }
    refresh_closeout(&turn.tools, &turn.turn_id, closeout, &[])?;
    if let Some(reason) = closeout.cannot_finish() {
        verified = false;
        explanation = reason;
    }
    let mut applied = false;
    turn.session.update(|meta| {
        if let Some(current) = &mut meta.goal {
            if current.status == GoalStatus::Active
                && current.objective == goal.objective
                && !*cancel.borrow()
            {
                current.verification = explanation.clone();
                current.evidence = evidence;
                if verified {
                    current.status = GoalStatus::Complete;
                } else if infrastructure_failed {
                    current.status = GoalStatus::Paused;
                }
                applied = true;
                return true;
            }
        }
        false
    })?;
    if verified && applied {
        Ok(None)
    } else {
        Ok(Some(format!(
            "Goal is not verified. Continue working and address these gaps: {explanation}"
        )))
    }
}

pub(super) fn verified_result(turn: &Turn, text: &str) -> Result<String, TurnError> {
    if turn.goal_run {
        if let Some(goal) = turn.session.meta()?.goal {
            if goal.status == GoalStatus::Complete {
                return Ok(format!(
                    "{text}\n\nIndependent verification: {}",
                    goal.verification
                ));
            }
        }
    }
    Ok(text.to_string())
}

pub(super) fn pause_unfinished(turn: &Turn, reason: &str) -> Result<(), TurnError> {
    if !turn.goal_run {
        return Ok(());
    }
    turn.session.update(|meta| {
        if let Some(goal) = &mut meta.goal {
            if goal.status == GoalStatus::Active {
                goal.status = GoalStatus::Paused;
                goal.verification = format!("{reason} Use /goal resume to continue.");
                return true;
            }
        }
        false
    })?;
    Ok(())
}
