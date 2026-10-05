use super::*;

pub(super) struct Popup {
    version: Option<u64>,
    highlight: usize,
    checks: bool,
}

pub(super) async fn fetch(
    client: &Client,
    session: &str,
) -> Result<Vec<crate::proof::ProofVersion>, String> {
    let (status, body) = client
        .request("GET", &format!("/v1/sessions/{session}/artifacts"), None)
        .await?;
    if status != 200 {
        return Err(error_text(&body, status));
    }
    serde_json::from_str(&body).map_err(|error| error.to_string())
}

pub(super) fn reconcile(app: &mut App, result: Result<Vec<crate::proof::ProofVersion>, String>) {
    match result {
        Ok(versions) => {
            app.proof_versions = versions;
            app.proof_status = None;
            if app.proof_selected.is_some_and(|selected| {
                !app.proof_versions
                    .iter()
                    .any(|version| version.version == selected)
            }) {
                app.proof_selected = None;
            }
        }
        Err(error) => app.proof_status = Some(format!("Artifacts unavailable: {error}")),
    }
}

pub(super) fn open(app: &mut App) {
    release_command_surfaces(app);
    app.right_panes.insert(RightPane::Proof);
    show_column(app);
    app.proof_popup = Some(Popup {
        version: None,
        highlight: 0,
        checks: false,
    });
    app.overlay = true;
    if app.proof_versions.is_empty() && !app.selected.is_empty() {
        app.proof_status = Some("Fetching artifacts…".into());
    }
}

pub(super) fn open_checks(app: &mut App) {
    release_command_surfaces(app);
    app.right_panes.insert(RightPane::Closeout);
    show_column(app);
    app.proof_popup = Some(Popup {
        version: None,
        highlight: 0,
        checks: true,
    });
    app.overlay = true;
}

fn rows(app: &App) -> Vec<(String, Option<screen::ProofAction>)> {
    let Some(popup) = &app.proof_popup else {
        return Vec::new();
    };
    if popup.checks {
        return app
            .closeout
            .iter()
            .flat_map(|check| check.runs.iter().rev())
            .filter_map(|run| {
                run.transcript.as_ref().map(|file| {
                    (
                        format!(
                            "{} · attempt {} · {}",
                            run.id,
                            run.attempt,
                            if run.timed_out {
                                "timed out"
                            } else if run.exit == 0 {
                                "passed"
                            } else {
                                "failed"
                            }
                        ),
                        Some(screen::ProofAction::Transcript(file.id.clone())),
                    )
                })
            })
            .collect();
    }
    if let Some(version) = popup.version.and_then(|number| {
        app.proof_versions
            .iter()
            .find(|version| version.version == number)
    }) {
        let mut rows = vec![("All versions".into(), None)];
        if version.response.is_some() {
            rows.push((
                "Show response in chat".into(),
                Some(screen::ProofAction::Response(version.version)),
            ));
        }
        rows.extend(version.proof.files.iter().map(|file| {
            (
                format!("{} · {} · {} bytes", file.name, file.media_type, file.size),
                Some(screen::ProofAction::File(version.version, file.id.clone())),
            )
        }));
        rows.push((
            "File details".into(),
            Some(screen::ProofAction::Details(version.version)),
        ));
        rows
    } else {
        app.proof_versions
            .iter()
            .rev()
            .map(|version| {
                (
                    format!(
                        "v{} · {} · {}",
                        version.version, version.at, version.turn_id
                    ),
                    Some(screen::ProofAction::Version(version.version)),
                )
            })
            .collect()
    }
}

fn window_start(app: &App) -> usize {
    let size = usize::from(app.area.height.saturating_sub(8) / 3).max(1);
    app.proof_popup
        .as_ref()
        .map(|popup| popup.highlight.saturating_sub(size - 1))
        .unwrap_or(0)
}

pub(super) fn choose_visible(app: &mut App, client: &Client, index: usize) {
    choose(app, client, window_start(app) + index);
}

pub(super) fn overlay(app: &App) -> Option<Overlay> {
    let popup = app.proof_popup.as_ref()?;
    let rows = rows(app);
    Some(Overlay::Question {
        text: if popup.checks {
            if rows.is_empty() {
                "No retained check transcripts".into()
            } else {
                "Closeout transcripts · ↑/↓ select · Enter open · Esc close".into()
            }
        } else if rows.is_empty() {
            app.proof_status
                .clone()
                .unwrap_or_else(|| "No artifacts or checks yet".into())
        } else {
            "Artifacts history · ↑/↓ select · Enter open · Esc close".into()
        },
        choices: rows
            .into_iter()
            .enumerate()
            .skip(window_start(app))
            .take(usize::from(app.area.height.saturating_sub(8) / 3).max(1))
            .map(|(index, (label, _))| Choice {
                label,
                marked: index == popup.highlight,
            })
            .collect(),
        prompt: String::new(),
    })
}

pub(super) fn key(app: &App, event: KeyEvent) -> Option<Effect> {
    match event.code {
        KeyCode::Esc => Some(Effect::CloseOverlay),
        KeyCode::Up | KeyCode::BackTab => Some(Effect::ProofMove(true)),
        KeyCode::Down | KeyCode::Tab => Some(Effect::ProofMove(false)),
        KeyCode::Enter => Some(Effect::ProofChoose(app.proof_popup.as_ref()?.highlight)),
        KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => Some(Effect::Exit),
        _ => None,
    }
}

pub(super) fn move_selection(app: &mut App, up: bool) {
    let len = rows(app).len();
    if let Some(popup) = &mut app.proof_popup {
        popup.highlight = if up {
            popup.highlight.saturating_sub(1)
        } else {
            popup.highlight.saturating_add(1).min(len.saturating_sub(1))
        };
    }
}

pub(super) fn choose(app: &mut App, client: &Client, index: usize) {
    let Some((_, action)) = rows(app).get(index).cloned() else {
        return;
    };
    if let Some(action) = action {
        activate(app, client, action);
    } else if let Some(popup) = &mut app.proof_popup {
        popup.version = None;
        popup.highlight = 0;
    }
}

pub(super) fn activate(app: &mut App, client: &Client, action: screen::ProofAction) {
    match action {
        screen::ProofAction::Transcript(id) => {
            let file = app
                .closeout
                .iter()
                .flat_map(|check| &check.runs)
                .filter_map(|run| run.transcript.as_ref())
                .find(|file| file.id == id)
                .cloned();
            if let Some(file) = file {
                proof_files::open(app, client, file);
            }
        }
        screen::ProofAction::Version(number) => {
            if !app
                .proof_versions
                .iter()
                .any(|version| version.version == number)
            {
                return;
            }
            app.proof_selected = if app
                .proof_versions
                .last()
                .is_some_and(|version| version.version == number)
            {
                None
            } else {
                Some(number)
            };
            app.proof_scroll = 0;
            if let Some(popup) = &mut app.proof_popup {
                popup.version = Some(number);
                popup.highlight = 0;
            }
        }
        screen::ProofAction::Response(number) => {
            let Some(response) = app
                .proof_versions
                .iter()
                .find(|version| version.version == number)
                .and_then(|version| version.response.as_ref())
            else {
                return;
            };
            let Some(index) = app
                .card_event_ids
                .iter()
                .position(|id| id == &response.event_id)
            else {
                app.notice = Some("Response is unavailable in chat".into());
                return;
            };
            app.proof_popup = None;
            app.overlay = false;
            app.scroll = screen::card_scroll(&screen_model(app), app.area, index);
            app.follow = false;
        }
        screen::ProofAction::Details(number) => {
            let Some(version) = app
                .proof_versions
                .iter()
                .find(|version| version.version == number)
            else {
                return;
            };
            let mut text = format!(
                "Artifacts v{} · {} · {}\n",
                version.version, version.at, version.turn_id
            );
            for file in &version.proof.files {
                text.push_str(&format!(
                    "\n{} · {} · {} bytes\nSHA-256: {}\n",
                    file.name, file.media_type, file.size, file.sha256
                ));
                if let Some(sha) = &file.git_sha {
                    text.push_str(&format!("Git SHA: {sha}\n"));
                }
            }
            app.open_text = Some(text);
            app.proof_popup = None;
            app.file_scroll = 0;
            app.overlay = true;
        }
        screen::ProofAction::File(number, id) => {
            let Some(file) = app
                .proof_versions
                .iter()
                .find(|version| version.version == number)
                .and_then(|version| version.proof.files.iter().find(|file| file.id == id))
            else {
                return;
            };
            proof_files::open(app, client, file.clone());
        }
    }
}

pub(super) fn focus(app: &mut App, id: Option<String>) {
    app.artifact_focus = id;
    let mut selected = None;
    for (index, card) in app.cards.iter_mut().enumerate() {
        if let Card::Artifact { file, focused, .. } = card {
            *focused = app.artifact_focus.as_ref() == Some(&file.id);
            if *focused {
                selected = Some(index);
            }
        }
    }
    if let Some(index) = selected {
        app.scroll = screen::card_scroll(&screen_model(app), app.area, index);
        app.follow = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keyboard_check_history_opens_retained_transcripts_without_artifacts() {
        let mut app = App::new("/w".into(), "/home/u".into(), "session".into());
        app.area = Rect::new(0, 0, 120, 30);
        app.ask = "Unsent draft".into();
        let file: crate::proof::ProofFile = serde_json::from_value(serde_json::json!({"id":"a".repeat(32), "name":"check.txt", "mediaType":"text/plain", "size":6, "sha256":"b".repeat(64)})).unwrap();
        app.closeout.push(screen::CloseoutCheck {
            id: "test".into(),
            kind: "command".into(),
            required: true,
            status: screen::CloseoutMark::Failed,
            attempt: Some(1),
            exit: Some(1),
            tail: "failed".into(),
            runs: vec![serde_json::from_value(serde_json::json!({"id":"test", "attempt":1, "exit":1, "tail":"failed", "transcript":file})).unwrap()],
        });
        open_checks(&mut app);
        let Some(Overlay::Question { text, choices, .. }) = overlay(&app) else {
            panic!("check history")
        };
        assert!(text.contains("Closeout transcripts"));
        assert!(choices[0].label.contains("failed"));
        choose(&mut app, &Client::at("/unused".into()), 0);
        assert!(app.proof_file_popup.is_some());
        assert!(app.proof_versions.is_empty());
        assert_eq!(app.ask, "Unsent draft");
    }

    fn version(number: u64) -> crate::proof::ProofVersion {
        crate::proof::ProofVersion {
            version: number,
            event_id: format!("proof-{number}"),
            turn_id: format!("turn-{number}"),
            at: "2026-10-03T12:00:00Z".into(),
            response: Some(crate::proof::ProofResponse {
                event_id: format!("result-{number}"),
                text: format!("Response {number}"),
            }),
            proof: crate::events::ProofBody {
                text: "Everything passed".into(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn older_selection_survives_refresh_and_arrival_does_not_open_the_pane() {
        let mut app = App::new("/w".into(), "/home/u".into(), "session".into());
        reconcile(&mut app, Ok(vec![version(1), version(2)]));
        assert!(!app.right_open);
        activate(
            &mut app,
            &Client::at("/unused".into()),
            screen::ProofAction::Version(1),
        );
        reconcile(&mut app, Ok(vec![version(1), version(2), version(3)]));
        assert_eq!(app.proof_selected, Some(1));
        assert!(app.right_panes.is_empty());
        activate(
            &mut app,
            &Client::at("/unused".into()),
            screen::ProofAction::Version(3),
        );
        assert_eq!(app.proof_selected, None);
        select_session(&mut app, "other".into());
        assert!(app.proof_versions.is_empty());
    }

    #[tokio::test]
    async fn keyboard_browsing_opens_the_correct_response_and_keeps_the_draft() {
        let mut app = App::new("/w".into(), "/home/u".into(), "session".into());
        app.area = Rect::new(0, 0, 120, 30);
        app.ask = "Unsent draft".into();
        app.cards = vec![Card::result("Response 1"), Card::result("Response 2")];
        app.card_event_ids = vec!["result-1".into(), "result-2".into()];
        reconcile(&mut app, Ok(vec![version(1), version(2)]));
        open(&mut app);
        let client = Client::at("/unused".into());
        for code in [KeyCode::Down, KeyCode::Enter, KeyCode::Down, KeyCode::Enter] {
            let effect = keystroke(&app, KeyEvent::new(code, KeyModifiers::NONE)).unwrap();
            apply(&mut app, &client, effect).await.unwrap();
        }
        assert!(app.open_text.is_none());
        assert!(!app.overlay);
        assert!(!app.follow);
        assert_eq!(app.ask, "Unsent draft");
        assert!(app.proof_popup.is_none());
    }

    #[tokio::test]
    async fn chat_artifacts_are_reachable_with_tab_enter_and_mouse_without_losing_drafts() {
        let mut app = App::new("/w".into(), "/home/u".into(), "session".into());
        app.area = Rect::new(0, 0, 120, 30);
        app.ask = "Unsent draft".into();
        let file = crate::proof::ProofFile {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "report.md".into(),
            media_type: "text/markdown".into(),
            size: 7,
            sha256: "a".repeat(64),
            git_sha: None,
        };
        app.cards.push(Card::Artifact {
            file: file.clone(),
            caption: None,
            focused: false,
        });
        let client = Client::at("/unused".into());
        for code in [KeyCode::Tab, KeyCode::Enter] {
            let effect = keystroke(&app, KeyEvent::new(code, KeyModifiers::NONE)).unwrap();
            apply(&mut app, &client, effect).await.unwrap();
        }
        assert_eq!(app.artifact_focus.as_deref(), Some(file.id.as_str()));
        assert!(app.proof_file_popup.is_some());
        assert_eq!(app.ask, "Unsent draft");
        close_overlay(&mut app);
        let model = screen_model(&app);
        let mut hit = false;
        for row in 0..app.area.height {
            for column in 0..app.area.width {
                if let Some(Effect::OpenArtifact(clicked)) = press_at(&model, app.area, column, row)
                {
                    assert_eq!(clicked, file);
                    hit = true;
                }
            }
        }
        assert!(hit);
        apply(&mut app, &client, Effect::Type('!')).await.unwrap();
        assert!(app.artifact_focus.is_none());
        assert_eq!(app.ask, "Unsent draft!");
    }

    #[test]
    fn legacy_proof_layout_loads_and_saves_as_artifacts() {
        let pane: RightPane = serde_json::from_str("\"proof\"").unwrap();
        assert_eq!(pane, RightPane::Proof);
        assert_eq!(serde_json::to_string(&pane).unwrap(), "\"artifacts\"");
    }

    #[test]
    fn every_version_is_reachable_in_a_short_terminal_and_errors_are_visible() {
        let mut app = App::new("/w".into(), "/home/u".into(), "session".into());
        app.area = Rect::new(0, 0, 80, 16);
        reconcile(&mut app, Ok((1..=20).map(version).collect()));
        open(&mut app);
        for _ in 0..19 {
            move_selection(&mut app, false);
        }
        let Some(Overlay::Question { choices, .. }) = overlay(&app) else {
            panic!("picker")
        };
        assert!(choices
            .iter()
            .any(|choice| choice.marked && choice.label.starts_with("v1 ·")));
        choose_visible(&mut app, &Client::at("/unused".into()), choices.len() - 1);
        assert_eq!(app.proof_selected, Some(1));
        reconcile(&mut app, Err("unauthorized".into()));
        assert!(app
            .proof_status
            .as_deref()
            .unwrap()
            .contains("unauthorized"));
        assert_eq!(app.proof_versions.len(), 20);
    }
}
