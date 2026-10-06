use super::*;

pub fn key(event: KeyEvent, mode: Mode, overlay: bool) -> Option<Effect> {
    key_for(event, mode, overlay, false, false, true)
}

pub fn keystroke(app: &App, event: KeyEvent) -> Option<Effect> {
    if event.kind != KeyEventKind::Press {
        return None;
    }
    if app.delete_confirm.is_some() && event.code == KeyCode::Char(' ') {
        return Some(Effect::ToggleDeleteWorkspace);
    }
    if app.proof_file_popup.is_some() {
        return proof_files::key(app, event);
    }
    if app.proof_popup.is_some() {
        return proof::key(app, event);
    }
    if app.project_popup.is_some() {
        return projects::key(app, event);
    }
    if app.catalog_popup.is_some() {
        return catalog::key(event);
    }
    if app.provider_popup.is_some() {
        return providers::key(app, event);
    }
    if app.server_popup.is_some() || app.server_step.is_some() {
        return connections::key(app, event);
    }
    if !app.overlay {
        let files: Vec<_> = app
            .cards
            .iter()
            .filter_map(|card| match card {
                Card::Artifact { file, .. } => Some(file),
                _ => None,
            })
            .collect();
        if matches!(event.code, KeyCode::Tab | KeyCode::BackTab) && !files.is_empty() {
            let current = files
                .iter()
                .position(|file| app.artifact_focus.as_ref() == Some(&file.id));
            let next = match (current, event.code == KeyCode::BackTab) {
                (Some(index), true) => (index + files.len() - 1) % files.len(),
                (Some(index), false) => (index + 1) % files.len(),
                (None, true) => files.len() - 1,
                (None, false) => 0,
            };
            return Some(Effect::FocusArtifact(Some(files[next].id.clone())));
        }
        if let Some(file) = files
            .into_iter()
            .find(|file| app.artifact_focus.as_ref() == Some(&file.id))
        {
            match event.code {
                KeyCode::Enter => return Some(Effect::OpenArtifact(file.clone())),
                KeyCode::Esc => return Some(Effect::FocusArtifact(None)),
                _ => {}
            }
        }
    }
    if event.kind == KeyEventKind::Press
        && event.modifiers.contains(KeyModifiers::CONTROL)
        && event.code == KeyCode::Char('o')
    {
        return app
            .cards
            .iter()
            .rev()
            .find_map(|card| card.preview_text())
            .map(|text| Effect::OpenText(text.to_string()));
    }
    if app.queue_open {
        return match event.code {
            KeyCode::Esc => Some(Effect::CloseOverlay),
            KeyCode::Up => Some(Effect::ScrollUp),
            KeyCode::Down => Some(Effect::ScrollDown),
            KeyCode::Delete => Some(Effect::RemoveQueued),
            KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Effect::Exit)
            }
            _ => None,
        };
    }
    if app.open_image.is_some() {
        return match event.code {
            KeyCode::Esc => Some(Effect::CloseOverlay),
            KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Effect::Exit)
            }
            _ => None,
        };
    }
    if app.open_text.is_some() {
        return match event.code {
            KeyCode::Esc => Some(Effect::CloseOverlay),
            KeyCode::Up => Some(Effect::ScrollUp),
            KeyCode::Down => Some(Effect::ScrollDown),
            KeyCode::PageUp => Some(Effect::PageUp),
            KeyCode::PageDown => Some(Effect::PageDown),
            KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Effect::Exit)
            }
            _ => None,
        };
    }
    if ((event.code == KeyCode::Enter && event.modifiers.contains(KeyModifiers::SHIFT))
        || (event.code == KeyCode::Char('j') && event.modifiers.contains(KeyModifiers::CONTROL)))
        && screen_model(app).overlay.is_some()
    {
        return None;
    }
    let effect = key_for(
        event,
        mode(app),
        app.overlay,
        prompt_opens_help(app),
        surface_typing(app),
        app.ask.is_empty(),
    )?;
    if matches!(effect, Effect::ConfirmDelete) && screen_model(app).overlay.is_some() {
        return None;
    }
    Some(effect)
}

pub fn prompt_opens_help(app: &App) -> bool {
    if app.overlay || app.picker.is_some() || app.command_ui.is_some() || choosing(app) {
        return false;
    }
    matches!(mode(app), Mode::Idle | Mode::Working) && app.ask.is_empty()
}

pub fn surface_typing(app: &App) -> bool {
    app.ask.starts_with('/') || app.command_ui.is_some() || app.picker.is_some()
}

pub fn key_for(
    event: KeyEvent,
    mode: Mode,
    overlay: bool,
    prompt_empty: bool,
    composing: bool,
    ask_empty: bool,
) -> Option<Effect> {
    if event.kind != KeyEventKind::Press {
        return None;
    }
    if (event.code == KeyCode::Enter && event.modifiers.contains(KeyModifiers::SHIFT))
        || (event.code == KeyCode::Char('j') && event.modifiers.contains(KeyModifiers::CONTROL))
    {
        return if !overlay && matches!(mode, Mode::Idle | Mode::Working | Mode::QuestionText) {
            Some(Effect::Type('\n'))
        } else {
            None
        };
    }
    if event.code == KeyCode::Backspace && event.modifiers.contains(KeyModifiers::SUPER) {
        return Some(Effect::DeleteLine);
    }
    if (event.code == KeyCode::Backspace
        && event
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL))
        || (event.code == KeyCode::Char('h')
            && event
                .modifiers
                .contains(KeyModifiers::ALT | KeyModifiers::CONTROL))
    {
        return Some(Effect::DeleteWord);
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return match event.code {
            KeyCode::Char('u') | KeyCode::Char('U') => Some(Effect::DeleteLine),
            KeyCode::Char('c') | KeyCode::Char('C') => Some(Effect::Exit),
            KeyCode::Char('n') | KeyCode::Char('N') => Some(Effect::SelectNext),
            KeyCode::Char('p') | KeyCode::Char('P') => Some(Effect::SelectPrev),
            KeyCode::Char('t') | KeyCode::Char('T') => Some(Effect::NewSession),
            KeyCode::Char('x') | KeyCode::Char('X') => Some(Effect::Cancel),
            KeyCode::Char('y') | KeyCode::Char('Y') => Some(Effect::ToggleYolo),
            KeyCode::Char('m') | KeyCode::Char('M') => Some(Effect::OpenModel),
            KeyCode::Char('k') | KeyCode::Char('K') => Some(Effect::OpenPalette),
            KeyCode::Char('b') | KeyCode::Char('B') => Some(Effect::ToggleLeft),
            KeyCode::Char('f') | KeyCode::Char('F') => Some(Effect::CycleListFilter),
            KeyCode::Char('g') | KeyCode::Char('G') => Some(Effect::ToggleRight),
            KeyCode::Char('w') | KeyCode::Char('W') => {
                if overlay {
                    None
                } else {
                    Some(Effect::ConfirmDelete)
                }
            }
            _ => None,
        };
    }
    match event.code {
        KeyCode::Up => Some(Effect::ScrollUp),
        KeyCode::Down => Some(Effect::ScrollDown),
        KeyCode::Left => Some(Effect::CollapseHeader),
        KeyCode::Right => Some(Effect::ExpandHeader),
        KeyCode::PageUp => Some(Effect::PageUp),
        KeyCode::PageDown => Some(Effect::PageDown),
        KeyCode::Esc => Some(Effect::CloseOverlay),
        KeyCode::Enter => match mode {
            Mode::Idle | Mode::Working | Mode::Enhance { .. } => Some(Effect::Submit),
            Mode::QuestionText | Mode::Question { .. } => {
                if overlay {
                    Some(Effect::Submit)
                } else {
                    Some(Effect::OpenOverlay)
                }
            }
            Mode::Permission if composing => Some(Effect::Submit),
            Mode::Permission => {
                if overlay {
                    None
                } else {
                    Some(Effect::OpenOverlay)
                }
            }
        },
        KeyCode::Backspace => match mode {
            Mode::Idle
            | Mode::Working
            | Mode::QuestionText
            | Mode::Question { .. }
            | Mode::Enhance { .. } => Some(Effect::Backspace),
            Mode::Permission if composing => Some(Effect::Backspace),
            _ => None,
        },
        KeyCode::Char(c) => match mode {
            Mode::Permission if composing || c == '/' => {
                if c.is_control() {
                    None
                } else {
                    Some(Effect::Type(c))
                }
            }
            Mode::Enhance { .. } if composing || c == '/' => {
                if c.is_control() {
                    None
                } else {
                    Some(Effect::Type(c))
                }
            }
            Mode::Enhance { retry } if ask_empty => match c {
                'u' | 'U' => Some(Effect::EnhanceUse),
                'e' | 'E' => Some(Effect::EnhanceEdit),
                'x' | 'X' => Some(Effect::EnhanceDiscard),
                'r' | 'R' if retry => Some(Effect::EnhanceRetry),
                'a' | 'A' | 's' | 'S' | 'd' | 'D' => None,
                _ if c.is_control() => None,
                _ => Some(Effect::Type(c)),
            },
            Mode::Enhance { .. } => {
                if c.is_control() {
                    None
                } else {
                    Some(Effect::Type(c))
                }
            }
            Mode::Permission => match c {
                'a' | 'A' => Some(Effect::AllowOnce),
                's' | 'S' => Some(Effect::AllowSession),
                'd' | 'D' => Some(Effect::Deny),
                _ => None,
            },
            Mode::Question { choices } => {
                if let Some(digit) = c.to_digit(10) {
                    let index = digit as usize;
                    if (1..=choices).contains(&index) {
                        return Some(Effect::Choose(index - 1));
                    }
                }
                if c.is_control() {
                    None
                } else {
                    Some(Effect::Type(c))
                }
            }
            Mode::Idle | Mode::Working if c == '?' && prompt_empty && !overlay => {
                Some(Effect::OpenHelp)
            }
            Mode::Idle | Mode::Working | Mode::QuestionText => {
                if c.is_control() {
                    None
                } else {
                    Some(Effect::Type(c))
                }
            }
        },
        _ => None,
    }
}

pub fn mode(app: &App) -> Mode {
    if let Some(names) = &app.server_step {
        return Mode::Question {
            choices: names
                .len()
                .saturating_add(if names.is_empty() { 4 } else { 6 }),
        };
    }
    if let Some(step) = &app.profile_step {
        return Mode::Question {
            choices: step.names.len().saturating_add(1),
        };
    }
    match &app.workspace_step {
        WorkspaceStep::Projects(projects) => {
            return Mode::Question {
                choices: projects.len() + 1 + usize::from(app.server.is_none()),
            };
        }
        WorkspaceStep::AllProjects(projects) => {
            return Mode::Question {
                choices: projects.len() + 1 + usize::from(app.server.is_none()),
            }
        }
        WorkspaceStep::Worktree(_) => return Mode::Question { choices: 2 },
        WorkspaceStep::Off => {}
    }
    let Some(row) = app.selected_session() else {
        return Mode::Idle;
    };
    if row.archived {
        return Mode::Idle;
    }
    match row.status {
        Status::Idle => Mode::Idle,
        Status::Working => Mode::Working,
        Status::Waiting => match row.waiting {
            Some(Wait::Permission) => Mode::Permission,
            Some(Wait::Question) => question_mode(&app.cards, &app.question_text),
            Some(Wait::Enhance) => Mode::Enhance {
                retry: app
                    .cards
                    .iter()
                    .rev()
                    .any(|card| matches!(card, Card::Enhance { error: Some(_), .. })),
            },
            None => Mode::Idle,
        },
    }
}

pub(super) fn question_mode(cards: &[Card], typed: &str) -> Mode {
    if !typed.is_empty() {
        return Mode::QuestionText;
    }
    let open = cards.iter().rev().find_map(|card| match card {
        Card::Question { choices, .. } if card.is_waiting() => Some(choices.len()),
        _ => None,
    });
    match open {
        Some(n) if n > 0 => Mode::Question { choices: n },
        _ => Mode::QuestionText,
    }
}
