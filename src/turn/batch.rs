use super::*;
use futures_util::future::join_all;

pub(super) fn parallel(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_dir"
            | "grep"
            | "write_file"
            | "search_replace"
            | "run"
            | "web_fetch"
            | "web_search"
    )
}

pub(super) async fn execute(
    turn: &Turn,
    calls: &[&ToolCall],
    cancel: &tokio::sync::watch::Receiver<bool>,
    closeout: &CloseoutState,
) -> Result<Vec<ToolOutcome>, TurnError> {
    let workspace = turn.tools.workspace();
    let profile = turn.session.meta()?.profile;
    let mut prepared = Vec::new();
    let mut written_paths = HashSet::new();
    for call in calls {
        turn.flight.tool_action(&call.name);
        let args = parse_args(&call.arguments);
        append_tool_call(
            &turn.session,
            &turn.turn_id,
            call,
            &args
                .as_ref()
                .cloned()
                .unwrap_or_else(|_| serde_json::json!({})),
        )?;
        let outcome = if *cancel.borrow() || turn.tools.gate().rejected() {
            Err(("Skipped because the turn ended.".to_string(), false))
        } else if !turn.config.tool_allowed(profile.as_deref(), &call.name)
            || (turn.session.meta()?.closeout_reviewer && !reviewer_tool(&call.name))
        {
            Err((format!("unknown tool: {}", call.name), true))
        } else {
            match args {
                Err(error) => Err((bad_arguments_result(&error, &call.arguments), true)),
                Ok(args) => {
                    let numeric: &[&str] = match call.name.as_str() {
                        "read_file" => &["offset", "line", "limit"],
                        "web_search" => &["num_results"],
                        _ => &[],
                    };
                    let validation = numeric
                        .iter()
                        .try_for_each(|key| optional_u64(&args, key).map(|_| ()))
                        .and_then(|()| {
                            if call.name == "search_replace" {
                                optional_bool(&args, "replace_all").map(|_| ())
                            } else if call.name == "read_file" {
                                optional_string(&args, "pages")?;
                                optional_string(&args, "format").map(|_| ())
                            } else {
                                Ok(())
                            }
                        });
                    let blocked = if let Err(error) = validation {
                        Some(error)
                    } else if hooks::fires(&call.name) {
                        hooks::load(workspace)
                            .pre_tool_use(workspace, &call.name, &args)
                            .await
                    } else {
                        None
                    };
                    let pinned = (call.name == "run")
                        .then(|| closeout.pinned_run(&argv_arg(&args)))
                        .flatten();
                    if let Some(reason) = blocked {
                        Err((reason, true))
                    } else if let Some(id) = pinned {
                        Err((
                            format!("Use run_closeout with id {id} to run this check."),
                            false,
                        ))
                    } else {
                        let tools = turn.tools.clone();
                        let path = args
                            .get("path")
                            .and_then(Value::as_str)
                            .and_then(|path| tools.lock_path(path));
                        let defer = matches!(call.name.as_str(), "write_file" | "search_replace")
                            && path
                                .as_ref()
                                .is_some_and(|path| !written_paths.insert(path.clone()));
                        let turn_id = turn.turn_id.clone();
                        let name = call.name.clone();
                        let preparation = tokio::task::spawn_blocking(move || {
                            let prepared = if defer {
                                Ok(Some(tools))
                            } else {
                                tools.prepare(&turn_id, &name, &args)
                            };
                            (prepared, args)
                        })
                        .await?;
                        match preparation {
                            (Ok(Some(tools)), args) => Ok((tools, args)),
                            (Ok(None), _) => {
                                Err((format!("Not allowed, so {} did not run.", call.name), false))
                            }
                            (Err(error), _) => Err((error.to_string(), true)),
                        }
                    }
                }
            }
        };
        prepared.push(outcome);
    }
    let paths: Vec<_> = prepared
        .iter()
        .map(|prepared| {
            prepared.as_ref().ok().and_then(|(tools, args)| {
                args.get("path")
                    .and_then(Value::as_str)
                    .and_then(|path| tools.lock_path(path))
            })
        })
        .collect();
    let writes: HashSet<_> = calls
        .iter()
        .zip(&paths)
        .filter_map(|(call, path)| {
            matches!(call.name.as_str(), "write_file" | "search_replace")
                .then_some(path.as_ref())
                .flatten()
                .cloned()
        })
        .collect();
    let locks: HashMap<_, _> = writes
        .into_iter()
        .map(|path| (path, tokio::sync::Mutex::new(())))
        .collect();
    let results = join_all(calls.iter().zip(prepared).zip(paths).map(
        |((call, prepared), path)| {
            let lock = path.as_ref().and_then(|path| locks.get(path));
            let mut cancel = cancel.clone();
            let mut closeout = closeout.clone();
            async move {
                let (tools, args) = match prepared {
                    Ok(prepared) => prepared,
                    Err((summary, is_error)) => {
                        return Ok(ToolOutcome {
                            is_error,
                            images: Vec::new(),
                            summary,
                            wrote: None,
                            failure: None,
                        })
                    }
                };
                let _guard = match lock {
                    Some(lock) => Some(lock.lock().await),
                    None => None,
                };
                if *cancel.borrow() {
                    return Ok(ToolOutcome {
                        is_error: false,
                        images: Vec::new(),
                        summary: "Skipped because the turn ended.".into(),
                        wrote: None,
                        failure: None,
                    });
                }
                let mut outcome =
                    execute_tool(turn, &tools, call, &args, &mut cancel, &mut closeout).await?;
                if hooks::fires(&call.name) {
                    if let Some(feedback) = hooks::load(workspace)
                        .post_tool_use(workspace, &call.name, &args, &outcome.summary)
                        .await
                    {
                        if !outcome.summary.is_empty() && !feedback.is_empty() {
                            outcome.summary.push_str("\n\n");
                        }
                        outcome.summary.push_str(&feedback);
                    }
                }
                Ok(outcome)
            }
        },
    ))
    .await;
    results.into_iter().collect()
}
