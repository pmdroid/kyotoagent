use super::*;

pub(super) async fn session_tool(
    session: &Session,
    call: &ToolCall,
    args: &Value,
    runner: &Arc<Runner>,
) -> Option<ToolOutcome> {
    let parent_id = session.meta().map(|meta| meta.id).unwrap_or_default();
    let summary = match call.name.as_str() {
        "check_task" => {
            let timeout_sec = match optional_u64(args, "timeout_sec") {
                Ok(timeout) => timeout,
                Err(error) => return Some(failed(error)),
            };
            let ids = match check_task_ids(args) {
                Ok(ids) => ids,
                Err(error) => return Some(failed(error)),
            };
            if ids.is_empty() {
                return Some(failed("check_task needs an id".into()));
            } else {
                let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
                runner.check_task(&parent_id, &refs, timeout_sec).await
            }
        }
        "kill_task" => {
            let id = string_arg(args, "id").unwrap_or_default();
            if id.is_empty() {
                return Some(failed("kill_task needs an id".into()));
            } else {
                runner.kill_task(&parent_id, &id).await
            }
        }
        "spawn_subagent" => runner.spawn_subagent(&parent_id, args).await,
        "archive_session" => {
            let id = string_arg(args, "id").unwrap_or_default();
            let target = if id.is_empty() {
                parent_id.as_str()
            } else {
                id.as_str()
            };
            runner.archive_session(&parent_id, target).await
        }
        _ => return None,
    };
    Some(plain(summary))
}

fn plain(summary: String) -> ToolOutcome {
    ToolOutcome {
        is_error: false,
        images: Vec::new(),
        summary,
        wrote: None,
        failure: None,
    }
}

fn failed(summary: String) -> ToolOutcome {
    ToolOutcome {
        is_error: true,
        ..plain(summary)
    }
}

/// Execute one tool call and return the tool result the model reads, plus what
/// the proof event needs to know about it.
///
/// A tool that fails — a file that is not there, a command that will not run —
/// is a tool result, not the end of the turn. The model reads the error and
/// decides what to do next. Only a failure of the loop itself (a task that did
/// not finish) propagates.
pub(super) async fn execute_tool(
    turn: &Turn,
    tools: &Tools,
    call: &ToolCall,
    args: &Value,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    closeout: &mut CloseoutState,
) -> Result<ToolOutcome, TurnError> {
    let session = &turn.session;
    let turn_id = turn.turn_id.as_str();
    let config = &turn.config;
    let profile = session.meta().ok().and_then(|meta| meta.profile);
    if !config.tool_allowed(profile.as_deref(), call.name.as_str())
        || (session.meta()?.closeout_reviewer && !reviewer_tool(&call.name))
    {
        return Ok(ToolOutcome {
            is_error: true,
            images: Vec::new(),
            summary: format!("unknown tool: {}", call.name),
            wrote: None,
            failure: None,
        });
    }
    if let Some(outcome) = session_tool(session, call, args, &turn.runner).await {
        return Ok(outcome);
    }
    let workspace = tools.workspace().to_path_buf();
    let outcome = match call.name.as_str() {
        "read_file" => {
            let path = string_arg(args, "path").unwrap_or_default();
            let offset = match optional_u64(args, "offset") {
                Ok(offset) => offset,
                Err(error) => {
                    return Ok(ToolOutcome {
                        is_error: true,
                        images: Vec::new(),
                        summary: error,
                        wrote: None,
                        failure: None,
                    })
                }
            };
            let line = match optional_u64(args, "line") {
                Ok(line) => line,
                Err(error) => {
                    return Ok(ToolOutcome {
                        is_error: true,
                        images: Vec::new(),
                        summary: error,
                        wrote: None,
                        failure: None,
                    })
                }
            };
            let limit = match optional_u64(args, "limit") {
                Ok(limit) => limit,
                Err(error) => {
                    return Ok(ToolOutcome {
                        is_error: true,
                        images: Vec::new(),
                        summary: error,
                        wrote: None,
                        failure: None,
                    })
                }
            };
            if offset.is_some() && line.is_some() {
                return Ok(failed("Supply only one of line or offset.".to_string()));
            }
            let options = match (
                optional_string(args, "pages"),
                optional_string(args, "format"),
            ) {
                (Ok(pages), Ok(format)) => crate::tools::ReadOptions {
                    offset: None,
                    line: line.or(offset),
                    limit,
                    pages,
                    format,
                },
                (Err(error), _) | (_, Err(error)) => return Ok(failed(error)),
            };
            let tools = tools.clone();
            let turn_id = turn_id.to_string();
            let result = tokio::task::spawn_blocking(move || {
                tools.read_file_with_options(&turn_id, &path, options)
            })
            .await?;
            match result {
                Ok(result) => ToolOutcome {
                    is_error: false,
                    images: result.images.clone(),
                    summary: result.summary(),
                    wrote: None,
                    failure: None,
                },
                Err(error) => ToolOutcome {
                    is_error: true,
                    images: Vec::new(),
                    summary: error.to_string(),
                    wrote: None,
                    failure: None,
                },
            }
        }
        "web_fetch" => {
            let url = string_arg(args, "url").unwrap_or_default();
            let max_bytes = args.get("max_bytes").and_then(Value::as_u64);
            let tools = tools.clone();
            let turn_id = turn_id.to_string();
            let result =
                tokio::task::spawn_blocking(move || tools.web_fetch(&turn_id, &url, max_bytes))
                    .await?;
            match result {
                Ok(fetched) => ToolOutcome {
                    is_error: false,
                    images: Vec::new(),
                    summary: fetched.summary(),
                    wrote: None,
                    failure: None,
                },
                Err(error) => ToolOutcome {
                    is_error: true,
                    images: Vec::new(),
                    summary: error.to_string(),
                    wrote: None,
                    failure: None,
                },
            }
        }
        "web_search" => {
            let query = string_arg(args, "query").unwrap_or_default();
            let num_results = match optional_u64(args, "num_results") {
                Ok(value) => value,
                Err(error) => {
                    return Ok(ToolOutcome {
                        is_error: true,
                        images: Vec::new(),
                        summary: error,
                        wrote: None,
                        failure: None,
                    })
                }
            };
            let tools = tools.clone();
            let turn_id = turn_id.to_string();
            let exa_env = config.exa_api_key_env.clone();
            let fire_env = config.firecrawl_api_key_env.clone();
            let result = tokio::task::spawn_blocking(move || {
                tools.web_search(&turn_id, &query, num_results, &exa_env, &fire_env)
            })
            .await?;
            match result {
                Ok(found) => ToolOutcome {
                    is_error: false,
                    images: Vec::new(),
                    summary: found.summary(),
                    wrote: None,
                    failure: None,
                },
                Err(error) => ToolOutcome {
                    is_error: true,
                    images: Vec::new(),
                    summary: error.to_string(),
                    wrote: None,
                    failure: None,
                },
            }
        }
        "list_dir" => {
            let path = string_arg(args, "path").unwrap_or_default();
            let tools = tools.clone();
            let turn_id = turn_id.to_string();
            let result =
                tokio::task::spawn_blocking(move || tools.list_dir(&turn_id, &path)).await?;
            match result {
                Ok(result) => ToolOutcome {
                    is_error: false,
                    images: Vec::new(),
                    summary: result.summary(),
                    wrote: None,
                    failure: None,
                },
                Err(error) => ToolOutcome {
                    is_error: true,
                    images: Vec::new(),
                    summary: error.to_string(),
                    wrote: None,
                    failure: None,
                },
            }
        }
        "grep" => {
            let pattern = string_arg(args, "pattern").unwrap_or_default();
            let path = string_arg(args, "path");
            let glob = string_arg(args, "glob");
            let head_limit = match optional_u64(args, "head_limit") {
                Ok(limit) => limit.map(|limit| limit as usize),
                Err(error) => {
                    return Ok(ToolOutcome {
                        is_error: true,
                        images: Vec::new(),
                        summary: error,
                        wrote: None,
                        failure: None,
                    })
                }
            };
            let tools = tools.clone();
            let result = tokio::task::spawn_blocking(move || {
                tools.grep(&pattern, path.as_deref(), glob.as_deref(), head_limit)
            })
            .await?;
            match result {
                Ok(found) => ToolOutcome {
                    is_error: false,
                    images: Vec::new(),
                    summary: found.summary(),
                    wrote: None,
                    failure: None,
                },
                Err(error) => ToolOutcome {
                    is_error: true,
                    images: Vec::new(),
                    summary: error.to_string(),
                    wrote: None,
                    failure: None,
                },
            }
        }
        "search_replace" => {
            let path = string_arg(args, "path").unwrap_or_default();
            let old_string = string_arg(args, "old_string").unwrap_or_default();
            let new_string = string_arg(args, "new_string").unwrap_or_default();
            let replace_all = match optional_bool(args, "replace_all") {
                Ok(replace_all) => replace_all,
                Err(error) => {
                    return Ok(ToolOutcome {
                        is_error: true,
                        images: Vec::new(),
                        summary: error,
                        wrote: None,
                        failure: None,
                    })
                }
            };
            let tools = tools.clone();
            let turn_id = turn_id.to_string();
            let result = tokio::task::spawn_blocking(move || {
                tools.search_replace(&turn_id, &path, &old_string, &new_string, replace_all)
            })
            .await?;
            match result {
                Ok(write) => {
                    let wrote = if !write.denied {
                        Some(relative_to_workspace(&workspace, &write.path))
                    } else {
                        None
                    };
                    ToolOutcome {
                        is_error: false,
                        images: Vec::new(),
                        summary: write.summary(),
                        wrote,
                        failure: None,
                    }
                }
                Err(error) => ToolOutcome {
                    is_error: true,
                    images: Vec::new(),
                    summary: error.to_string(),
                    wrote: None,
                    failure: None,
                },
            }
        }
        "write_file" => {
            let path = string_arg(args, "path").unwrap_or_default();
            let contents = string_arg(args, "contents").unwrap_or_default();
            let tools = tools.clone();
            let turn_id = turn_id.to_string();
            let result =
                tokio::task::spawn_blocking(move || tools.write_file(&turn_id, &path, &contents))
                    .await?;
            match result {
                Ok(write) => {
                    // A denied write created or replaced nothing.
                    let wrote = if !write.denied {
                        Some(relative_to_workspace(&workspace, &write.path))
                    } else {
                        None
                    };
                    ToolOutcome {
                        is_error: false,
                        images: Vec::new(),
                        summary: write.summary(),
                        wrote,
                        failure: None,
                    }
                }
                Err(error) => ToolOutcome {
                    is_error: true,
                    images: Vec::new(),
                    summary: error.to_string(),
                    wrote: None,
                    failure: None,
                },
            }
        }
        "run" => {
            let argv = argv_arg(args);
            if let Some(reason) = guard_pull_request(turn, turn_id, closeout, &argv, cancel).await?
            {
                return Ok(failed(reason));
            }
            if let Some(id) = closeout.pinned_run(&argv) {
                return Ok(ToolOutcome {
                    is_error: false,
                    images: Vec::new(),
                    summary: format!("Use run_closeout with id {id} to run this check."),
                    wrote: None,
                    failure: None,
                });
            }
            let timeout_sec = args.get("timeout_sec").and_then(Value::as_u64);
            let allowed = {
                let tools = tools.clone();
                let turn_id = turn_id.to_string();
                let argv = argv.clone();
                let result = tokio::task::spawn_blocking(move || {
                    tools.run_allowed(&turn_id, &argv, timeout_sec)
                })
                .await?;
                match result {
                    Ok(allowed) => allowed,
                    Err(error) => {
                        return Ok(ToolOutcome {
                            is_error: true,
                            images: Vec::new(),
                            summary: error.to_string(),
                            wrote: None,
                            failure: None,
                        })
                    }
                }
            };
            if !allowed {
                ToolOutcome {
                    is_error: false,
                    images: Vec::new(),
                    summary: RunOutput::denied(&argv).summary(),
                    wrote: None,
                    failure: None,
                }
            } else {
                if let Some(reason) =
                    guard_pull_request(turn, turn_id, closeout, &argv, cancel).await?
                {
                    return Ok(failed(reason));
                }
                match tools.execute_cancellable(&argv, timeout_sec, cancel).await {
                    Ok(output) => {
                        let summary = output.summary();
                        // A command that did not exit 0, or that timed out, is a
                        // failure the proof card names. A denied command never
                        // ran, so it is not a failure.
                        let failure = if output.exit != Some(0) || output.timed_out {
                            Some(ProofFailure {
                                argv: output.argv.clone(),
                                exit: output.exit.unwrap_or(-1),
                                tail: last_n_lines(&output.stdout, &output.stderr, 40),
                            })
                        } else {
                            None
                        };
                        ToolOutcome {
                            is_error: failure.is_some(),
                            images: Vec::new(),
                            summary,
                            wrote: None,
                            failure,
                        }
                    }
                    Err(error) => ToolOutcome {
                        is_error: true,
                        images: Vec::new(),
                        summary: error.to_string(),
                        wrote: None,
                        failure: None,
                    },
                }
            }
        }
        "start_task" => {
            let argv = argv_arg(args);
            if let Some(reason) = guard_pull_request(turn, turn_id, closeout, &argv, cancel).await?
            {
                return Ok(failed(reason));
            }
            let timeout_sec = args.get("timeout_sec").and_then(Value::as_u64);
            let allowed = {
                let tools = tools.clone();
                let turn_id = turn_id.to_string();
                let argv = argv.clone();
                let result = tokio::task::spawn_blocking(move || {
                    tools.start_task_allowed(&turn_id, &argv, timeout_sec)
                })
                .await?;
                match result {
                    Ok(allowed) => allowed,
                    Err(error) => {
                        return Ok(ToolOutcome {
                            is_error: true,
                            images: Vec::new(),
                            summary: error.to_string(),
                            wrote: None,
                            failure: None,
                        })
                    }
                }
            };
            let summary = if !allowed {
                format!("Not allowed, so {} did not start.", argv.join(" "))
            } else {
                if let Some(reason) =
                    guard_pull_request(turn, turn_id, closeout, &argv, cancel).await?
                {
                    return Ok(failed(reason));
                }
                match tools.tasks().start(turn_id, &argv, timeout_sec).await {
                    Ok(started) => serde_json::to_string(&started).unwrap_or_else(|_| {
                        format!("{{\"id\":\"{}\",\"state\":\"running\"}}", started.id)
                    }),
                    Err(error) => return Ok(failed(error.to_string())),
                }
            };
            ToolOutcome {
                is_error: false,
                images: Vec::new(),
                summary,
                wrote: None,
                failure: None,
            }
        }
        "schedule" => {
            let minutes = args.get("minutes").and_then(Value::as_i64);
            let note = string_arg(args, "note").unwrap_or_default();
            let summary = match minutes {
                Some(minutes) => match tools.schedules().create(turn_id, minutes, &note) {
                    Ok(id) => id,
                    Err(error) => return Ok(failed(error)),
                },
                None => {
                    return Ok(failed(format!(
                        "minutes must be from {} to {}",
                        crate::schedule::MIN_MINUTES,
                        crate::schedule::MAX_MINUTES
                    )))
                }
            };
            ToolOutcome {
                is_error: false,
                images: Vec::new(),
                summary,
                wrote: None,
                failure: None,
            }
        }
        "cancel_schedule" => {
            let id = string_arg(args, "id").unwrap_or_default();
            let summary = if id.is_empty() {
                return Ok(failed("cancel_schedule needs an id".into()));
            } else {
                match tools.schedules().cancel(turn_id, &id) {
                    Ok(id) => id,
                    Err(error) => return Ok(failed(error)),
                }
            };
            ToolOutcome {
                is_error: false,
                images: Vec::new(),
                summary,
                wrote: None,
                failure: None,
            }
        }
        "get_closeout" => {
            refresh_closeout(tools, turn_id, closeout, &[])?;
            if let Some(error) = sync_retry(turn, closeout, cancel).await {
                return Ok(failed(error));
            }
            plain(closeout.report().to_string())
        }
        "run_closeout" => {
            let id = string_arg(args, "id").unwrap_or_default();
            let model = string_arg(args, "model");
            let summary =
                run_closeout(turn, &id, model.as_deref(), turn_id, cancel, closeout).await?;
            ToolOutcome {
                is_error: false,
                images: Vec::new(),
                summary,
                wrote: None,
                failure: None,
            }
        }
        "ask" => {
            if session
                .meta()
                .ok()
                .is_some_and(|meta| meta.parent_id.is_some())
            {
                return Ok(ToolOutcome {
                    is_error: false,
                    images: Vec::new(),
                    summary:
                        "A subagent has no ask tool. Call finish with the decision you needed."
                            .to_string(),
                    wrote: None,
                    failure: None,
                });
            }
            let text = string_arg(args, "text").unwrap_or_default();
            let choices = choices_arg(args);
            let visual_tools = tools.clone();
            let visual_turn = turn_id.to_string();
            let visual_args = args.clone();
            let visuals = tokio::task::spawn_blocking(move || {
                crate::question::prepare(&visual_tools, &visual_turn, &visual_args)
            })
            .await?;
            let visuals = match visuals {
                Ok(visuals) => visuals,
                Err(error) => {
                    return Ok(ToolOutcome {
                        is_error: true,
                        images: Vec::new(),
                        summary: error,
                        wrote: None,
                        failure: None,
                    })
                }
            };
            let gate = tools.gate().clone();
            let turn_id = turn_id.to_string();
            let result = tokio::task::spawn_blocking(move || {
                gate.ask_visual_question(
                    &turn_id,
                    crate::events::QuestionBody {
                        text,
                        choices,
                        visuals,
                    },
                )
            })
            .await?;
            match result {
                Ok(answer) => ToolOutcome {
                    is_error: false,
                    images: Vec::new(),
                    summary: answer,
                    wrote: None,
                    failure: None,
                },
                Err(error) => ToolOutcome {
                    is_error: true,
                    images: Vec::new(),
                    summary: error.to_string(),
                    wrote: None,
                    failure: None,
                },
            }
        }
        "use_skill" => {
            let name = string_arg(args, "name").unwrap_or_default();
            let skill_args = string_arg(args, "args").unwrap_or_default();
            let skill_workspace = workspace.clone();
            let allowed = config.skill_allowance(profile.as_deref());
            let result = tokio::task::spawn_blocking(move || {
                skills::use_skill_allowed(&skill_workspace, &name, &skill_args, allowed.as_deref())
            })
            .await?;
            match result {
                Ok(body) => ToolOutcome {
                    is_error: false,
                    images: Vec::new(),
                    summary: body,
                    wrote: None,
                    failure: None,
                },
                Err(error) => ToolOutcome {
                    is_error: true,
                    images: Vec::new(),
                    summary: error.to_string(),
                    wrote: None,
                    failure: None,
                },
            }
        }
        "todo" => match parse_todo_items(args) {
            Ok(items) => {
                append_with_body(
                    session,
                    turn_id,
                    EventKind::Todos,
                    &TodosBody {
                        items: items.clone(),
                    },
                )?;
                ToolOutcome {
                    is_error: false,
                    images: Vec::new(),
                    summary: format!("{} items", items.len()),
                    wrote: None,
                    failure: None,
                }
            }
            Err(error) => ToolOutcome {
                is_error: true,
                images: Vec::new(),
                summary: error,
                wrote: None,
                failure: None,
            },
        },
        _ => ToolOutcome {
            is_error: true,
            images: Vec::new(),
            summary: format!("unknown tool: {}", call.name),
            wrote: None,
            failure: None,
        },
    };
    Ok(outcome)
}

/// The event id of the permission a session is still waiting on, if any.
///
/// A turn holds one open permission at a time, so the newest `permission` with
/// no `permission_answer` after it is the one on screen. A session that was
/// interrupted while waiting has this permission in its log and a `waiting`
/// status in its meta.
pub(super) fn open_permission_id(session: &Session) -> Option<String> {
    let events = session.events().ok()?;
    let mut open = None;
    for event in &events {
        match event.kind {
            EventKind::Permission => open = Some(event.id.clone()),
            EventKind::PermissionAnswer => open = None,
            _ => {}
        }
    }
    open
}

pub(super) fn open_question_id(session: &Session) -> Option<String> {
    let events = session.events().ok()?;
    let mut open = None;
    for event in &events {
        match event.kind {
            EventKind::Question => open = Some(event.id.clone()),
            EventKind::QuestionAnswer => open = None,
            _ => {}
        }
    }
    open
}

pub(super) fn settle_held_question(session: &Session, text: &str) -> Result<(), SessionError> {
    let events = session.events()?;
    let question = events
        .iter()
        .rev()
        .find(|event| event.kind == EventKind::Question);
    let turn_id = question
        .map(|event| event.turn_id.clone())
        .unwrap_or_else(|| next_turn_id(session));
    let question_id = question.map(|event| event.id.clone());
    let event = Event::new(
        &session.next_event_id()?,
        &now(),
        &turn_id,
        EventKind::QuestionAnswer,
    )
    .with_body(&QuestionAnswerBody {
        question_id,
        answer: text.to_string(),
    })
    .map_err(|source| SessionError::Json {
        path: session.events_path(),
        source,
    })?;
    session.append(&event)?;
    session.update(|meta| {
        meta.status = Status::Idle;
        meta.updated_at = now();
        true
    })
}

/// Settle a permission that outlived the server: the answer lands on a card
/// no turn is waiting on. The `permission_answer` event goes to the log beside
/// the permission it settles, and the session goes back to idle, because the
/// turn that asked is gone and a new ask starts a new turn.
pub(super) fn settle_held_answer(session: &Session, answer: &Answer) -> Result<(), SessionError> {
    let events = session.events()?;
    let turn_id = events
        .iter()
        .rev()
        .find(|event| {
            event.kind == EventKind::Permission
                && answer.permission_id.as_deref() == Some(event.id.as_str())
        })
        .or_else(|| {
            events
                .iter()
                .rev()
                .find(|event| event.kind == EventKind::Permission)
        })
        .map(|event| event.turn_id.clone())
        .unwrap_or_else(|| next_turn_id(session));
    let event = Event::new(
        &session.next_event_id()?,
        &now(),
        &turn_id,
        EventKind::PermissionAnswer,
    )
    .with_body(&PermissionAnswerBody {
        permission_id: answer.permission_id.clone(),
        decision: answer.decision,
    })
    .map_err(|source| SessionError::Json {
        path: session.events_path(),
        source,
    })?;
    session.append(&event)?;
    session.update(|meta| {
        meta.status = Status::Idle;
        meta.updated_at = now();
        true
    })
}

pub(super) fn maybe_title(turn: &Turn) {
    let Some(model) = turn.config.title_model() else {
        return;
    };
    let asks = turn
        .session
        .events()
        .map(|events| {
            events
                .iter()
                .filter(|event| event.kind == EventKind::UserAsk)
                .count()
        })
        .unwrap_or(0);
    if asks != 1 {
        return;
    }
    let session = turn.session.clone();
    let config = turn.config.clone();
    let root = turn.root.clone();
    let ask = crate::session::title_ask(&turn.text);
    let model = model.to_string();
    tokio::spawn(async move {
        let Ok(client) = ChatClient::in_root(&config, root.as_deref()) else {
            return;
        };
        let Ok(reply) = client.title(&model, &ask).await else {
            return;
        };
        let title = crate::session::tidy_title(reply.text());
        if title.is_empty() {
            return;
        }
        let _ = session.update(|meta| {
            if !crate::session::title_unset(&meta.title) {
                return false;
            }
            meta.title = Some(title);
            true
        });
    });
}

/// The next turn id for a session: one past the number of asks already in the
/// log, so ids stay `t1`, `t2`, and so on across a reload.
pub(super) fn next_turn_id(session: &Session) -> String {
    let asks = session
        .events()
        .map(|events| {
            events
                .iter()
                .filter(|event| event.kind == EventKind::UserAsk)
                .count()
        })
        .unwrap_or(0);
    format!("t{}", asks + 1)
}

/// Set the session's status, touching `updated_at` when it changes.
pub(super) fn set_status(session: &Session, status: Status) -> Result<(), SessionError> {
    session.update(|meta| {
        if meta.status == status {
            return false;
        }
        meta.status = status;
        meta.updated_at = now();
        true
    })
}

pub(super) fn parse_args(arguments: &str) -> Result<Value, TurnError> {
    Ok(serde_json::from_str(if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    })?)
}

const ARGUMENT_ECHO_LIMIT: usize = 2_000;

pub(super) fn bad_arguments_result(error: &TurnError, raw: &str) -> String {
    let mut text = error.to_string();
    if raw.is_empty() {
        return text;
    }
    text.push_str("\n\nYour original arguments:\n");
    text.push_str(cap_utf8(raw, ARGUMENT_ECHO_LIMIT));
    text.push_str("\n\nPlease fix the syntax and retry.");
    text
}

fn cap_utf8(raw: &str, max: usize) -> &str {
    if raw.len() <= max {
        return raw;
    }
    let mut end = max;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    &raw[..end]
}

/// The result text and note a `finish` call carries.
pub(super) fn finish_args(args: &Value) -> Result<(String, String, String), TurnError> {
    let text = string_arg(args, "text").unwrap_or_default();
    let note = string_arg(args, "note").unwrap_or_default();
    let proof = string_arg(args, "proof").unwrap_or_default();
    Ok((text, note, proof))
}

pub(super) fn optional_u64(args: &Value, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{key} must be a non-negative integer")),
    }
}

pub(super) fn optional_bool(args: &Value, key: &str) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{key} must be a boolean")),
    }
}

pub(super) fn check_task_ids(args: &Value) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    match args.get("id") {
        None | Some(Value::Null) => {}
        Some(Value::String(id)) => {
            if !id.is_empty() {
                ids.push(id.clone());
            }
        }
        Some(_) => return Err("id must be a string".to_string()),
    }
    match args.get("ids") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for item in items {
                match item.as_str() {
                    Some(id) if !id.is_empty() && !ids.iter().any(|have| have == id) => {
                        ids.push(id.to_string());
                    }
                    Some(_) => {}
                    None => return Err("ids must be an array of strings".to_string()),
                }
            }
        }
        Some(_) => return Err("ids must be an array of strings".to_string()),
    }
    Ok(ids)
}

pub(super) fn optional_string(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

pub(super) fn string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

pub(super) fn argv_arg(args: &Value) -> Vec<String> {
    args.get("argv")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn choices_arg(args: &Value) -> Vec<String> {
    args.get("choices")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Append one event with a body to the log. The body is serialized here, so a
/// body that does not fit its kind is a session error and not a surprise.
pub(super) fn append_with_body<B: serde::Serialize>(
    session: &Session,
    turn_id: &str,
    kind: EventKind,
    body: &B,
) -> Result<(), SessionError> {
    let event = Event::new(&session.next_event_id()?, &now(), turn_id, kind)
        .with_body(body)
        .map_err(|source| SessionError::Json {
            path: session.events_path(),
            source,
        })?;
    session.append(&event)
}

pub(super) fn append_model_message(
    session: &Session,
    turn_id: &str,
    reply: &Reply,
) -> Result<(), SessionError> {
    append_with_body(
        session,
        turn_id,
        EventKind::ModelMessage,
        &ModelMessageBody {
            text: reply.text().to_string(),
        },
    )
}

pub(super) fn append_tool_call(
    session: &Session,
    turn_id: &str,
    call: &ToolCall,
    args: &Value,
) -> Result<(), SessionError> {
    append_with_body(
        session,
        turn_id,
        EventKind::ToolCall,
        &ToolCallBody {
            tool: call.name.clone(),
            args: args.clone(),
        },
    )
}

pub(super) fn append_tool_result(
    session: &Session,
    turn_id: &str,
    call: &ToolCall,
    output: &str,
) -> Result<(), SessionError> {
    append_tool_output(session, turn_id, call, output, &[], false).map(|_| ())
}

pub(super) fn append_tool_output(
    session: &Session,
    turn_id: &str,
    call: &ToolCall,
    output: &str,
    images: &[crate::attachment::ImageAttachment],
    is_error: bool,
) -> Result<String, SessionError> {
    let output = if output.len() > crate::compact::DUMP_LIMIT {
        let directory = session.dir().join("tool-output");
        std::fs::create_dir_all(&directory).map_err(|source| SessionError::Io {
            path: directory.clone(),
            source,
        })?;
        let path = directory.join(format!("{}.txt", session.next_event_id()?));
        std::fs::write(&path, output).map_err(|source| SessionError::Io {
            path: path.clone(),
            source,
        })?;
        format!(
            "{output}\nFull output saved to {}. Read a narrow line range with read_file.",
            path.display()
        )
    } else {
        output.to_string()
    };
    append_with_body(
        session,
        turn_id,
        EventKind::ToolResult,
        &ToolResultBody {
            is_error,
            images: images.to_vec(),
            tool: call.name.clone(),
            output: output.clone(),
        },
    )?;
    Ok(crate::compact::cap_dump(&output))
}

pub(super) fn append_result(
    session: &Session,
    turn_id: &str,
    text: &str,
    note: &str,
) -> Result<(), SessionError> {
    append_with_body(
        session,
        turn_id,
        EventKind::Result,
        &ResultBody {
            text: text.to_string(),
            note: note.to_string(),
        },
    )
}

pub(super) fn append_proof(
    session: &Session,
    turn_id: &str,
    proof: &ProofBody,
) -> Result<(), SessionError> {
    if proof.files.is_empty()
        && proof.items.is_empty()
        && proof.failures.is_empty()
        && proof.wrote.is_empty()
        && (proof.text.is_empty() || session.meta()?.parent_id.is_none())
    {
        return Ok(());
    }
    append_with_body(session, turn_id, EventKind::Proof, proof)
}

pub(super) fn append_recovered_proof(
    session: &Session,
    turn_id: &str,
    note: &str,
) -> Result<(), SessionError> {
    let events = session.events()?;
    if events
        .iter()
        .any(|event| event.turn_id == turn_id && event.kind == EventKind::Proof)
    {
        return Ok(());
    }
    let mut proof = ProofBody {
        note: note.to_string(),
        ..ProofBody::default()
    };
    for event in events.iter().filter(|event| event.turn_id == turn_id) {
        match event.kind {
            EventKind::ToolResult => {
                if let Ok(body) = event.body_as::<ToolResultBody>() {
                    if matches!(body.tool.as_str(), "attach_proof" | "attach_artifact") {
                        if let Ok(file) =
                            serde_json::from_str::<crate::proof::ProofFile>(&body.output)
                        {
                            if !proof.files.iter().any(|seen| seen.id == file.id) {
                                proof.files.push(file);
                            }
                        }
                    }
                }
            }
            EventKind::Artifact => {
                if let Ok(artifact) = event.body_as::<crate::events::ArtifactBody>() {
                    if !proof.files.iter().any(|file| file.id == artifact.file.id) {
                        proof.files.push(artifact.file);
                    }
                }
            }
            EventKind::CloseoutRun => {
                if let Ok(run) = event.body_as::<crate::events::CloseoutRunBody>() {
                    let item = crate::events::ProofItem {
                        id: run.id,
                        kind: "command".into(),
                        outcome: if run.exit == 0 && !run.timed_out {
                            "passed"
                        } else {
                            "failed"
                        }
                        .into(),
                        argv: run.argv,
                        exit: Some(run.exit),
                        tail: run.tail,
                    };
                    if let Some(existing) = proof
                        .items
                        .iter_mut()
                        .find(|existing| existing.id == item.id)
                    {
                        *existing = item;
                    } else {
                        proof.items.push(item);
                    }
                }
            }
            _ => {}
        }
    }
    append_proof(session, turn_id, &proof)
}

/// The path of a written file relative to the workspace, for the proof card.
pub(super) fn relative_to_workspace(workspace: &Path, absolute: &str) -> String {
    let path = Path::new(absolute);
    path.strip_prefix(workspace)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| absolute.to_string())
}

/// The last `n` lines of a command's combined output, stdout then stderr.
pub(super) fn last_n_lines(stdout: &str, stderr: &str, n: usize) -> String {
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.extend(stderr.lines());
    lines
        .iter()
        .rev()
        .take(n)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// The stdout of a git command in the workspace, or an empty string when git is
/// missing or the directory is not a repository.
pub(super) fn git_output(workspace: &Path, args: &[&str]) -> String {
    match std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        _ => String::new(),
    }
}
