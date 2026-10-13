use super::*;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Target {
    pub server: Option<String>,
    pub id: String,
}

pub(super) struct Selection {
    pub targets: BTreeSet<Target>,
    pub focus: Option<String>,
}

pub(super) struct Confirm {
    targets: Vec<Target>,
    count: String,
    error: Option<String>,
    archived: bool,
    server_name: String,
}

pub(super) struct Job {
    task: tokio::task::JoinHandle<Vec<(Target, Result<(), String>)>>,
}

fn target(app: &App, id: &str) -> Target {
    if app.servers.is_some() {
        if let Ok((server, id)) = serde_json::from_str(id) {
            return Target { server, id };
        }
    }
    Target {
        server: app.server.clone(),
        id: id.into(),
    }
}

pub(super) fn marks(app: &App) -> Option<BTreeSet<String>> {
    app.session_selection.as_ref().map(|selection| {
        selection
            .targets
            .iter()
            .map(|target| {
                if app.servers.is_some() {
                    servers::key(&target.server, &target.id)
                } else {
                    target.id.clone()
                }
            })
            .collect()
    })
}

pub(super) fn open(app: &mut App) {
    release_command_surfaces(app);
    app.left_open = true;
    let ids = screen::visible_session_ids(&screen_model(app));
    let selected = servers::selected(app);
    let focus = if ids.contains(&selected) {
        Some(selected)
    } else {
        ids.first().cloned()
    };
    app.session_selection = Some(Selection {
        targets: BTreeSet::new(),
        focus,
    });
}

pub(super) fn key(app: &App, event: KeyEvent) -> Option<Option<Effect>> {
    if app.batch_confirm.is_some() {
        return Some(match event.code {
            KeyCode::Esc => Some(Effect::CloseOverlay),
            KeyCode::Enter => Some(Effect::Submit),
            KeyCode::Backspace => Some(Effect::Backspace),
            KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Effect::Exit)
            }
            KeyCode::Char(c) if c.is_ascii_digit() => Some(Effect::Type(c)),
            _ => None,
        });
    }
    if app.session_selection.is_some() {
        return Some(match event.code {
            KeyCode::Esc => Some(Effect::CloseOverlay),
            KeyCode::Up => Some(Effect::SelectPrev),
            KeyCode::Down => Some(Effect::SelectNext),
            KeyCode::Char(' ') | KeyCode::Enter => Some(Effect::ToggleSessionTarget),
            KeyCode::Delete | KeyCode::Char('d') => Some(Effect::DeleteSelectedSessions),
            KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Effect::Exit)
            }
            _ => None,
        });
    }
    if event.modifiers.contains(KeyModifiers::CONTROL)
        && event.code == KeyCode::Char('s')
        && screen_model(app).overlay.is_none()
    {
        return Some(Some(Effect::SelectSessions));
    }
    None
}

fn confirm(app: &mut App, targets: Vec<Target>, archived: bool) {
    if targets.is_empty() {
        app.notice = Some(
            if archived {
                "No archived sessions on this server."
            } else {
                "Select sessions before deleting."
            }
            .into(),
        );
        return;
    }
    app.batch_confirm = Some(Confirm {
        targets,
        count: String::new(),
        error: None,
        archived,
        server_name: app.server_name().to_string(),
    });
}

pub(super) fn handle(app: &mut App, client: &Client, effect: &Effect) -> bool {
    if app.batch_job.is_some() {
        return !matches!(effect, Effect::Exit | Effect::DismissNotice);
    }
    if app.batch_confirm.is_some() {
        match effect {
            Effect::CloseOverlay | Effect::Choose(1) => app.batch_confirm = None,
            Effect::Type(c) if c.is_ascii_digit() => {
                let confirm = app.batch_confirm.as_mut().unwrap();
                if confirm.count.len() < 10 {
                    confirm.count.push(*c);
                }
            }
            Effect::Backspace => {
                app.batch_confirm.as_mut().unwrap().count.pop();
            }
            Effect::Paste(text) if text.trim().chars().all(|c| c.is_ascii_digit()) => {
                app.batch_confirm.as_mut().unwrap().count = text.trim().chars().take(10).collect();
            }
            Effect::Submit | Effect::Choose(0) => start(app, client),
            Effect::Exit => return false,
            _ => {}
        }
        return true;
    }
    match effect {
        Effect::SelectSessions => {
            open(app);
            return true;
        }
        Effect::ClearArchivedSessions => {
            release_command_surfaces(app);
            let targets = app
                .sessions
                .iter()
                .filter(|row| row.archived)
                .map(|row| Target {
                    server: app.server.clone(),
                    id: row.id.clone(),
                })
                .collect();
            confirm(app, targets, true);
            return true;
        }
        _ => {}
    }
    if app.session_selection.is_none() {
        return false;
    }
    match effect {
        Effect::CloseOverlay => app.session_selection = None,
        Effect::SelectSession(id) => {
            if !servers::sessions(app).iter().any(|row| &row.id == id) {
                return true;
            }
            let target = target(app, id);
            let selection = app.session_selection.as_mut().unwrap();
            selection.focus = Some(id.clone());
            if !selection.targets.remove(&target) {
                selection.targets.insert(target);
            }
        }
        Effect::ToggleSessionTarget => {
            if let Some(id) = app.session_selection.as_ref().unwrap().focus.clone() {
                return handle(app, client, &Effect::SelectSession(id));
            }
        }
        Effect::SelectNext | Effect::SelectPrev => {
            let model = screen_model(app);
            let ids = screen::visible_session_ids(&model);
            let selection = app.session_selection.as_mut().unwrap();
            let current = selection
                .focus
                .as_ref()
                .and_then(|id| ids.iter().position(|row| row == id));
            let index = current.map_or(0, |index| {
                if matches!(effect, Effect::SelectNext) {
                    (index + 1).min(ids.len().saturating_sub(1))
                } else {
                    index.saturating_sub(1)
                }
            });
            selection.focus = ids.get(index).cloned();
            app.list_scroll =
                screen::list_scroll_for(&screen_model(app), app.area, app.list_scroll);
        }
        Effect::DeleteSelectedSessions => {
            let targets = app
                .session_selection
                .as_ref()
                .unwrap()
                .targets
                .iter()
                .cloned()
                .collect();
            confirm(app, targets, false);
        }
        Effect::Exit
        | Effect::ScrollList { .. }
        | Effect::SetListFilter(_)
        | Effect::CycleListFilter
        | Effect::ToggleHeader(_)
        | Effect::DismissNotice => return false,
        _ => {}
    }
    true
}

fn start(app: &mut App, client: &Client) {
    let confirm = app.batch_confirm.as_mut().unwrap();
    if confirm.count != confirm.targets.len().to_string() {
        confirm.error = Some(format!(
            "Type {} to confirm the exact session count.",
            confirm.targets.len()
        ));
        return;
    }
    let targets = confirm.targets.clone();
    let current = app.server.clone();
    let client = client.clone();
    let home = app.home.clone();
    app.batch_job = Some(Job {
        task: tokio::spawn(async move {
            let saved = crate::pairing::Connections::load(&home.join(".kyotoagent/config.toml"));
            let mut results = Vec::new();
            for target in targets {
                let connection = if target.server == current {
                    Ok(client.clone())
                } else if let Some(id) = &target.server {
                    saved.as_ref().map_err(Clone::clone).and_then(|saved| {
                        saved
                            .servers
                            .get(id)
                            .ok_or_else(|| "Server is no longer saved.".to_string())
                            .and_then(|uri| Client::at_url(uri))
                    })
                } else {
                    Ok(Client::at(
                        home.join(".kyotoagent").join(server::SOCKET_FILE),
                    ))
                };
                let result = async {
                    let connection = connection?;
                    let path = format!(
                        "/v1/sessions/{}?delete_workspace=false&confirm_dirty=false",
                        target.id
                    );
                    let (status, body) = tokio::time::timeout(
                        Duration::from_secs(15),
                        connection.request("DELETE", &path, None),
                    )
                    .await
                    .map_err(|_| "Deletion timed out; retry to check the result.".to_string())??;
                    if matches!(status, 204 | 404) {
                        Ok(())
                    } else {
                        Err(error_text(&body, status))
                    }
                }
                .await;
                results.push((target, result));
            }
            results
        }),
    });
}

pub(super) async fn advance(app: &mut App) {
    if !app
        .batch_job
        .as_ref()
        .is_some_and(|job| job.task.is_finished())
    {
        return;
    }
    let job = app.batch_job.take().unwrap();
    let results = match job.task.await {
        Ok(results) => results,
        Err(_) => {
            if let Some(confirm) = &mut app.batch_confirm {
                confirm.error = Some("Deletion stopped. Retry to check remaining sessions.".into());
            }
            return;
        }
    };
    let mut failed = Vec::new();
    let mut errors = Vec::new();
    let mut deleted = 0;
    for (target, result) in results {
        match result {
            Ok(()) => {
                deleted += 1;
                servers::remove_session(app, &target.server, &target.id);
                app.deleted_sessions.insert(target.clone());
                if let Some(selection) = &mut app.session_selection {
                    selection.targets.remove(&target);
                }
            }
            Err(error) => {
                errors.push(format!(
                    "{} / {}: {error}",
                    target.server.as_deref().unwrap_or("Local"),
                    target.id
                ));
                failed.push(target);
            }
        }
    }
    if !app.sessions.iter().any(|row| row.id == app.selected) {
        select_session(
            app,
            app.sessions
                .first()
                .map(|row| row.id.clone())
                .unwrap_or_default(),
        );
    }
    app.notice = Some(format!(
        "Deleted {deleted} sessions; {} failed. Worktrees preserved.",
        failed.len()
    ));
    if failed.is_empty() {
        app.batch_confirm = None;
        app.session_selection = None;
    } else if let Some(confirm) = &mut app.batch_confirm {
        confirm.targets = failed;
        confirm.count.clear();
        confirm.error = Some(format!(
            "Deleted {deleted}; {} failed. Retry remaining sessions or cancel.\n{}",
            confirm.targets.len(),
            errors.join("\n")
        ));
    }
}

pub(super) fn overlay(app: &App) -> Option<Overlay> {
    let confirm = app.batch_confirm.as_ref()?;
    let count = confirm.targets.len();
    let scope = if confirm.archived {
        format!(" archived sessions on {}", confirm.server_name)
    } else {
        " selected sessions".into()
    };
    if app.batch_job.is_some() {
        return Some(Overlay::Text {
            text: format!(
                "Deleting {count}{scope}…\nWorktrees and files are preserved. Please wait."
            ),
        });
    }
    Some(Overlay::Question {
        text: format!("Delete {count}{scope}?\nSession history will be deleted. Worktrees and files are preserved.\nType {count} to confirm, then Enter to delete. Esc cancels.\n{}", confirm.error.as_deref().unwrap_or("")),
        choices: vec![choice_row(&format!("Delete {count} sessions")), choice_row("Cancel")],
        prompt: confirm.count.clone(),
    })
}

pub(super) fn list_action(
    model: &ScreenModel,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<Effect> {
    let column = screen::list_footer_at(model, area, column, row)?;
    let count = model.session_targets.as_ref()?.len().to_string().len();
    if column >= count + 12 && column < count + 18 {
        Some(Effect::DeleteSelectedSessions)
    } else if column >= count + 21 {
        Some(Effect::CloseOverlay)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new(PathBuf::from("/work"), PathBuf::from("/tmp"), "same".into());
        app.sessions = crate::mock::idle().sessions;
        app.sessions.truncate(1);
        app.sessions[0].id = "same".into();
        app
    }

    fn client() -> Client {
        Client::at(PathBuf::from("/missing-batch-test.sock"))
    }

    #[tokio::test]
    async fn mouse_and_keyboard_expose_selection_delete_and_cancel() {
        let mut app = app();
        let client = client();
        let area = app.area;
        let model = screen_model(&app);
        let list = screen::split_of(&model, area).list;
        let footer = list.bottom() - 2;
        assert_eq!(
            released_click(&model, area, list.x + 1, footer),
            Some(Effect::SelectSessions)
        );
        assert_eq!(
            released_click(&model, area, list.x + 10, footer),
            Some(Effect::ClearArchivedSessions)
        );
        open(&mut app);
        let model = screen_model(&app);
        let row = (list.y + 1..footer)
            .find(|row| {
                matches!(
                    screen::list_at(&model, area, list.x + 2, *row),
                    Some(screen::ListHit::Session(_))
                )
            })
            .unwrap();
        let effect = released_click(&model, area, list.x + 2, row).unwrap();
        handle(&mut app, &client, &effect);
        assert_eq!(app.session_selection.as_ref().unwrap().targets.len(), 1);
        let model = screen_model(&app);
        assert_eq!(
            released_click(&model, area, list.x + 14, footer),
            Some(Effect::DeleteSelectedSessions)
        );
        assert_eq!(
            released_click(&model, area, list.x + 23, footer),
            Some(Effect::CloseOverlay)
        );
        assert_eq!(
            keystroke(&app, KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            Some(Effect::ToggleSessionTarget)
        );
        assert!(!esc_stops(&app));
    }

    #[tokio::test]
    async fn selection_is_server_qualified_and_confirmation_is_a_snapshot() {
        let mut app = app();
        app.server = Some("first".into());
        app.servers = Some(BTreeMap::from([
            (app.server.clone(), servers::Snapshot::default()),
            (Some("second".into()), servers::Snapshot::default()),
        ]));
        open(&mut app);
        let first = servers::key(&Some("first".into()), "same");
        let second = servers::key(&Some("second".into()), "same");
        handle(&mut app, &client(), &Effect::SelectSession(first));
        app.servers.as_mut().unwrap().insert(
            Some("second".into()),
            servers::Snapshot::from_sessions(app.sessions.clone()),
        );
        handle(&mut app, &client(), &Effect::SelectSession(second.clone()));
        assert_eq!(app.session_selection.as_ref().unwrap().targets.len(), 2);
        handle(&mut app, &client(), &Effect::DeleteSelectedSessions);
        app.session_selection.as_mut().unwrap().targets.clear();
        assert_eq!(app.batch_confirm.as_ref().unwrap().targets.len(), 2);
        handle(&mut app, &client(), &Effect::Submit);
        assert!(app.batch_job.is_none());
        assert!(app.batch_confirm.as_ref().unwrap().error.is_some());
        handle(&mut app, &client(), &Effect::CloseOverlay);
        assert!(app.batch_confirm.is_none());
        assert!(app.session_selection.is_some());
        handle(&mut app, &client(), &Effect::CloseOverlay);
        assert!(app.session_selection.is_none());
    }

    #[tokio::test]
    async fn clear_archived_targets_current_server_and_failed_deletes_are_retained() {
        let mut app = app();
        app.sessions[0].archived = true;
        app.server = Some("current".into());
        app.servers = Some(BTreeMap::from([(
            Some("other".into()),
            servers::Snapshot::default(),
        )]));
        handle(&mut app, &client(), &Effect::ClearArchivedSessions);
        let targets = app.batch_confirm.as_ref().unwrap().targets.clone();
        assert_eq!(
            targets,
            vec![Target {
                server: Some("current".into()),
                id: "same".into()
            }]
        );
        handle(&mut app, &client(), &Effect::Type('1'));
        handle(&mut app, &client(), &Effect::Submit);
        assert!(app.batch_job.is_some());
        handle(&mut app, &client(), &Effect::CloseOverlay);
        assert!(app.batch_confirm.is_some());
        tokio::time::timeout(Duration::from_secs(5), async {
            while app.batch_job.is_some() {
                tokio::task::yield_now().await;
                advance(&mut app).await;
            }
        })
        .await
        .unwrap();
        let confirm = app.batch_confirm.as_ref().unwrap();
        assert_eq!(confirm.targets, targets);
        assert_eq!(confirm.count, "");
        assert!(confirm.error.as_ref().unwrap().contains("1 failed"));
        assert_eq!(app.sessions.len(), 1);
    }

    #[tokio::test]
    async fn partial_success_removes_only_successful_targets_and_keeps_failures_selected() {
        let mut app = app();
        let mut second = app.sessions[0].clone();
        second.id = "second".into();
        app.sessions.push(second);
        open(&mut app);
        for id in ["same", "second"] {
            handle(&mut app, &client(), &Effect::SelectSession(id.into()));
        }
        handle(&mut app, &client(), &Effect::DeleteSelectedSessions);
        app.batch_job = Some(Job {
            task: tokio::spawn(async {
                vec![
                    (
                        Target {
                            server: None,
                            id: "same".into(),
                        },
                        Ok(()),
                    ),
                    (
                        Target {
                            server: None,
                            id: "second".into(),
                        },
                        Err("denied".into()),
                    ),
                ]
            }),
        });
        tokio::task::yield_now().await;
        advance(&mut app).await;
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].id, "second");
        assert_eq!(app.session_selection.as_ref().unwrap().targets.len(), 1);
        assert_eq!(app.batch_confirm.as_ref().unwrap().targets.len(), 1);
        assert!(app
            .notice
            .as_ref()
            .unwrap()
            .contains("Deleted 1 sessions; 1 failed"));
    }
}
