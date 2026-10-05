use super::*;

#[test]
fn pane_close_buttons_hide_only_the_clicked_pane_and_allow_reopening() {
    for pane in RightPane::ORDER {
        let mut app = todos_app(true);
        app.closeout = vec![closeout_check()];
        app.right_panes = RightPane::ORDER.into_iter().collect();
        app.area = Rect::new(0, 0, 160, 40);
        let model = screen_model(&app);
        let rect = screen::pane_rect(&model, app.area, pane).unwrap();
        let effect = mouse(click(rect.x + rect.width - 1, rect.y), &model, app.area);
        assert_eq!(effect, Some(Effect::TogglePane(pane)));
        let todos = app.todos.clone();
        assert!(apply_pane(&mut app, effect.unwrap()));
        assert!(!app.right_panes.contains(&pane));
        assert_eq!(app.right_panes.len(), RightPane::ORDER.len() - 1);
        assert_eq!(app.todos, todos);
        toggle_pane(&mut app, pane);
        assert!(app.right_panes.contains(&pane));
    }
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

#[test]
fn a_on_a_permission_is_allow_once() {
    assert_eq!(
        key(press(KeyCode::Char('a')), Mode::Permission, false),
        Some(Effect::AllowOnce)
    );
    assert_eq!(
        key(press(KeyCode::Char('s')), Mode::Permission, false),
        Some(Effect::AllowSession)
    );
    assert_eq!(
        key(press(KeyCode::Char('d')), Mode::Permission, false),
        Some(Effect::Deny)
    );
    assert_eq!(
        key_for(
            press(KeyCode::Char('/')),
            Mode::Permission,
            false,
            false,
            false,
            true,
        ),
        Some(Effect::Type('/'))
    );
    assert_eq!(
        key_for(
            press(KeyCode::Char('a')),
            Mode::Permission,
            false,
            false,
            true,
            true,
        ),
        Some(Effect::Type('a'))
    );
    assert_eq!(
        key_for(
            press(KeyCode::Enter),
            Mode::Permission,
            false,
            false,
            true,
            true
        ),
        Some(Effect::Submit)
    );
}

#[test]
fn a_while_idle_is_typed() {
    assert_eq!(
        key(press(KeyCode::Char('a')), Mode::Idle, false),
        Some(Effect::Type('a'))
    );
    assert_eq!(
        key(press(KeyCode::Char('s')), Mode::Idle, false),
        Some(Effect::Type('s'))
    );
}

#[test]
fn ctrl_k_opens_the_palette_and_ctrl_p_still_selects() {
    assert_eq!(key(ctrl('k'), Mode::Idle, false), Some(Effect::OpenPalette));
    assert_eq!(key(ctrl('p'), Mode::Idle, true), Some(Effect::SelectPrev));
    assert_eq!(
        key(ctrl('w'), Mode::Idle, false),
        Some(Effect::ConfirmDelete)
    );
    assert_eq!(key(ctrl('w'), Mode::Working, true), None);
    assert_eq!(key(ctrl('x'), Mode::Idle, true), Some(Effect::Cancel));
}

#[test]
fn question_mark_on_an_empty_prompt_opens_help() {
    let app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), String::new());
    assert!(prompt_opens_help(&app));
    assert_eq!(
        key_for(
            press(KeyCode::Char('?')),
            Mode::Idle,
            false,
            prompt_opens_help(&app),
            false,
            true,
        ),
        Some(Effect::OpenHelp)
    );
    let mut typing = app;
    typing.ask = "hi".into();
    assert!(!prompt_opens_help(&typing));
    assert_eq!(
        key_for(
            press(KeyCode::Char('?')),
            Mode::Idle,
            false,
            prompt_opens_help(&typing),
            false,
            false,
        ),
        Some(Effect::Type('?'))
    );
    assert_eq!(
        key_for(
            press(KeyCode::Char('?')),
            Mode::Working,
            false,
            false,
            false,
            true,
        ),
        Some(Effect::Type('?'))
    );
}

#[test]
fn mod_leaves_the_model_commands() {
    let skills = vec![SkillEntry {
        name: "preflight".into(),
        description: "Ship the closeout".into(),
        ..SkillEntry::default()
    }];
    let rows = command_catalog(&skills);
    let left: Vec<&str> = rows
        .iter()
        .filter(|row| command_matches(row, "mod"))
        .map(|row| row.line.name.as_str())
        .collect();
    assert_eq!(left, vec!["Open model", "/model"]);
    assert!(rows
        .iter()
        .any(|row| { row.line.name == "New session" && row.line.keys == "Ctrl-T" }));
    assert!(rows.iter().any(|row| row.line.name == "/compact"));
    assert!(rows.iter().all(|row| row.line.name != "preflight"));
}

#[test]
fn the_catalog_lists_commands_and_not_skills() {
    let skills = vec![
        SkillEntry {
            name: "preflight".into(),
            description: "Ship the closeout".into(),
            ..SkillEntry::default()
        },
        SkillEntry {
            name: "hidden".into(),
            description: "Model only".into(),
            user_invocable: false,
            ..SkillEntry::default()
        },
        SkillEntry {
            name: "deploy".into(),
            description: "User runs this".into(),
            disable_model_invocation: true,
            ..SkillEntry::default()
        },
    ];
    let rows = command_catalog(&skills);
    assert!(rows.iter().any(|row| row.line.name == "/todos"));
    assert!(rows.iter().all(|row| row.line.name != "preflight"));
    assert!(rows.iter().all(|row| row.line.name != "deploy"));
    assert!(rows.iter().all(|row| row.line.name != "hidden"));
    let names = |query: &str| -> Vec<String> {
        filtered_commands(&skills, query)
            .into_iter()
            .map(|row| row.line.name)
            .collect()
    };
    assert!(names("session").iter().any(|name| name == "New session"));
    assert_eq!(
        names("todos"),
        vec!["Todos".to_string(), "/todos".to_string()]
    );
    assert!(names("preflight").is_empty());
    assert!(names("SESSION").iter().any(|name| name == "New session"));
    assert!(names("/").iter().any(|name| name == "/todos"));
    assert!(names("/").iter().all(|name| name.starts_with('/')));
    assert_eq!(
        names("summarize"),
        vec!["Compact".to_string(), "/compact".to_string()]
    );
    assert_eq!(names("ctrl-t"), vec!["New session".to_string()]);
    assert_eq!(names("").len(), command_catalog(&skills).len());
}

#[test]
fn everything_sends_an_empty_profile_and_a_name_sends_it() {
    let names = vec!["review".to_string()];
    let everything = profile_choice(&names, 0).expect("a choice");
    let cleared = create_body("/work", false, everything.as_deref());
    assert_eq!(cleared["profile"], "");
    assert_eq!(cleared["workspace"], "/work");
    assert_eq!(cleared["worktree"], false);
    let named = profile_choice(&names, 1).expect("a choice");
    let body = create_body("/work", true, named.as_deref());
    assert_eq!(body["profile"], "review");
    assert_eq!(body["worktree"], true);
}

#[test]
fn the_profile_question_lists_everything_then_the_names() {
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), String::new());
    show_profile_prompt(
        &mut app,
        vec!["review".into(), "planning".into()],
        Some(PendingSession {
            workspace: PathBuf::from("/w"),
            worktree: false,
        }),
    );
    assert_eq!(mode(&app), Mode::Question { choices: 3 });
    match screen_model(&app).overlay {
        Some(Overlay::Question { text, choices, .. }) => {
            assert_eq!(text, PROFILE_QUESTION);
            assert_eq!(
                choices
                    .iter()
                    .map(|choice| choice.label.as_str())
                    .collect::<Vec<_>>(),
                vec![PROFILE_EVERYTHING, "review", "planning"]
            );
        }
        other => panic!("expected the profile question, got {other:?}"),
    }
}

#[test]
fn the_catalog_lists_profile_with_no_shortcut() {
    let rows = command_catalog(&[]);
    let profile = rows
        .iter()
        .find(|row| row.line.name == "Profile")
        .expect("Profile");
    assert_eq!(profile.line.keys, "");
    assert_eq!(
        rows.iter()
            .find(|row| row.line.name == "Effort")
            .map(|row| row.line.keys.as_str()),
        Some("")
    );
}

#[tokio::test]
async fn typing_tod_filters_the_open_palette() {
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), String::new());
    app.skills = vec![SkillEntry {
        name: "preflight".into(),
        description: "Ship the closeout".into(),
        ..SkillEntry::default()
    }];
    open_palette(&mut app);
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    for c in ['t', 'o', 'd'] {
        apply(&mut app, &client, Effect::Type(c)).await.unwrap();
    }
    match &app.command_ui {
        Some(CommandUi::Palette { query, .. }) => assert_eq!(query, "tod"),
        other => panic!("palette query, got {other:?}"),
    }
    assert!(app.ask.is_empty());
    let model = screen_model(&app);
    match &model.overlay {
        Some(Overlay::Palette { rows, query, .. }) => {
            let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
            assert_eq!(names, vec!["Todos", "/todos"]);
            assert_eq!(query, "tod");
        }
        other => panic!("overlay, got {other:?}"),
    }
    let _ = overlay_cells_of(&model, "\u{203a} tod");
}

#[tokio::test]
async fn the_palette_paints_its_last_row() {
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), String::new());
    app.area = Rect::new(0, 0, 76, 24);
    open_palette(&mut app);
    let last = command_catalog(&[]).len() - 1;
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    for _ in 0..last {
        apply(&mut app, &client, Effect::ScrollDown).await.unwrap();
    }
    match &app.command_ui {
        Some(CommandUi::Palette {
            highlight, scroll, ..
        }) => {
            assert_eq!(*highlight, last);
            assert!(*scroll > 0, "scroll {scroll}");
        }
        other => panic!("palette, got {other:?}"),
    }
    let model = screen_model(&app);
    let _ = overlay_cells_of(&model, "/closeout");
    let text = painted(&model);
    assert!(!text.contains("New session"), "{text}");
}

#[tokio::test]
async fn wheel_over_the_palette_moves_the_highlight() {
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), String::new());
    open_palette(&mut app);
    let model = screen_model(&app);
    let (column, row) = overlay_cells_of(&model, "New session");
    assert_eq!(
        mouse(
            wheel(MouseEventKind::ScrollDown, column, row),
            &model,
            app.area
        ),
        Some(Effect::ScrollDown)
    );
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    apply(&mut app, &client, Effect::ScrollDown).await.unwrap();
    assert!(matches!(
        app.command_ui,
        Some(CommandUi::Palette { highlight: 1, .. })
    ));
    open_help(&mut app);
    let help = screen_model(&app);
    let (column, row) = overlay_cells_of(&help, "New session");
    assert_eq!(
        mouse(
            wheel(MouseEventKind::ScrollDown, column, row),
            &help,
            app.area
        ),
        Some(Effect::ScrollDown)
    );
}

#[test]
fn help_scrolls_and_the_palette_moves() {
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), String::new());
    open_help(&mut app);
    assert!(scroll_command_ui(&mut app, false, 1));
    assert!(matches!(
        app.command_ui,
        Some(CommandUi::Help { scroll: 1 })
    ));
    open_palette(&mut app);
    assert!(scroll_command_ui(&mut app, false, 1));
    assert!(matches!(
        app.command_ui,
        Some(CommandUi::Palette { highlight: 1, .. })
    ));
    close_overlay(&mut app);
    assert!(app.command_ui.is_none());
}

#[test]
fn ctrl_b_toggles_the_session_list_and_ctrl_g_toggles_the_panes() {
    assert_eq!(key(ctrl('b'), Mode::Idle, false), Some(Effect::ToggleLeft));
    assert_eq!(
        key(ctrl('g'), Mode::Working, false),
        Some(Effect::ToggleRight)
    );
    assert_eq!(
        key(ctrl('b'), Mode::Permission, true),
        Some(Effect::ToggleLeft)
    );
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), "91bc".into());
    assert!(app.left_open);
    assert!(apply_pane(&mut app, Effect::ToggleLeft));
    assert!(!app.left_open);
    app.todos = crate::mock::todos().todos;
    assert!(apply_pane(&mut app, Effect::ToggleRight));
    assert!(app.right_open);
    assert!(app.right_panes.contains(&RightPane::Todos));
    assert!(apply_pane(&mut app, Effect::ToggleRight));
    assert!(!app.right_open);
    assert!(app.right_panes.contains(&RightPane::Todos));
}

#[test]
fn ctrl_n_and_ctrl_p_change_session() {
    assert_eq!(
        key(ctrl('n'), Mode::Working, false),
        Some(Effect::SelectNext)
    );
    assert_eq!(
        key(ctrl('p'), Mode::Permission, false),
        Some(Effect::SelectPrev)
    );
}

#[test]
fn ctrl_c_exits_and_posts_nothing() {
    assert_eq!(key(ctrl('c'), Mode::Permission, false), Some(Effect::Exit));
    assert_eq!(key(ctrl('c'), Mode::Idle, false), Some(Effect::Exit));
    assert_ne!(key(ctrl('c'), Mode::Idle, false), Some(Effect::Cancel));
    assert_ne!(key(ctrl('c'), Mode::Idle, false), Some(Effect::Submit));
    let mut release = ctrl('c');
    release.kind = KeyEventKind::Release;
    assert_eq!(key(release, Mode::Idle, false), None);
}

#[test]
fn a_broken_pipe_is_fatal_and_a_plain_io_error_is_not() {
    assert!(loop_error_is_fatal(
        &io::Error::from(io::ErrorKind::BrokenPipe).to_string()
    ));
    assert!(!loop_error_is_fatal(
        &io::Error::other("the view body was not json").to_string()
    ));
}

#[test]
fn a_panic_payload_is_the_notice_text() {
    let as_str: Box<dyn Any + Send> = Box::new("index out of bounds");
    assert_eq!(panic_line(as_str.as_ref()), "index out of bounds");
    let as_string: Box<dyn Any + Send> = Box::new(String::from("buffer"));
    assert_eq!(panic_line(as_string.as_ref()), "buffer");
}

#[test]
fn slash_compact_is_its_own_command() {
    assert!(matches!(
        slash_command("/compact"),
        Some(SlashCommand::Compact)
    ));
    assert!(slash_command("/compact now").is_none());
    assert!(matches!(
        slash_command("/yolo"),
        Some(SlashCommand::Yolo(None))
    ));
    assert!(matches!(
        slash_command("/yolo on"),
        Some(SlashCommand::Yolo(Some(true)))
    ));
    assert!(matches!(
        slash_command("/yolo off"),
        Some(SlashCommand::Yolo(Some(false)))
    ));
    assert!(slash_command("/yolo now").is_none());
    assert!(matches!(
        slash_command("/closeout"),
        Some(SlashCommand::TogglePane(RightPane::Closeout))
    ));
    assert!(matches!(
        slash_command("/closeout on"),
        Some(SlashCommand::ShowCloseout(true))
    ));
    assert!(matches!(
        slash_command("/closeout off"),
        Some(SlashCommand::ShowCloseout(false))
    ));
    assert!(slash_command("/closeout now").is_none());
    assert!(matches!(
        slash_command("/enhance"),
        Some(SlashCommand::Enhance(None))
    ));
    assert!(matches!(
        slash_command("/enhance on"),
        Some(SlashCommand::Enhance(Some(true)))
    ));
    assert!(matches!(
        slash_command("/enhance off"),
        Some(SlashCommand::Enhance(Some(false)))
    ));
    assert!(slash_command("/enhance now").is_none());
}

#[test]
fn enhance_is_a_palette_row_and_ctrl_y_stays_yolo() {
    let rows = command_catalog(&[]);
    let enhance = rows
        .iter()
        .find(|row| row.line.name == "Enhance")
        .expect("enhance");
    assert_eq!(enhance.line.keys, "");
    assert_eq!(enhance.line.hint, "rewrite a short prompt before the turn");
    assert_eq!(enhance.action, CommandAction::ToggleEnhance);
    let slash = rows
        .iter()
        .find(|row| row.line.name == "/enhance")
        .expect("/enhance");
    assert_eq!(slash.line.keys, "/enhance");
    assert_eq!(slash.action, CommandAction::ToggleEnhance);
    assert_eq!(
        key(ctrl('y'), Mode::Enhance { retry: true }, true),
        Some(Effect::ToggleYolo)
    );
}

#[test]
fn an_open_enhance_card_uses_its_keys_and_esc_discards() {
    assert_eq!(
        key(
            press(KeyCode::Char('u')),
            Mode::Enhance { retry: false },
            true
        ),
        Some(Effect::EnhanceUse)
    );
    assert_eq!(
        key(
            press(KeyCode::Char('e')),
            Mode::Enhance { retry: false },
            true
        ),
        Some(Effect::EnhanceEdit)
    );
    assert_eq!(
        key(
            press(KeyCode::Char('x')),
            Mode::Enhance { retry: false },
            true
        ),
        Some(Effect::EnhanceDiscard)
    );
    assert_eq!(
        key(
            press(KeyCode::Char('r')),
            Mode::Enhance { retry: false },
            true
        ),
        Some(Effect::Type('r'))
    );
    assert_eq!(
        key(
            press(KeyCode::Char('r')),
            Mode::Enhance { retry: true },
            true
        ),
        Some(Effect::EnhanceRetry)
    );
    assert_eq!(
        key(
            press(KeyCode::Char('a')),
            Mode::Enhance { retry: false },
            true
        ),
        None
    );
    assert_eq!(
        key_for(
            press(KeyCode::Char('u')),
            Mode::Enhance { retry: false },
            true,
            false,
            false,
            false
        ),
        Some(Effect::Type('u'))
    );
    assert_eq!(
        key(press(KeyCode::Enter), Mode::Enhance { retry: false }, true),
        Some(Effect::Submit)
    );
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Enhance { retry: false }, true),
        Some(Effect::CloseOverlay)
    );
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), "91bc".into());
    app.sessions = vec![blank_row()];
    app.sessions[0].id = "91bc".into();
    app.sessions[0].status = Status::Waiting;
    app.sessions[0].waiting = Some(Wait::Enhance);
    app.selected = "91bc".into();
    app.cards = vec![Card::Enhance {
        source: "ship it".into(),
        text: "Do the thing carefully.".into(),
        error: None,
        event_id: "e1".into(),
    }];
    app.overlay = true;
    assert!(!esc_stops(&app));
    assert!(enhance_is_esc_target(&app));
    assert!(matches!(mode(&app), Mode::Enhance { retry: false }));
}

#[test]
fn the_pane_commands_are_titled_rows_and_slash_rows() {
    let rows = command_catalog(&[]);
    let expect = [
        ("Todos", "", "show or hide the todos pane", RightPane::Todos),
        (
            "Tasks",
            "",
            "show or hide the background-command pane",
            RightPane::Tasks,
        ),
        (
            "Schedules",
            "",
            "show or hide the schedules pane",
            RightPane::Schedules,
        ),
        (
            "Closeout",
            "",
            "show or hide the closeout pane",
            RightPane::Closeout,
        ),
        (
            "/todos",
            "/todos",
            "show or hide the todos pane",
            RightPane::Todos,
        ),
        (
            "/tasks",
            "/tasks",
            "show or hide the background-command pane",
            RightPane::Tasks,
        ),
        (
            "/schedules",
            "/schedules",
            "show or hide the schedules pane",
            RightPane::Schedules,
        ),
        (
            "/closeout",
            "/closeout",
            "show or hide the closeout pane",
            RightPane::Closeout,
        ),
    ];
    for (name, keys, hint, pane) in expect {
        let row = rows
            .iter()
            .find(|row| row.line.name == name)
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(row.line.keys, keys);
        assert_eq!(row.line.hint, hint);
        assert_eq!(row.action, CommandAction::TogglePane(pane));
    }
    let sessions = rows
        .iter()
        .find(|row| row.line.name == "Sessions")
        .expect("sessions");
    assert_eq!(sessions.line.keys, "Ctrl-B");
    assert_eq!(sessions.action, CommandAction::ToggleLeft);
    let panes = rows
        .iter()
        .find(|row| row.line.name == "Panes")
        .expect("panes");
    assert_eq!(panes.line.keys, "Ctrl-G");
    let cancel = rows
        .iter()
        .find(|row| row.line.name == "Cancel")
        .expect("cancel");
    assert_eq!(cancel.line.keys, "Ctrl-X");
    let close = rows
        .iter()
        .find(|row| row.line.name == "Close popup")
        .expect("close popup");
    assert_eq!(close.line.keys, "Esc");
    assert_eq!(close.action, CommandAction::CloseOverlay);
    assert_eq!(panes.action, CommandAction::ToggleRight);
    let help = command_lines(&[]);
    for name in [
        "/todos",
        "/tasks",
        "/schedules",
        "/closeout",
        "Sessions",
        "Panes",
    ] {
        assert!(help.iter().any(|row| row.name == name), "{name}");
    }
}

#[test]
fn slash_pane_commands_take_no_extra_words() {
    assert_eq!(
        slash_command("/todos"),
        Some(SlashCommand::TogglePane(RightPane::Todos))
    );
    assert_eq!(
        slash_command("/tasks"),
        Some(SlashCommand::TogglePane(RightPane::Tasks))
    );
    assert_eq!(
        slash_command("/schedules"),
        Some(SlashCommand::TogglePane(RightPane::Schedules))
    );
    assert_eq!(
        slash_command("/closeout"),
        Some(SlashCommand::TogglePane(RightPane::Closeout))
    );
    assert_eq!(
        slash_command("/todos "),
        Some(SlashCommand::TogglePane(RightPane::Todos))
    );
    assert!(slash_command("/todos extra").is_none());
    assert!(slash_command("/tasks extra").is_none());
    assert!(slash_command("/schedules extra").is_none());
    assert!(slash_command("/closeout extra").is_none());
}

#[test]
fn a_slash_query_matches_pane_rows_the_way_yolo_matches() {
    let rows = command_catalog(&[]);
    let names = |query: &str| -> Vec<&str> {
        rows.iter()
            .filter(|row| command_matches(row, query))
            .map(|row| row.line.name.as_str())
            .collect()
    };
    assert_eq!(names("/yolo"), vec!["/yolo"]);
    assert_eq!(names("yolo"), vec!["Yolo", "/yolo"]);
    assert_eq!(names("/todos"), vec!["/todos"]);
    assert_eq!(names("todos"), vec!["Todos", "/todos"]);
    assert_eq!(names("tasks"), vec!["Tasks", "/tasks"]);
}

#[test]
fn pane_commands_toggle_one_pane_and_leave_the_others() {
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), "91bc".into());
    run_pane_command(&mut app, RightPane::Todos);
    assert!(app.ask.is_empty());
    assert!(app.right_panes.contains(&RightPane::Todos));
    run_pane_command(&mut app, RightPane::Tasks);
    assert!(app.right_panes.contains(&RightPane::Todos));
    assert!(app.right_panes.contains(&RightPane::Tasks));
    assert!(app.right_open);
    run_pane_command(&mut app, RightPane::Todos);
    assert!(!app.right_panes.contains(&RightPane::Todos));
    assert!(app.right_panes.contains(&RightPane::Tasks));
    run_pane_command(&mut app, RightPane::Schedules);
    run_pane_command(&mut app, RightPane::Closeout);
    assert!(app.right_panes.contains(&RightPane::Schedules));
    assert!(app.right_panes.contains(&RightPane::Closeout));
    assert!(app.right_panes.contains(&RightPane::Tasks));
    run_pane_command(&mut app, RightPane::Schedules);
    run_pane_command(&mut app, RightPane::Closeout);
    assert!(!app.right_panes.contains(&RightPane::Schedules));
    assert!(!app.right_panes.contains(&RightPane::Closeout));
    assert!(app.right_panes.contains(&RightPane::Tasks));
}

#[test]
fn working_types_and_enter_submits() {
    assert_eq!(
        key(press(KeyCode::Char('t')), Mode::Working, false),
        Some(Effect::Type('t'))
    );
    assert_eq!(
        key(press(KeyCode::Backspace), Mode::Working, false),
        Some(Effect::Backspace)
    );
    assert_eq!(
        key(press(KeyCode::Enter), Mode::Working, false),
        Some(Effect::Submit)
    );
}

#[test]
fn esc_on_a_question_overlay_closes_it_and_a_bare_esc_does_not_cancel() {
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), "91bc".into());
    app.sessions = vec![screen::SessionRow {
        id: "91bc".into(),
        workspace: PathBuf::from("/w"),
        title: None,
        status: screen::Status::Waiting,
        waiting: Some(screen::Wait::Question),
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
        archived: false,
    }];
    app.overlay = true;
    app.cards = vec![Card::question("Which?", &[("a", false), ("b", false)])];
    let cards = app.cards.clone();
    assert!(!esc_stops(&app));
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Question { choices: 2 }, true),
        Some(Effect::CloseOverlay)
    );
    close_overlay(&mut app);
    assert!(!app.overlay);
    assert_eq!(app.cards, cards);

    app.sessions[0].status = screen::Status::Working;
    app.sessions[0].waiting = None;
    app.cards.clear();
    app.thinking_open = true;
    app.overlay = true;
    assert!(esc_stops(&app));
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Working, true),
        Some(Effect::CloseOverlay)
    );
    close_overlay(&mut app);
    assert!(!app.thinking_open);
    assert!(!app.overlay);
    assert!(esc_stops(&app));
    assert_eq!(app.sessions[0].status, screen::Status::Working);
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Working, false),
        Some(Effect::CloseOverlay)
    );
    assert_eq!(key(ctrl('x'), Mode::Working, true), Some(Effect::Cancel));
}

#[test]
fn a_session_menu_offers_archive_and_an_archived_row_offers_restore() {
    assert_eq!(
        screen::session_menu_items(false),
        vec![
            screen::MENU_ARCHIVE.to_string(),
            screen::MENU_CLOSE.to_string()
        ]
    );
    assert_eq!(
        screen::session_menu_items(true),
        vec![
            screen::MENU_UNARCHIVE.to_string(),
            screen::MENU_CLOSE.to_string()
        ]
    );
    let rows = command_catalog(&[]);
    assert!(rows.iter().any(|row| row.line.name == "Archive session"));
    assert!(rows.iter().any(|row| row.line.name == "Unarchive session"));
}

#[test]
fn closing_a_middle_row_selects_the_one_below_it() {
    let sessions = [
        SessionRow {
            id: "a".into(),
            ..blank_row()
        },
        SessionRow {
            id: "b".into(),
            ..blank_row()
        },
        SessionRow {
            id: "c".into(),
            ..blank_row()
        },
    ];
    assert_eq!(next_list_id(&sessions, "a").as_deref(), Some("b"));
    assert_eq!(next_list_id(&sessions, "c").as_deref(), Some("b"));
    assert_eq!(next_list_id(&sessions[..1], "a"), None);
}

fn blank_row() -> SessionRow {
    SessionRow {
        id: String::new(),
        workspace: PathBuf::from("/w"),
        title: None,
        status: Status::Idle,
        waiting: None,
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
        archived: false,
    }
}

#[test]
fn ctrl_x_is_cancel() {
    assert_eq!(key(ctrl('x'), Mode::Working, false), Some(Effect::Cancel));
    assert_eq!(
        key(ctrl('x'), Mode::Permission, false),
        Some(Effect::Cancel)
    );
}

#[test]
fn number_keys_choose_a_question() {
    assert_eq!(
        key(
            press(KeyCode::Char('1')),
            Mode::Question { choices: 2 },
            false
        ),
        Some(Effect::Choose(0))
    );
    assert_eq!(
        key(
            press(KeyCode::Char('2')),
            Mode::Question { choices: 2 },
            false
        ),
        Some(Effect::Choose(1))
    );
    assert_eq!(
        key(
            press(KeyCode::Char('3')),
            Mode::Question { choices: 2 },
            false
        ),
        Some(Effect::Type('3'))
    );
    assert_eq!(
        key(
            press(KeyCode::Char('h')),
            Mode::Question { choices: 2 },
            false
        ),
        Some(Effect::Type('h'))
    );
    assert_eq!(
        key(press(KeyCode::Enter), Mode::Question { choices: 2 }, false),
        Some(Effect::OpenOverlay)
    );
    assert_eq!(
        key(press(KeyCode::Enter), Mode::Question { choices: 2 }, true),
        Some(Effect::Submit)
    );
    assert_eq!(
        key(
            press(KeyCode::Backspace),
            Mode::Question { choices: 2 },
            false
        ),
        Some(Effect::Backspace)
    );
}

fn waiting_question(text: &str) -> App {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions.push(SessionRow {
        id: "91bc7a1d".into(),
        workspace: PathBuf::from("/w"),
        title: None,
        status: Status::Waiting,
        waiting: Some(Wait::Question),
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
        archived: false,
    });
    app.cards.push(Card::question(
        "Which way?",
        &[("continue", false), ("stop", false)],
    ));
    app.question_text = text.to_string();
    app
}

#[test]
fn a_listed_question_is_a_prompt_and_digits_choose_while_it_is_empty() {
    let mut app = waiting_question("");
    assert_eq!(mode(&app), Mode::Question { choices: 2 });
    let model = screen_model(&app);
    assert_eq!(model.bottom, "");
    assert_eq!(model.bottom_kind, Bottom::Prompt);
    assert_eq!(
        key(press(KeyCode::Char('1')), mode(&app), false),
        Some(Effect::Choose(0))
    );
    assert_eq!(
        key(press(KeyCode::Char('2')), mode(&app), false),
        Some(Effect::Choose(1))
    );
    app.question_text.push_str("hello");
    assert_eq!(mode(&app), Mode::QuestionText);
    assert_eq!(screen_model(&app).bottom, "hello");
    assert_eq!(
        key(press(KeyCode::Char('1')), mode(&app), false),
        Some(Effect::Type('1'))
    );
    assert_eq!(
        key(press(KeyCode::Enter), mode(&app), false),
        Some(Effect::OpenOverlay)
    );
    assert_eq!(
        key(press(KeyCode::Enter), mode(&app), true),
        Some(Effect::Submit)
    );
}

#[test]
fn an_empty_choice_list_stays_a_prompt() {
    let mut app = waiting_question("");
    app.cards = vec![Card::question("What should the heading say?", &[])];
    assert_eq!(mode(&app), Mode::QuestionText);
    assert_eq!(
        key(press(KeyCode::Char('h')), mode(&app), false),
        Some(Effect::Type('h'))
    );
    assert_eq!(
        key(press(KeyCode::Enter), mode(&app), false),
        Some(Effect::OpenOverlay)
    );
    assert_eq!(
        key(press(KeyCode::Enter), mode(&app), true),
        Some(Effect::Submit)
    );
}

fn assert_question_overlay(app: &App, text: &str) {
    match screen_model(app).overlay {
        Some(Overlay::Question {
            text: overlay_text, ..
        }) => {
            assert!(
                overlay_text.contains(text),
                "{overlay_text} does not contain {text}"
            );
        }
        other => panic!("expected a question overlay, got {other:?}"),
    }
}

#[test]
fn poll_opens_a_waiting_question_overlay() {
    let mut app = waiting_question("");
    app.question_id = Some("e1".into());
    maybe_open_question(&mut app);
    assert!(app.overlay);
    assert_question_overlay(&app, "Which way?");
}

#[test]
fn esc_keeps_the_same_question_closed_on_poll() {
    let mut app = waiting_question("");
    app.question_id = Some("e1".into());
    maybe_open_question(&mut app);
    close_overlay(&mut app);
    assert!(!app.overlay);
    assert!(screen_model(&app).overlay.is_none());
    assert_eq!(app.dismissed_question.as_deref(), Some("e1"));
    maybe_open_question(&mut app);
    assert!(!app.overlay);
    assert!(screen_model(&app).overlay.is_none());
}

#[test]
fn click_opens_a_dismissed_question() {
    let mut app = waiting_question("");
    app.question_id = Some("e1".into());
    maybe_open_question(&mut app);
    close_overlay(&mut app);
    app.overlay = true;
    maybe_open_question(&mut app);
    assert!(app.overlay);
    assert_question_overlay(&app, "Which way?");
}

#[test]
fn a_new_question_id_opens_the_overlay() {
    let mut app = waiting_question("");
    app.question_id = Some("e1".into());
    maybe_open_question(&mut app);
    close_overlay(&mut app);
    app.question_id = Some("e2".into());
    maybe_open_question(&mut app);
    assert!(app.overlay);
    assert_question_overlay(&app, "Which way?");
}

#[test]
fn ctrl_t_opens_the_workspace_question() {
    let mut app = App::new(
        PathBuf::from("/tmp/work"),
        PathBuf::from("/tmp"),
        String::new(),
    );
    assert_eq!(key(ctrl('t'), Mode::Idle, false), Some(Effect::NewSession));
    open_workspace_prompt(&mut app);
    assert!(workspace_open(&app.workspace_step));
    assert!(app.overlay);
    assert_eq!(mode(&app), Mode::Question { choices: 2 });
    match screen_model(&app).overlay {
        Some(Overlay::Question { text, choices, .. }) => {
            assert_eq!(text, WORKSPACE_QUESTION);
            assert_eq!(choices[0].label, WORKSPACE_HERE);
            assert_eq!(choices[1].label, WORKSPACE_WORKTREE);
        }
        other => panic!("expected a workspace question, got {other:?}"),
    }
    assert_eq!(
        key(press(KeyCode::Char('1')), mode(&app), true),
        Some(Effect::Choose(0))
    );
    assert_eq!(
        key(press(KeyCode::Char('2')), mode(&app), true),
        Some(Effect::Choose(1))
    );
    close_overlay(&mut app);
    assert!(!workspace_open(&app.workspace_step));
    assert!(!app.overlay);
    assert!(screen_model(&app).overlay.is_none());
}

#[test]
fn a_project_pick_then_a_worktree_uses_that_folder() {
    let mut app = App::new(
        PathBuf::from("/tmp/work"),
        PathBuf::from("/tmp"),
        String::new(),
    );
    open_project_prompt(
        &mut app,
        vec![
            ListedProject {
                id: String::new(),
                name: "kyotoagent".into(),
                path: "/work/kyotoagent".into(),
            },
            ListedProject {
                id: String::new(),
                name: "acpbot".into(),
                path: "/work/acpbot".into(),
            },
        ],
    );
    match screen_model(&app).overlay {
        Some(Overlay::Question { text, choices, .. }) => {
            assert_eq!(text, WORKSPACE_QUESTION);
            assert_eq!(
                choices
                    .iter()
                    .map(|choice| choice.label.as_str())
                    .collect::<Vec<_>>(),
                vec![WORKSPACE_HERE, "kyotoagent", "acpbot", "Add project"]
            );
        }
        other => panic!("expected the project list, got {other:?}"),
    }
    assert_eq!(mode(&app), Mode::Question { choices: 4 });
    assert!(workspace_choice(&mut app, 1).is_none());
    match screen_model(&app).overlay {
        Some(Overlay::Question { choices, .. }) => {
            assert_eq!(choices[0].label, WORKSPACE_HERE);
            assert_eq!(choices[1].label, WORKSPACE_WORKTREE);
        }
        other => panic!("expected the worktree question, got {other:?}"),
    }
    let (workspace, worktree) = workspace_choice(&mut app, 1).expect("the worktree choice");
    assert_eq!(workspace, PathBuf::from("/work/kyotoagent"));
    assert!(worktree);
}

#[test]
fn worktree_choice_reads_the_second_line() {
    assert!(!worktree_choice(""));
    assert!(!worktree_choice("1"));
    assert!(!worktree_choice(WORKSPACE_HERE));
    assert!(worktree_choice("2"));
    assert!(worktree_choice(&format!("{WORKSPACE_WORKTREE}\n")));
}

#[test]
fn a_waiting_permission_does_not_auto_open() {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions.push(SessionRow {
        id: "91bc7a1d".into(),
        workspace: PathBuf::from("/w"),
        title: None,
        status: Status::Waiting,
        waiting: Some(Wait::Permission),
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
        archived: false,
    });
    app.cards
        .push(Card::permission("Replace notes.md", None, &["+hello"]));
    app.question_id = None;
    maybe_open_question(&mut app);
    assert!(!app.overlay);
    assert!(screen_model(&app).overlay.is_none());
    assert_eq!(mode(&app), Mode::Permission);
}

#[test]
fn a_waiting_row_is_permission_mode() {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions.push(SessionRow {
        id: "91bc7a1d".into(),
        workspace: PathBuf::from("/w"),
        title: None,
        status: Status::Waiting,
        waiting: Some(Wait::Permission),
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
        archived: false,
    });
    assert_eq!(mode(&app), Mode::Permission);
    app.notice = Some("Layout saved".into());
    let model = screen_model(&app);
    assert_eq!(model.bottom_kind, Bottom::Keys);
    assert_eq!(model.bottom, "a once   s session   d deny");
    assert_eq!(model.toast.as_deref(), Some("Layout saved"));

    app.sessions[0].status = Status::Idle;
    app.sessions[0].waiting = None;
    assert_eq!(mode(&app), Mode::Idle);
}

#[test]
fn ctrl_y_toggles_yolo() {
    assert_eq!(key(ctrl('y'), Mode::Idle, false), Some(Effect::ToggleYolo));
    assert_eq!(
        key(ctrl('y'), Mode::Permission, true),
        Some(Effect::ToggleYolo)
    );
}

#[test]
fn ctrl_m_opens_the_model_picker() {
    assert_eq!(key(ctrl('m'), Mode::Idle, false), Some(Effect::OpenModel));
    assert_eq!(
        key(ctrl('m'), Mode::Working, false),
        Some(Effect::OpenModel)
    );
}

#[test]
fn esc_on_the_model_picker_keeps_the_stored_pair() {
    let mut app = idle_with_skills();
    app.model = "grok-4.6".into();
    app.effort = Some("high".into());
    app.picker = Some(Picker::Model {
        rows: vec![ModelRow {
            id: "grok-4.5".into(),
            aliases: Vec::new(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            provider: None,
        }],
        highlight: 0,
        query: String::new(),
    });
    assert!(matches!(
        screen_model(&app).overlay,
        Some(Overlay::Model { .. })
    ));
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Idle, false),
        Some(Effect::CloseOverlay)
    );
    app.picker = None;
    assert_eq!(app.model, "grok-4.6");
    assert_eq!(app.effort.as_deref(), Some("high"));
    assert!(screen_model(&app).overlay.is_none());
}

fn picker_row(id: &str, aliases: &[&str]) -> ModelRow {
    ModelRow {
        id: id.into(),
        aliases: aliases.iter().map(|alias| (*alias).to_string()).collect(),
        reasoning_efforts: Vec::new(),
        context_length: None,
        provider: None,
    }
}

fn model_picker_app(highlight: usize) -> App {
    let mut app = idle_with_skills();
    app.model = "test/model".into();
    app.picker = Some(Picker::Model {
        rows: vec![
            picker_row("test/model", &[]),
            picker_row("grok-4.6", &["grok"]),
            picker_row("other", &["green"]),
        ],
        highlight,
        query: String::new(),
    });
    app
}

#[test]
fn typing_filters_the_model_picker_by_id_and_alias() {
    let mut app = model_picker_app(0);
    assert!(edit_open_model_query(&mut app, Some('g')));
    assert!(edit_open_model_query(&mut app, Some('R')));
    match screen_model(&app).overlay {
        Some(Overlay::Model { rows, highlight }) => {
            assert_eq!(rows, vec!["grok-4.6", "other"]);
            assert_eq!(highlight, 0);
            assert_eq!(rows[highlight], "grok-4.6");
        }
        other => panic!("expected a filtered model overlay, got {other:?}"),
    }
    assert_eq!(screen_model(&app).bottom, "gR");
    assert!(app.ask.is_empty());
}

#[test]
fn a_query_keeps_the_highlighted_row_when_it_still_matches() {
    let mut app = model_picker_app(1);
    assert!(edit_open_model_query(&mut app, Some('g')));
    match screen_model(&app).overlay {
        Some(Overlay::Model { rows, highlight }) => {
            assert_eq!(rows, vec!["grok-4.6", "other"]);
            assert_eq!(rows[highlight], "grok-4.6");
        }
        other => panic!("expected a filtered model overlay, got {other:?}"),
    }
}

#[test]
fn backspace_restores_every_model_row() {
    let mut app = model_picker_app(0);
    edit_open_model_query(&mut app, Some('g'));
    edit_open_model_query(&mut app, Some('r'));
    edit_open_model_query(&mut app, None);
    edit_open_model_query(&mut app, None);
    match screen_model(&app).overlay {
        Some(Overlay::Model { rows, .. }) => {
            assert_eq!(rows, vec!["test/model", "grok-4.6", "other"]);
        }
        other => panic!("expected the full catalog, got {other:?}"),
    }
    assert!(screen_model(&app).bottom.is_empty());
    assert!(app.ask.is_empty());
}

#[test]
fn an_empty_model_filter_draws_no_rows() {
    let mut app = model_picker_app(0);
    for c in ['z', 'z', 'z'] {
        edit_open_model_query(&mut app, Some(c));
    }
    match screen_model(&app).overlay {
        Some(Overlay::Model { rows, highlight }) => {
            assert!(rows.is_empty(), "{rows:?}");
            assert_eq!(highlight, 0);
        }
        other => panic!("expected an open empty model overlay, got {other:?}"),
    }
    assert_eq!(screen_model(&app).bottom, "zzz");
    assert!(app.ask.is_empty());
}

#[test]
fn a_mid_id_query_lists_grok_and_a_miss_stays_empty() {
    let mut app = model_picker_app(0);
    for c in ['4', '.', '6'] {
        assert!(edit_open_model_query(&mut app, Some(c)));
    }
    match screen_model(&app).overlay {
        Some(Overlay::Model { rows, highlight }) => {
            assert_eq!(rows, vec!["grok-4.6"]);
            assert_eq!(rows[highlight], "grok-4.6");
        }
        other => panic!("expected grok-4.6, got {other:?}"),
    }
    assert_eq!(screen_model(&app).bottom, "4.6");
    for _ in 0..3 {
        assert!(edit_open_model_query(&mut app, None));
    }
    for c in ['z', 'z', 'z'] {
        assert!(edit_open_model_query(&mut app, Some(c)));
    }
    match screen_model(&app).overlay {
        Some(Overlay::Model { rows, .. }) => {
            assert!(rows.is_empty(), "{rows:?}");
        }
        other => panic!("expected an empty model overlay, got {other:?}"),
    }
    assert_eq!(screen_model(&app).bottom, "zzz");
    assert!(app.ask.is_empty());
}

#[test]
fn a_model_query_ranks_prefix_then_substring_then_subsequence() {
    let mut app = idle_with_skills();
    app.picker = Some(Picker::Model {
        rows: vec![
            picker_row("xg4y", &[]),
            picker_row("grok-4.6", &["grok"]),
            picker_row("g4-fast", &[]),
        ],
        highlight: 0,
        query: String::new(),
    });
    for c in ['g', '4'] {
        assert!(edit_open_model_query(&mut app, Some(c)));
    }
    match screen_model(&app).overlay {
        Some(Overlay::Model { rows, highlight }) => {
            assert_eq!(rows, vec!["g4-fast", "xg4y", "grok-4.6"]);
            assert_eq!(rows[highlight], "xg4y");
        }
        other => panic!("expected a ranked model overlay, got {other:?}"),
    }
    assert!(app.ask.is_empty());
}

#[test]
fn an_alias_prefix_outranks_an_id_substring() {
    let mut app = idle_with_skills();
    app.picker = Some(Picker::Model {
        rows: vec![picker_row("xg4y", &[]), picker_row("zzzz", &["g4"])],
        highlight: 0,
        query: String::new(),
    });
    for c in ['g', '4'] {
        assert!(edit_open_model_query(&mut app, Some(c)));
    }
    match screen_model(&app).overlay {
        Some(Overlay::Model { rows, highlight }) => {
            assert_eq!(rows, vec!["zzzz", "xg4y"]);
            assert_eq!(rows[highlight], "xg4y");
        }
        other => panic!("expected the alias prefix first, got {other:?}"),
    }
}

#[test]
fn a_subsequence_of_an_alias_lists_that_model() {
    let mut app = model_picker_app(0);
    for c in ['g', 'r', 'n'] {
        assert!(edit_open_model_query(&mut app, Some(c)));
    }
    match screen_model(&app).overlay {
        Some(Overlay::Model { rows, highlight }) => {
            assert_eq!(rows, vec!["other"]);
            assert_eq!(rows[highlight], "other");
        }
        other => panic!("expected the green alias, got {other:?}"),
    }
    assert_eq!(screen_model(&app).bottom, "grn");
    assert!(app.ask.is_empty());
}

#[test]
fn enter_opens_a_waiting_overlay_and_esc_closes_it() {
    assert_eq!(
        key(press(KeyCode::Enter), Mode::Permission, false),
        Some(Effect::OpenOverlay)
    );
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Permission, true),
        Some(Effect::CloseOverlay)
    );
    assert_eq!(
        key(press(KeyCode::Char('d')), Mode::Permission, true),
        Some(Effect::Deny)
    );
    assert_eq!(
        key(press(KeyCode::Enter), Mode::Idle, false),
        Some(Effect::Submit)
    );
}

#[tokio::test]
async fn closing_a_permission_popup_leaves_it_unanswered() {
    let mut app = waiting_question("");
    app.sessions[0].status = Status::Waiting;
    app.sessions[0].waiting = Some(Wait::Permission);
    app.cards = vec![Card::permission("Write notes.md", None, &["+ hello"])];
    app.overlay = true;
    let cards = app.cards.clone();
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    apply(&mut app, &client, Effect::CloseOverlay)
        .await
        .expect("esc closes");
    assert!(!app.overlay);
    assert_eq!(app.cards, cards);
    assert!(matches!(cards[0], Card::Permission { decision: None, .. }));
    assert!(screen_model(&app).overlay.is_none());
    apply(&mut app, &client, Effect::OpenOverlay)
        .await
        .expect("the card opens again");
    match screen_model(&app).overlay {
        Some(Overlay::Permission { action, .. }) => {
            assert!(action.contains("notes.md"), "{action}");
        }
        other => panic!("expected the permission popup, got {other:?}"),
    }
}

#[test]
fn a_click_on_x_closes_a_question_and_a_dimmed_click_does_nothing() {
    let model = crate::mock::projects_new();
    let area = Rect::new(0, 0, 76, 24);
    let (x, y) = close_cell(&model);
    assert!(screen::overlay_close_at(&model, area, x, y));
    assert_eq!(mouse(click(x, y), &model, area), Some(Effect::CloseOverlay));
    assert_eq!(mouse(click(0, 0), &model, area), None);

    let mut menu = crate::mock::idle();
    menu.overlay = Some(Overlay::Menu {
        id: "91bc7a1d".into(),
        items: vec!["Delete session".into()],
        column: 4,
        row: 3,
    });
    assert_eq!(mouse(click(70, 10), &menu, area), None);
    assert!(!screen::overlay_close_at(&menu, area, 4, 3));
}

fn close_cell(model: &ScreenModel) -> (u16, u16) {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(76, 24);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| screen::render(model, frame.area(), frame))
        .expect("the popup draws");
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if buffer[(x, y)].symbol() == "x" {
                return (x, y);
            }
        }
    }
    panic!("missing x");
}

fn proof_view(items: serde_json::Value) -> view::Card {
    view::Card {
        id: "c4".into(),
        kind: CardKind::Proof,
        at: "2026-09-29T00:00:00.000Z".into(),
        body: serde_json::json!({
            "text": "cargo test passed.",
            "wrote": ["README.md"],
            "status": "M README.md",
            "diffStat": "README.md | 1 +",
            "items": items,
        }),
    }
}

fn idle_proof_app(card: Card) -> App {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions.push(SessionRow {
        id: "91bc7a1d".into(),
        workspace: PathBuf::from("/w"),
        title: None,
        status: Status::Idle,
        waiting: None,
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
        archived: false,
    });
    app.cards.push(card);
    app
}

#[test]
fn a_proof_projects_each_closeout_item_onto_the_card() {
    let screen = to_screen_card(&proof_view(serde_json::json!([
        {"id": "test", "kind": "command", "outcome": "passed"},
        {
            "id": "lint",
            "kind": "command",
            "outcome": "failed",
            "argv": ["cargo", "clippy"],
            "exit": 1,
            "tail": "error: unused"
        }
    ])))
    .expect("a proof card");
    match screen {
        Card::Proof { items, text } => {
            assert_eq!(text, "cargo test passed.");
            assert!(!text.contains("M README.md"));
            assert!(!text.contains("README.md | 1 +"));
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].id, "test");
            assert_eq!(items[0].kind, ItemKind::Command);
            assert_eq!(items[0].outcome, Outcome::Passed);
            assert_eq!(items[1].id, "lint");
            assert_eq!(items[1].kind, ItemKind::Command);
            assert_eq!(items[1].outcome, Outcome::Failed);
            assert_eq!(items[1].argv, vec!["cargo", "clippy"]);
            assert_eq!(items[1].exit, Some(1));
            assert_eq!(items[1].tail, "error: unused");
        }
        other => panic!("expected a proof card, got {other:?}"),
    }
}

#[test]
fn a_proof_with_items_opens_an_overlay_and_esc_closes_it() {
    let screen = to_screen_card(&proof_view(serde_json::json!([
        {"id": "test", "kind": "command", "outcome": "passed"},
        {
            "id": "lint",
            "kind": "command",
            "outcome": "failed",
            "argv": ["cargo", "clippy"],
            "exit": 1,
            "tail": "error: unused"
        }
    ])))
    .expect("a proof card");
    let mut app = idle_proof_app(screen);
    match overlay_from(&app.cards, "") {
        Some(Overlay::Proof { items, .. }) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].id, "test");
            assert_eq!(items[1].id, "lint");
            assert_eq!(items[1].argv, vec!["cargo", "clippy"]);
            assert_eq!(items[1].tail, "error: unused");
        }
        other => panic!("expected a proof overlay, got {other:?}"),
    }
    app.overlay = true;
    match screen_model(&app).overlay {
        Some(Overlay::Proof { items, .. }) => {
            assert_eq!(items[1].id, "lint");
            assert!(items[1].tail.contains("unused"));
        }
        other => panic!("expected a proof overlay, got {other:?}"),
    }
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Idle, true),
        Some(Effect::CloseOverlay)
    );
    app.overlay = false;
    assert!(screen_model(&app).overlay.is_none());
}

#[test]
fn a_proof_with_no_items_stays_on_the_pane() {
    let screen = to_screen_card(&proof_view(serde_json::json!([]))).expect("a proof card");
    match &screen {
        Card::Proof { items, text } => {
            assert!(items.is_empty());
            assert_eq!(text, "cargo test passed.");
        }
        other => panic!("expected a proof card, got {other:?}"),
    }
    let app = idle_proof_app(screen);
    match overlay_from(&app.cards, "") {
        Some(Overlay::Proof { text, items }) => {
            assert_eq!(text, "cargo test passed.");
            assert!(items.is_empty());
        }
        other => panic!("expected the proof sentence, got {other:?}"),
    }
}

#[test]
fn a_pull_url_opens_a_pull_overlay_and_esc_closes_it() {
    let mut app = idle_proof_app(Card::proof("cargo test passed.", &[]));
    let url = format!(
        "{}/pmdroid/kyotoagent/pull/14",
        crate::session::github_origin()
    );
    app.sessions[0].pull_url = Some(url.clone());
    app.overlay = true;
    app.overlay_pull = true;
    match screen_model(&app).overlay {
        Some(Overlay::Pull { url: shown }) => assert_eq!(shown, url),
        other => panic!("expected a pull overlay, got {other:?}"),
    }
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Idle, true),
        Some(Effect::CloseOverlay)
    );
    app.overlay = false;
    app.overlay_pull = false;
    assert!(screen_model(&app).overlay.is_none());
    let model = screen_model(&app);
    assert!(model.sessions[0].pull_mark().as_deref() == Some("pr 14"));
    app.sessions[0].pull_url = None;
    assert!(screen_model(&app).sessions[0].pull_mark().is_none());
}

fn idle_with_skills() -> App {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions.push(SessionRow {
        id: "91bc7a1d".into(),
        workspace: PathBuf::from("/w"),
        title: None,
        status: Status::Idle,
        waiting: None,
        pull_url: None,
        compacting: false,
        yolo: false,
        show_closeout: true,
        profile: None,
        enhance: false,
        model: String::new(),
        effort: None,
        project: None,
        project_name: None,
        parent_id: None,
        isolation: None,
        worktree: false,
        archived: false,
    });
    app.skills = vec![
        SkillEntry {
            name: "preflight".into(),
            description: "Ship checks".into(),
            ..SkillEntry::default()
        },
        SkillEntry {
            name: "preview".into(),
            description: "Preview a change".into(),
            ..SkillEntry::default()
        },
    ];
    app
}

#[test]
fn typing_pre_lists_preflight_and_preview() {
    let mut app = idle_with_skills();
    app.ask = "/pre".into();
    let picker = screen_model(&app).skill_picker.expect("the list opens");
    let names: Vec<&str> = picker.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, vec!["preflight", "preview"]);
    assert_eq!(picker.rows[0].description, "Ship checks");
    assert_eq!(picker.rows[1].description, "Preview a change");
}

#[test]
fn the_slash_picker_hides_a_skill_the_user_cannot_invoke() {
    let mut app = idle_with_skills();
    app.skills.push(SkillEntry {
        name: "precheck".into(),
        description: "Hidden from the user".into(),
        user_invocable: false,
        ..SkillEntry::default()
    });
    app.skills.push(SkillEntry {
        name: "deploy".into(),
        description: "User runs this".into(),
        disable_model_invocation: true,
        ..SkillEntry::default()
    });
    app.ask = "/pre".into();
    let picker = screen_model(&app).skill_picker.expect("the list opens");
    let names: Vec<&str> = picker.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, vec!["preflight", "preview"]);
    app.ask = "/de".into();
    let picker = screen_model(&app).skill_picker.expect("deploy is listed");
    let names: Vec<&str> = picker.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, vec!["deploy"]);
}

#[test]
fn enter_on_preflight_leaves_the_slash_name_in_the_prompt() {
    let mut app = idle_with_skills();
    app.ask = "/pre".into();
    assert!(fill_skill_picker(&mut app));
    assert_eq!(app.ask, "/preflight ");
}

#[test]
fn down_then_enter_fills_the_highlighted_name() {
    let mut app = idle_with_skills();
    app.ask = "/pre".into();
    app.skill_highlight = 1;
    assert!(fill_skill_picker(&mut app));
    assert_eq!(app.ask, "/preview ");
}

#[test]
fn esc_closes_the_list_and_keeps_the_typed_text() {
    let mut app = idle_with_skills();
    app.ask = "/pre".into();
    assert!(screen_model(&app).skill_picker.is_some());
    app.skill_picker_closed = true;
    assert_eq!(app.ask, "/pre");
    assert!(screen_model(&app).skill_picker.is_none());
}

#[test]
fn an_empty_idle_prompt_has_no_skill_list() {
    let app = idle_with_skills();
    assert!(app.ask.is_empty());
    assert!(screen_model(&app).skill_picker.is_none());
}

#[test]
fn enter_on_the_filled_name_submits_instead_of_filling_again() {
    let mut app = idle_with_skills();
    app.ask = "/preflight ".into();
    assert!(!fill_skill_picker(&mut app));
    assert_eq!(app.ask, "/preflight ");
}

fn click(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn drag_to(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn todos_app(right_open: bool) -> App {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.todos = crate::mock::todos().todos;
    app.right_open = right_open;
    app.right_width = TODOS_WIDTH;
    if right_open {
        app.right_panes.insert(RightPane::Todos);
    }
    app
}

#[test]
fn a_click_where_the_progress_row_was_does_not_open_todos() {
    let app = todos_app(false);
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let split = screen::split_of(&model, area);
    assert_eq!(split.todos.width, 0);
    assert_eq!(split.session.x + split.session.width, area.width);
    assert_eq!(split.input.y, split.session.y + split.session.height);
    let effect = mouse(
        click(split.session.x + 2, split.input.y.saturating_sub(1)),
        &model,
        area,
    );
    assert_ne!(effect, Some(Effect::TogglePane(RightPane::Todos)));
    assert!(!app.right_open);
}

#[test]
fn a_click_on_empty_space_in_the_todos_pane_does_nothing() {
    let app = todos_app(true);
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let inner = todos_inner(&model, area);
    assert_eq!(mouse(click(inner.x + 2, inner.y + 6), &model, area), None);
    assert_eq!(app.right_width, TODOS_WIDTH);
    assert!(app.right_open);
    assert!(app.open_todo.is_none());
}

fn tasks_app(rows: Vec<screen::TaskLine>) -> App {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.tasks = rows;
    app
}

fn one_task() -> screen::TaskLine {
    screen::TaskLine {
        id: "ab12cd34".into(),
        argv: "sleep 30".into(),
        state: "running".into(),
    }
}

fn task_line(id: &str) -> screen::TaskLine {
    screen::TaskLine {
        id: id.into(),
        argv: "sleep 30".into(),
        state: "running".into(),
    }
}

fn closeout_check() -> screen::CloseoutCheck {
    screen::CloseoutCheck {
        runs: Vec::new(),
        id: "cargo-test".into(),
        kind: "test".into(),
        required: true,
        status: screen::CloseoutMark::Running,
        exit: None,
        attempt: Some(1),
        tail: String::new(),
    }
}

fn fresh_app() -> App {
    App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), "91bc".into())
}

#[test]
fn arriving_items_preserve_the_selected_panes_and_column_visibility() {
    let mut app = fresh_app();
    app.right_panes.insert(RightPane::Schedules);
    let before = arrived_lists(&app);
    app.todos = crate::mock::todos().todos;
    app.closeout = vec![closeout_check()];
    app.tasks = vec![one_task()];
    assert!(open_arrived(&mut app, &before).is_some());
    assert!(!app.right_open);
    assert_eq!(app.right_panes, BTreeSet::from([RightPane::Schedules]));
    app.todos.clear();
    app.closeout.clear();
    app.tasks.clear();
    prune_panes(&mut app);
    assert_eq!(app.right_panes, BTreeSet::from([RightPane::Schedules]));
    let empty = arrived_lists(&app);
    app.closeout = vec![closeout_check()];
    assert!(open_arrived(&mut app, &empty).is_none());
    assert!(!app.right_panes.contains(&RightPane::Closeout));
    assert!(!app.right_open);
}

#[test]
fn a_new_task_loads_its_tail_without_reopening_a_hidden_pane() {
    let mut app = fresh_app();
    app.tasks = vec![task_line("old")];
    let before = arrived_lists(&app);
    assert!(open_arrived(&mut app, &before).is_none());
    app.tasks.push(task_line("newer"));
    app.tasks.push(task_line("newest"));
    assert_eq!(open_arrived(&mut app, &before).as_deref(), Some("newest"));
    assert!(app.right_panes.is_empty());
    assert!(!app.right_open);
}

#[test]
fn a_click_on_the_tasks_row_opens_the_list_and_esc_returns_to_the_pane() {
    let mut app = tasks_app(vec![one_task()]);
    let area = Rect::new(0, 0, 76, 24);
    let closed = screen_model(&app);
    let split = screen::split_of(&closed, area);
    assert_eq!(split.input.y, split.session.y + split.session.height);
    assert_ne!(
        mouse(
            click(split.session.x + 2, split.input.y.saturating_sub(1)),
            &closed,
            area
        ),
        Some(Effect::OpenTasks)
    );
    open_tasks_overlay(&mut app);
    let listed = screen_model(&app);
    assert!(listed.overlay.is_none());
    assert!(listed.right_panes.contains(&RightPane::Tasks));
    let drawn = draw_app(&app);
    assert!(drawn.contains("sleep 30"), "{drawn}");
    assert!(drawn.contains("tasks"), "{drawn}");
    let mut hit = None;
    for y in 0..area.height {
        for x in 0..area.width {
            if screen::task_line_at(&listed, area, x, y).as_deref() == Some("ab12cd34") {
                hit = Some((x, y));
                break;
            }
        }
    }
    let (x, y) = hit.expect("the task line is in the pane");
    assert_eq!(
        released_click(&listed, area, x, y),
        Some(Effect::OpenTask("ab12cd34".into()))
    );
    app.task_detail = Some(TaskDetail {
        id: "ab12cd34".into(),
        argv: "sleep 30".into(),
        state: "running".into(),
        tail: "still going".into(),
    });
    let detailed = screen_model(&app);
    assert!(detailed.overlay.is_none());
    assert_eq!(
        detailed.open_task.as_ref().map(|task| task.id.as_str()),
        Some("ab12cd34")
    );
    assert_eq!(
        detailed.open_task.as_ref().map(|task| task.tail.as_str()),
        Some("still going")
    );
    let tail = draw_app(&app);
    assert!(tail.contains("still going"), "{tail}");
    close_overlay(&mut app);
    assert!(screen_model(&app).right_panes.contains(&RightPane::Tasks));
    assert!(screen_model(&app).overlay.is_none());
}

#[test]
fn opening_tasks_leaves_the_todos_pane_in_the_same_column() {
    let mut app = todos_app(true);
    app.tasks = vec![one_task()];
    let area = Rect::new(0, 0, 76, 24);
    open_tasks_overlay(&mut app);
    let model = screen_model(&app);
    assert!(model.overlay.is_none());
    let rects = screen::right_pane_rects(&model, area);
    assert_eq!(
        rects.iter().map(|(pane, _)| *pane).collect::<Vec<_>>(),
        vec![RightPane::Todos, RightPane::Tasks]
    );
    assert!(rects[0].1.y < rects[1].1.y);
    assert_eq!(rects[0].1.x, rects[1].1.x);
    let drawn = draw_app(&app);
    assert!(drawn.contains(" todos "), "{drawn}");
    assert!(drawn.contains(" tasks "), "{drawn}");
    assert!(drawn.contains("sleep 30"), "{drawn}");
}

#[test]
fn opening_closeout_leaves_todos_and_tasks_open() {
    let mut app = todos_app(true);
    app.tasks = vec![one_task()];
    app.right_panes.insert(RightPane::Tasks);
    app.closeout = vec![screen::CloseoutCheck {
        runs: Vec::new(),
        id: "lint".into(),
        kind: "command".into(),
        required: true,
        status: screen::CloseoutMark::Failed,
        exit: Some(1),
        attempt: Some(1),
        tail: "fail".into(),
    }];
    toggle_pane(&mut app, RightPane::Closeout);
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let rects = screen::right_pane_rects(&model, area);
    assert_eq!(
        rects.iter().map(|(pane, _)| *pane).collect::<Vec<_>>(),
        vec![RightPane::Todos, RightPane::Closeout, RightPane::Tasks]
    );
    let drawn = draw_app(&app);
    assert!(drawn.contains(" todos "), "{drawn}");
    assert!(drawn.contains(" closeout "), "{drawn}");
    assert!(drawn.contains(" tasks "), "{drawn}");
}

#[test]
fn toggle_right_hides_the_column_and_restores_the_same_set() {
    let mut app = todos_app(true);
    app.tasks = vec![one_task()];
    app.right_panes.insert(RightPane::Tasks);
    let area = Rect::new(0, 0, 76, 24);
    let before = app.right_panes.clone();
    apply_pane(&mut app, Effect::ToggleRight);
    assert!(!app.right_open);
    assert_eq!(app.right_panes, before);
    let hidden = screen_model(&app);
    let split = screen::split_of(&hidden, area);
    assert_eq!(split.todos.width, 0);
    assert_eq!(split.session.x + split.session.width, area.width);
    assert_ne!(
        mouse(click(area.width - 1, split.session.y + 2), &hidden, area),
        Some(Effect::ToggleRight)
    );
    apply_pane(&mut app, Effect::ToggleRight);
    assert!(app.right_open);
    assert_eq!(app.right_panes, before);
    let rects = screen::right_pane_rects(&screen_model(&app), area);
    assert_eq!(
        rects.iter().map(|(pane, _)| *pane).collect::<Vec<_>>(),
        vec![RightPane::Todos, RightPane::Tasks]
    );
}

#[test]
fn a_click_on_a_pane_title_does_not_close_it() {
    let app = todos_app(true);
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let rect = screen::pane_rect(&model, area, RightPane::Todos).expect("todos pane");
    assert_eq!(mouse(click(rect.x + 2, rect.y), &model, area), None);
    assert!(model.right_panes.contains(&RightPane::Todos));
}

#[test]
fn a_click_on_thinking_opens_the_thoughts_and_esc_closes_them() {
    let frame = crate::mock::thinking();
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        frame.selected.clone(),
    );
    app.sessions = frame.sessions.clone();
    app.cards = frame.cards.clone();
    app.phase = frame.phase;
    app.thinking = frame.thinking.clone();
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let mut hit = None;
    for y in 0..area.height {
        for x in 0..area.width {
            if screen::thinking_at(&model, area, x, y) {
                hit = Some((x, y));
                break;
            }
        }
    }
    let (x, y) = hit.expect("the thinking row");
    assert_eq!(mouse(click(x, y), &model, area), Some(Effect::OpenThinking));
    assert_ne!(
        mouse(click(x, y.saturating_sub(1)), &model, area),
        Some(Effect::OpenThinking)
    );
    open_thinking_overlay(&mut app);
    match screen_model(&app).overlay {
        Some(Overlay::Thinking { text }) => assert_eq!(text, "ponder the answer"),
        other => panic!("expected thoughts, got {other:?}"),
    }
    apply_flight(
        &mut app,
        Some(screen::Phase::Thinking),
        Some("ponder the answer more".into()),
    );
    match screen_model(&app).overlay {
        Some(Overlay::Thinking { text }) => assert_eq!(text, "ponder the answer more"),
        other => panic!("expected the longer thought, got {other:?}"),
    }
    let drawn = draw_app(&app);
    assert!(drawn.contains("ponder the answer more"), "{drawn}");
    apply_flight(&mut app, Some(screen::Phase::Tool), None);
    match screen_model(&app).overlay {
        Some(Overlay::Thinking { text }) => assert_eq!(text, "ponder the answer more"),
        other => panic!("expected the kept thought, got {other:?}"),
    }
    assert_eq!(
        key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            mode(&app),
            true
        ),
        Some(Effect::CloseOverlay)
    );
    close_overlay(&mut app);
    assert!(screen_model(&app).overlay.is_none());
    let closed = draw_app(&app);
    assert!(closed.contains("Using tool"), "{closed}");
    assert!(!closed.contains("ponder the answer"), "{closed}");
}

#[test]
fn a_click_on_the_percent_opens_the_context_overlay_and_esc_closes_it() {
    let frame = crate::mock::idle();
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        frame.selected.clone(),
    );
    app.sessions = frame.sessions.clone();
    app.cards = frame.cards.clone();
    let cards = app.cards.clone();
    app.context = Some(crate::compact::context_usage(
        "abcd",
        "",
        "xxxx",
        &[],
        None,
        None,
    ));
    let bare = draw_app(&app);
    assert!(bare.contains(" Kyoto Agent "), "{bare}");
    assert!(bare.contains(screen::LIST_GLYPH), "{bare}");
    assert!(!bare.contains('%'), "{bare}");
    assert!(screen_model(&app).context_percent.is_none());
    open_context_overlay(&mut app);
    assert!(!app.context_open);

    app.context = Some(crate::compact::context_usage(
        &"s".repeat(2_100 * 4),
        &"k".repeat(400 * 4),
        &"t".repeat(4_800 * 4),
        &[crate::chat::Message::User {
            content: "m".repeat(23_900 * 4).into(),
        }],
        Some(30_000),
        Some(256_000),
    ));
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    assert_eq!(model.context_percent, Some(12));
    assert_eq!(mouse(click(16, 0), &model, area), Some(Effect::OpenContext));
    assert_eq!(mouse(click(1, 0), &model, area), Some(Effect::ToggleLeft));
    let panes = screen::panes_glyph_column(&model, area.width).expect("the panes glyph");
    assert_eq!(
        mouse(click(panes, 0), &model, area),
        Some(Effect::ToggleRight)
    );
    let mut closed = model.clone();
    closed.left_open = false;
    closed.right_open = false;
    assert!(screen::list_glyph_at(&closed, area, 1, 0));
    let closed_panes = screen::panes_glyph_column(&closed, area.width).expect("glyph stays");
    assert_eq!(
        mouse(click(closed_panes, 0), &closed, area),
        Some(Effect::ToggleRight)
    );
    open_context_overlay(&mut app);
    match screen_model(&app).overlay {
        Some(Overlay::Context {
            percent,
            used,
            reported_prompt_tokens,
            window,
            buckets,
        }) => {
            assert_eq!(percent, 12);
            assert_eq!(used, 31_200);
            assert_eq!(reported_prompt_tokens, Some(30_000));
            assert_eq!(window, 256_000);
            assert_eq!(
                buckets
                    .iter()
                    .map(|bucket| bucket.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["system", "tools", "skills", "messages", "free"]
            );
            let first_four: u64 = buckets
                .iter()
                .filter(|bucket| bucket.id != "free")
                .map(|bucket| bucket.tokens.unwrap_or(0))
                .sum();
            assert_eq!(first_four, used);
            assert_eq!(used + buckets[4].tokens.unwrap_or(0), window);
        }
        other => panic!("expected the context overlay, got {other:?}"),
    }
    let drawn = draw_app(&app);
    assert!(drawn.contains("CONTEXT  12%"), "{drawn}");
    assert!(drawn.contains("31,200 / 256,000"), "{drawn}");
    assert_eq!(
        key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            mode(&app),
            true
        ),
        Some(Effect::CloseOverlay)
    );
    close_overlay(&mut app);
    assert!(screen_model(&app).overlay.is_none());
    assert!(!app.context_open);
    assert_eq!(app.cards, cards);
    let closed = draw_app(&app);
    assert!(!closed.contains("CONTEXT"), "{closed}");
    assert!(closed.contains("12%"), "{closed}");
}

fn app_from(model: ScreenModel) -> App {
    let mut app = App::new(
        PathBuf::from("/home/u/work/kyotoagent"),
        PathBuf::from("/home/u"),
        model.selected.clone(),
    );
    app.sessions = model.sessions;
    app.cards = model.cards;
    app.area = Rect::new(0, 0, 76, 24);
    app.follow = false;
    app.scroll = model.scroll;
    app
}

fn cells_of(model: &ScreenModel, phrase: &str) -> (u16, u16) {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(76, 24);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| screen::render(model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let mut acc = String::new();
        let mut starts = Vec::new();
        for x in 0..buffer.area.width {
            starts.push(acc.len());
            acc.push_str(buffer[(x, y)].symbol());
        }
        if let Some(at) = acc.find(phrase) {
            let x = starts
                .iter()
                .position(|start| *start == at)
                .expect("phrase starts on a cell");
            return (x as u16, y);
        }
    }
    panic!("missing {phrase}");
}

fn painted(model: &ScreenModel) -> String {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(76, 24);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| screen::render(model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn overlay_cells_of(model: &ScreenModel, phrase: &str) -> (u16, u16) {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let area = Rect::new(0, 0, 76, 24);
    let backend = TestBackend::new(76, 24);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| screen::render(model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let mut acc = String::new();
        let mut starts = Vec::new();
        for x in 0..buffer.area.width {
            starts.push(acc.len());
            acc.push_str(buffer[(x, y)].symbol());
        }
        let mut from = 0;
        while let Some(at) = acc[from..].find(phrase) {
            let at = from + at;
            let x = starts
                .iter()
                .position(|start| *start == at)
                .expect("phrase starts on a cell");
            let col = x as u16;
            if screen::overlay_at(model, area, col, y) {
                return (col, y);
            }
            from = at + phrase.len();
        }
    }
    panic!("missing overlay {phrase}");
}

fn decode_osc(payload: &str) -> String {
    use base64::Engine;
    let body = payload
        .strip_prefix("\u{1b}]52;c;")
        .and_then(|rest| rest.strip_suffix('\u{7}'))
        .expect("osc 52");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body)
        .expect("base64");
    String::from_utf8(bytes).expect("utf-8")
}

fn example_docs() -> String {
    let mut url = String::from("https:");
    url.push('/');
    url.push('/');
    url.push_str("example.com");
    url
}

fn drive_mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) -> Option<Effect> {
    let event = pointer(kind, column, row);
    let effect = track_mouse(app, app.area, event)?;
    if apply_pane(app, effect.clone()) {
        None
    } else {
        Some(effect)
    }
}

#[test]
fn dragging_across_the_idle_result_copies_the_wrapped_slice() {
    let _cap = capture_copy();
    let mut app = app_from(crate::mock::idle());
    let model = screen_model(&app);
    let (x0, y0) = cells_of(&model, "The readme now names the");
    let (x1, y1) = cells_of(&model, "binary");
    let end_x = x1 + 5;
    assert!(drive_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x0, y0).is_none());
    assert!(app.select.expect("armed").held);
    drive_mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), end_x, y1);
    assert!(drive_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), end_x, y1).is_none());
    let text = decode_osc(&last_copied().expect("osc 52"));
    assert!(text.contains("The readme now names the"), "{text:?}");
    assert!(text.contains("binary"), "{text:?}");
    assert!(!text.contains("RESULT"), "{text:?}");
    let kept = app.select.expect("highlight stays");
    assert!(!kept.held);
    assert!(!kept.is_empty());
    drive_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x0, y0);
    assert!(app.select.expect("new press").is_empty());
}

#[test]
fn a_click_on_a_link_copies_nothing_and_a_drag_does_not_open_it() {
    let _open = capture_open_url();
    let _copy = capture_copy();
    let url = example_docs();
    let mut app = app_from(crate::mock::idle());
    let line = format!("See [docs]({url}) now.");
    app.cards = vec![Card::result(&line)];
    let model = screen_model(&app);
    let (x, y) = cells_of(&model, "docs");
    assert!(drive_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y).is_none());
    let effect = drive_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
    assert_eq!(effect, Some(Effect::OpenLink(url.clone())));
    assert!(last_copied().is_none());
    assert!(last_opened_url().is_none());
    assert!(drive_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y).is_none());
    drive_mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x + 3, y);
    assert!(drive_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 3, y).is_none());
    assert!(last_opened_url().is_none());
    let text = decode_osc(&last_copied().expect("drag copies"));
    assert!(text.contains("doc"), "{text:?}");
}

#[test]
fn dragging_a_pane_border_does_not_copy() {
    let _cap = capture_copy();
    let mut app = todos_app(false);
    app.area = Rect::new(0, 0, 76, 24);
    let split = screen::split_of(&screen_model(&app), app.area);
    let border_x = split.list.x + split.list.width - 1;
    let row = split.list.y + 2;
    let area = app.area;
    let start = track_mouse(
        &mut app,
        area,
        pointer(MouseEventKind::Down(MouseButton::Left), border_x, row),
    )
    .expect("border");
    assert_eq!(start, Effect::DragLeft(border_x));
    apply_pane(&mut app, start);
    let dragged = track_mouse(
        &mut app,
        area,
        pointer(MouseEventKind::Drag(MouseButton::Left), 20, row),
    )
    .expect("drag");
    assert_eq!(dragged, Effect::SetLeftWidth(21));
    apply_pane(&mut app, dragged);
    let up = track_mouse(
        &mut app,
        area,
        pointer(MouseEventKind::Up(MouseButton::Left), 20, row),
    );
    assert_eq!(up, Some(Effect::EndDrag));
    assert!(last_copied().is_none());
    assert!(app.select.is_none());
}

#[test]
fn a_failed_copy_is_a_notice() {
    let _cap = capture_copy();
    fail_captured_copy();
    let mut app = app_from(crate::mock::idle());
    let model = screen_model(&app);
    let (x0, y0) = cells_of(&model, "The");
    let (x1, y1) = cells_of(&model, "readme");
    drive_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x0, y0);
    drive_mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        x1 + 5,
        y1,
    );
    drive_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x1 + 5, y1);
    let notice = app.notice.expect("notice");
    assert!(notice.contains("could not copy"), "{notice}");
    assert!(last_copied().is_none());
}

#[test]
fn dragging_overlay_body_text_copies_it() {
    let _cap = capture_copy();
    let mut app = app_from(crate::mock::idle());
    app.overlay = true;
    let model = screen_model(&app);
    let area = Rect::new(0, 0, 76, 24);
    let (x, y) = overlay_cells_of(&model, "test");
    assert!(screen::overlay_at(&model, area, x, y));
    drive_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    drive_mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x + 3, y);
    drive_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 3, y);
    let text = decode_osc(&last_copied().expect("overlay copy"));
    assert!(text.contains("test"), "{text:?}");
    assert_eq!(
        app.select.expect("overlay select").anchor.place,
        screen::SelectPlace::Overlay
    );
}

fn draw_app(app: &App) -> String {
    let backend = ratatui::backend::TestBackend::new(76, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("a terminal");
    let model = screen_model(app);
    terminal
        .draw(|frame| screen::render(&model, frame.area(), frame))
        .expect("draws");
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn two_running_tasks_name_the_count_and_list_both_lines() {
    let app = tasks_app(vec![
        one_task(),
        screen::TaskLine {
            id: "cd34ef56".into(),
            argv: "cargo test".into(),
            state: "running".into(),
        },
    ]);
    let area = Rect::new(0, 0, 76, 24);
    let drawn = {
        let backend = ratatui::backend::TestBackend::new(76, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("a terminal");
        let model = screen_model(&app);
        terminal
            .draw(|frame| screen::render(&model, frame.area(), frame))
            .expect("draws");
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    };
    assert!(!drawn.contains("2 running"), "{drawn}");
    let mut open = app;
    open_tasks_overlay(&mut open);
    let listed = screen_model(&open);
    assert!(listed.overlay.is_none());
    assert_eq!(listed.tasks.len(), 2);
    let mut hits = Vec::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(id) = screen::task_line_at(&listed, area, x, y) {
                if !hits.contains(&id) {
                    hits.push(id);
                }
            }
        }
    }
    assert_eq!(hits, vec!["ab12cd34".to_string(), "cd34ef56".to_string()]);
    let (x, y) = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .find(|(x, y)| screen::task_line_at(&listed, area, *x, *y).as_deref() == Some("ab12cd34"))
        .expect("the first task row");
    assert_eq!(
        mouse(click(x, y), &listed, area),
        Some(Effect::OpenTask("ab12cd34".into()))
    );
}

fn one_schedule(id: &str, note: &str, remaining_min: u64) -> screen::ScheduleLine {
    screen::ScheduleLine {
        id: id.into(),
        note: note.into(),
        remaining_min,
    }
}

fn drawn_frame(app: &App) -> String {
    let backend = ratatui::backend::TestBackend::new(76, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("a terminal");
    let model = screen_model(app);
    terminal
        .draw(|frame| screen::render(&model, frame.area(), frame))
        .expect("draws");
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn a_click_on_the_schedules_row_opens_the_list_and_esc_returns_to_the_pane() {
    let mut app = tasks_app(Vec::new());
    app.schedules = vec![one_schedule("cd34ef56", "Check gh comments", 10)];
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let split = screen::split_of(&model, area);
    assert_eq!(split.input.y, split.session.y + split.session.height);
    assert_ne!(
        mouse(
            click(split.session.x + 2, split.input.y.saturating_sub(1)),
            &model,
            area
        ),
        Some(Effect::OpenSchedules)
    );
    open_schedules_overlay(&mut app);
    let opened = screen_model(&app);
    assert!(opened.overlay.is_none());
    assert!(opened.right_panes.contains(&RightPane::Schedules));
    assert_eq!(opened.schedules[0].note, "Check gh comments");
    assert_eq!(opened.schedules[0].remaining_min, 10);
    let drawn = drawn_frame(&app);
    assert!(drawn.contains("In 10 min"), "{drawn}");
    assert!(drawn.contains("Check gh"), "{drawn}");
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Idle, false),
        Some(Effect::CloseOverlay)
    );
    close_overlay(&mut app);
    assert!(screen_model(&app).overlay.is_none());
    assert!(app.right_panes.contains(&RightPane::Schedules));
}

#[test]
fn two_pending_schedules_name_the_count_and_list_soonest_first() {
    let app = {
        let mut app = tasks_app(Vec::new());
        app.schedules = vec![
            one_schedule("cd34ef56", "Check gh comments", 10),
            one_schedule("ab12cd34", "Look at CI", 20),
        ];
        app
    };
    let drawn = drawn_frame(&app);
    assert!(!drawn.contains("2 due"), "{drawn}");
    let mut open = app;
    open_schedules_overlay(&mut open);
    let opened = screen_model(&open);
    assert!(opened.overlay.is_none());
    assert_eq!(opened.schedules.len(), 2);
    assert_eq!(opened.schedules[0].note, "Check gh comments");
    assert_eq!(opened.schedules[1].note, "Look at CI");
    let listed = drawn_frame(&open);
    assert!(listed.contains("In 10 min"), "{listed}");
    assert!(listed.contains("Check gh"), "{listed}");
    assert!(listed.contains("In 20 min"), "{listed}");
    assert!(listed.contains("Look at CI"), "{listed}");
}

#[test]
fn dragging_the_list_border_changes_the_left_width() {
    let mut app = todos_app(false);
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let split = screen::split_of(&model, area);
    let border_x = split.list.x + split.list.width - 1;
    let start = mouse(click(border_x, split.list.y + 2), &model, area).expect("border");
    assert_eq!(start, Effect::DragLeft(border_x));
    apply_pane(&mut app, start);
    let dragged = mouse(drag_to(20, split.list.y + 2), &screen_model(&app), area).expect("drag");
    assert_eq!(dragged, Effect::SetLeftWidth(21));
    apply_pane(&mut app, dragged);
    assert_eq!(app.left_width, 21);
    assert!(app.left_open);
}

#[test]
fn collapsing_the_left_list_leaves_a_rail() {
    let mut app = todos_app(false);
    let area = Rect::new(0, 0, 76, 24);
    let before = screen::split_of(&screen_model(&app), area);
    assert_eq!(before.list.width, LIST_WIDTH);
    apply_pane(&mut app, Effect::ToggleLeft);
    assert!(!app.left_open);
    let after = screen::split_of(&screen_model(&app), area);
    assert_eq!(after.list.width, 1);
    assert!(after.session.width > before.session.width);
}

fn todos_inner(model: &ScreenModel, area: Rect) -> Rect {
    let todos = screen::split_of(model, area).todos;
    Rect {
        x: todos.x.saturating_add(1),
        y: todos.y.saturating_add(1),
        width: todos.width.saturating_sub(2),
        height: todos.height.saturating_sub(2),
    }
}

#[tokio::test]
async fn clicking_a_todo_expands_and_collapses_details_without_widening() {
    let mut app = todos_app(true);
    let area = Rect::new(0, 0, 76, 24);
    app.area = area;
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    let model = screen_model(&app);
    let inner = todos_inner(&model, area);
    let x = inner.x + 2;
    let row = inner.y + 2;
    let effect = released_click(&model, area, x, row).expect("todo click");
    assert_eq!(effect, Effect::OpenTodo("write".into()));
    apply(&mut app, &client, effect).await.unwrap();
    assert_eq!(app.open_todo.as_deref(), Some("write"));
    assert_eq!(app.right_width, TODOS_WIDTH);
    let expanded = draw_app(&app);
    assert!(expanded.contains("in_progress"), "{expanded}");
    assert!(expanded.contains("Replace content"), "{expanded}");
    assert!(expanded.contains("src/events.rs"), "{expanded}");
    let effect = released_click(&screen_model(&app), area, x, row).expect("todo click");
    apply(&mut app, &client, effect).await.unwrap();
    assert!(app.open_todo.is_none());
    assert!(app.right_open);
    assert_eq!(app.right_width, TODOS_WIDTH);
    assert!(!draw_app(&app).contains("Replace content"));
    focus_todo(&mut app, "write".into());
    focus_todo(&mut app, "read".into());
    assert_eq!(app.open_todo.as_deref(), Some("read"));
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Idle, false),
        Some(Effect::CloseOverlay)
    );
    close_overlay(&mut app);
    assert!(app.open_todo.is_none());
    assert!(app.right_open);
    assert_eq!(app.right_width, TODOS_WIDTH);
}

#[test]
fn a_click_on_the_todos_left_edge_starts_a_right_width_drag() {
    let app = todos_app(true);
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let todos = screen::split_of(&model, area).todos;
    let border_x = todos.x;
    let row = todos.y + 2;
    assert_eq!(screen::todo_at(&model, area, border_x, row), None);
    assert_eq!(
        mouse(click(border_x, row), &model, area),
        Some(Effect::DragRight(border_x))
    );
}

#[test]
fn todo_at_skips_empty_space_and_the_pane_title() {
    let app = todos_app(true);
    let area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let todos = screen::split_of(&model, area).todos;
    let inner = todos_inner(&model, area);
    assert_eq!(
        screen::todo_at(&model, area, inner.x + 1, inner.y).as_deref(),
        None
    );
    assert_eq!(
        screen::todo_at(&model, area, inner.x + 1, inner.y + 1).as_deref(),
        Some("read")
    );
    assert_eq!(
        screen::todo_at(&model, area, inner.x + 1, inner.y + 4),
        None
    );
    assert_eq!(screen::todo_at(&model, area, inner.x + 1, todos.y), None);
    let mut closed = todos_app(false);
    closed.right_open = false;
    let collapsed = screen_model(&closed);
    let hidden = screen::split_of(&collapsed, area);
    assert_eq!(hidden.todos.width, 0);
    assert_eq!(
        screen::todo_at(&collapsed, area, area.width - 1, hidden.session.y + 1),
        None
    );
}

#[test]
fn an_empty_right_pane_keeps_the_column_open() {
    let mut app = todos_app(true);
    let area = Rect::new(0, 0, 76, 24);
    app.area = area;
    app.todos.clear();
    prune_panes(&mut app);
    assert!(app.right_open);
    assert!(app.right_panes.contains(&RightPane::Todos));
    assert!(screen::split_of(&screen_model(&app), area).todos.width > 0);
}

fn docs_rs_serde() -> String {
    let mut url = String::from("https:");
    url.push('/');
    url.push('/');
    url.push_str("docs.rs/serde");
    url
}

fn hit(
    model: &ScreenModel,
    area: Rect,
    finder: fn(&ScreenModel, Rect, u16, u16) -> Option<String>,
    want: &str,
) -> (u16, u16) {
    for y in 0..area.height {
        for x in 0..area.width {
            if finder(model, area, x, y).as_deref() == Some(want) {
                return (x, y);
            }
        }
    }
    panic!("no hit for {want}");
}

#[test]
fn a_click_on_a_todo_file_opens_the_file_effect() {
    let mut app = todos_app(true);
    let area = Rect::new(0, 0, 76, 24);
    app.area = area;
    app.open_todo = Some("write".into());
    app.overlay = true;
    let model = screen_model(&app);
    let (x, y) = hit(&model, area, screen::file_at, "src/events.rs");
    assert_eq!(
        released_click(&model, area, x, y),
        Some(Effect::OpenFile("src/events.rs".into()))
    );
}

#[test]
fn a_click_on_a_todo_link_opens_the_url() {
    let mut app = todos_app(true);
    let area = Rect::new(0, 0, 76, 24);
    app.area = area;
    app.open_todo = Some("write".into());
    app.overlay = true;
    let model = screen_model(&app);
    let url = docs_rs_serde();
    let (x, y) = hit(&model, area, screen::link_at, &url);
    assert_eq!(
        released_click(&model, area, x, y),
        Some(Effect::OpenLink(url))
    );
}

#[test]
fn a_click_on_a_pull_overlay_url_opens_the_browser() {
    let mut app = idle_proof_app(Card::proof("cargo test passed.", &[]));
    let url = format!(
        "{}/pmdroid/kyotoagent/pull/14",
        crate::session::github_origin()
    );
    app.sessions[0].pull_url = Some(url.clone());
    app.overlay = true;
    app.overlay_pull = true;
    let area = Rect::new(0, 0, 76, 24);
    app.area = area;
    let model = screen_model(&app);
    let (x, y) = hit(&model, area, screen::link_at, &url);
    assert_eq!(
        released_click(&model, area, x, y),
        Some(Effect::OpenLink(url.clone()))
    );
    let title_y = screen::session_inner_of(&model, area).y.saturating_sub(1);
    let title_x = screen::session_inner_of(&model, area).x;
    assert_eq!(mouse(click(title_x, title_y), &model, area), None);
}

#[test]
fn a_click_on_a_dimmed_wrote_line_does_nothing_while_a_popup_is_open() {
    let mut app = idle_proof_app(Card::proof(
        "Wrote README.md",
        &[(
            "test",
            crate::screen::ItemKind::Command,
            crate::screen::Outcome::Passed,
        )],
    ));
    app.overlay = true;
    let area = Rect::new(0, 0, 76, 24);
    app.area = area;
    let model = screen_model(&app);
    let (x, y) = hit(&model, area, screen::file_at, "README.md");
    assert!(!screen::overlay_at(&model, area, x, y));
    assert_eq!(released_click(&model, area, x, y), None);
}

#[test]
fn esc_on_a_file_opened_from_a_todo_returns_to_the_todo() {
    let mut app = todos_app(true);
    app.open_todo = Some("write".into());
    app.overlay = true;
    app.open_file = Some(FileView {
        path: "src/events.rs".into(),
        text: "mod events;".into(),
        truncated: false,
    });
    match screen_model(&app).overlay {
        Some(Overlay::File {
            path,
            text,
            truncated,
        }) => {
            assert_eq!(path, "src/events.rs");
            assert_eq!(text, "mod events;");
            assert!(!truncated);
        }
        other => panic!("expected a file overlay, got {other:?}"),
    }
    close_overlay(&mut app);
    let back = screen_model(&app);
    assert!(back.overlay.is_none());
    assert_eq!(back.open_todo.as_deref(), Some("write"));
    assert!(app.open_file.is_none());
    let drawn = draw_app(&app);
    assert!(drawn.contains("Replace content with"), "{drawn}");
    assert!(drawn.contains("src/events.rs"), "{drawn}");
}

#[test]
fn a_stubbed_link_open_records_the_url() {
    let _cap = capture_open_url();
    let url = docs_rs_serde();
    open_url(&url).expect("stubbed open");
    assert_eq!(last_opened_url().as_deref(), Some(url.as_str()));
}

#[test]
fn browser_candidates_start_with_the_platform_opener_after_browser() {
    let linux = browser_openers(None, false);
    assert_eq!(linux[0].program, "xdg-open");
    assert!(linux[0].leading.is_empty());
    assert_eq!(linux[1].program, "gio");
    assert_eq!(linux[1].leading, ["open"]);
    let mac = browser_openers(None, true);
    assert_eq!(mac[0].program, "open");
    assert_eq!(mac[1].program, "xdg-open");
    assert_eq!(mac[2].program, "gio");
    assert_eq!(mac[2].leading, ["open"]);
    for macos in [false, true] {
        let chosen = browser_openers(Some("/opt/bin/firefox"), macos);
        assert_eq!(chosen[0].program, "/opt/bin/firefox");
        assert!(chosen[0].leading.is_empty());
        let next = if macos { "open" } else { "xdg-open" };
        assert_eq!(chosen[1].program, next);
    }
    assert_eq!(browser_openers(Some(""), false)[0].program, "xdg-open");
}

#[test]
fn a_missing_first_opener_falls_through_to_the_next() {
    let url = "https://example.com/next";
    let mut calls = Vec::new();
    open_in_browser(url, Some("/opt/bin/firefox"), true, |opener, opened| {
        calls.push((
            opener.program.clone(),
            opener.leading.to_vec(),
            opened.to_string(),
        ));
        if opener.program == "/opt/bin/firefox" {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "No such file or directory",
            ))
        } else {
            Ok(())
        }
    })
    .expect("the next opener runs");
    assert_eq!(
        calls,
        vec![
            (
                "/opt/bin/firefox".to_string(),
                Vec::<&str>::new(),
                url.to_string()
            ),
            ("open".to_string(), Vec::<&str>::new(), url.to_string()),
        ]
    );

    let mut gio = None;
    open_in_browser(url, None, false, |opener, opened| {
        if opener.program == "gio" {
            gio = Some((opener.leading.to_vec(), opened.to_string()));
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "No such file or directory",
            ))
        }
    })
    .expect("gio opens the url");
    assert_eq!(gio, Some((vec!["open"], url.to_string())));
}

#[test]
fn a_missing_opener_names_the_program_in_the_notice() {
    let _cap = capture_open_url();
    let mut app = app_from(crate::mock::idle());
    let url = "https://example.com/missing";
    stub_missing_opener(None, false);
    open_link(&mut app, url);
    let notice = app.notice.take().expect("notice");
    assert!(notice.ends_with(": xdg-open not found"), "{notice}");
    assert!(notice.contains(url), "{notice}");
    assert!(!notice.contains("No such file or directory"), "{notice}");
    assert!(last_opened_url().is_none());

    stub_missing_opener(None, true);
    open_link(&mut app, url);
    let notice = app.notice.take().expect("notice");
    assert!(notice.ends_with(": open not found"), "{notice}");
    assert!(notice.contains(url), "{notice}");

    stub_missing_opener(Some("/opt/bin/firefox"), false);
    open_link(&mut app, url);
    let notice = app.notice.take().expect("notice");
    assert!(notice.ends_with(": /opt/bin/firefox not found"), "{notice}");
    assert!(notice.contains(url), "{notice}");
    assert!(!notice.contains("No such file or directory"), "{notice}");
}

fn list_column(model: &screen::ScreenModel) -> String {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let backend = TestBackend::new(76, 24);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| screen::render(model, frame.area(), frame))
        .expect("the screen draws");
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..30 {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn clicking_a_project_header_hides_and_shows_its_rows() {
    let model = crate::mock::projects();
    let area = Rect::new(0, 0, 76, 24);
    let list = screen::split_of(&model, area).list;
    let column = list.x + 2;
    let row = list.y + 1;
    assert_eq!(
        mouse(click(column, row), &model, area),
        Some(Effect::ToggleHeader("kyotoagent".into()))
    );
    let mut app = App::new(
        PathBuf::from("/home/u/work/kyotoagent"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions = model.sessions.clone();
    app.sessions[0].title = Some("Main task".into());
    app.area = area;
    assert!(apply_pane(
        &mut app,
        Effect::ToggleHeader("kyotoagent".into())
    ));
    let hidden = list_column(&screen_model(&app));
    assert!(
        !hidden.contains("Main task"),
        "collapsed kyotoagent still lists the session:\n{hidden}"
    );
    assert!(hidden.contains("acpbot"), "{hidden}");
    assert!(hidden.contains("other"), "{hidden}");
    assert!(hidden.contains("notes"), "{hidden}");
    assert!(apply_pane(
        &mut app,
        Effect::ToggleHeader("kyotoagent".into())
    ));
    let shown = list_column(&screen_model(&app));
    assert!(shown.contains("Main task"), "{shown}");
    assert!(shown.contains("acpbot"), "{shown}");
}

#[test]
fn left_collapses_a_selected_header_and_right_expands_it() {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions = crate::mock::projects().sessions;
    app.sessions[0].title = Some("Main task".into());
    app.list_header = Some("kyotoagent".into());
    assert_eq!(
        key(press(KeyCode::Left), Mode::Idle, false),
        Some(Effect::CollapseHeader)
    );
    assert_eq!(
        key(press(KeyCode::Right), Mode::Idle, false),
        Some(Effect::ExpandHeader)
    );
    assert!(apply_pane(&mut app, Effect::CollapseHeader));
    assert!(app.collapsed.contains("kyotoagent"));
    let hidden = list_column(&screen_model(&app));
    assert!(!hidden.contains("Main task"), "{hidden}");
    assert!(apply_pane(&mut app, Effect::ExpandHeader));
    assert!(!app.collapsed.contains("kyotoagent"));
    assert!(list_column(&screen_model(&app)).contains("Main task"));
}

#[test]
fn selecting_a_session_in_a_collapsed_group_expands_it() {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "3f2ae04c".into(),
    );
    app.sessions = crate::mock::projects().sessions;
    app.sessions[0].title = Some("Main task".into());
    app.collapsed.insert("kyotoagent".into());
    select_session(&mut app, "91bc7a1d".into());
    assert!(!app.collapsed.contains("kyotoagent"));
    assert!(app.list_header.is_none());
    assert!(list_column(&screen_model(&app)).contains("Main task"));
}

fn closeout_app() -> App {
    let mut app = App::new(
        PathBuf::from("/w"),
        PathBuf::from("/home/u"),
        "91bc7a1d".into(),
    );
    app.sessions = crate::mock::idle().sessions;
    app.cards = crate::mock::idle().cards;
    app.area = Rect::new(0, 0, 76, 24);
    let mut tail = vec![
        "---".to_string(),
        "\u{2514}\u{2500}\u{2500} fail".to_string(),
    ];
    for index in 0..40 {
        tail.push(format!("row{index}"));
    }
    app.closeout = vec![
        screen::CloseoutCheck {
            runs: Vec::new(),
            id: "test".into(),
            kind: "command".into(),
            required: true,
            status: screen::CloseoutMark::Running,
            exit: None,
            attempt: None,
            tail: String::new(),
        },
        screen::CloseoutCheck {
            runs: Vec::new(),
            id: "lint".into(),
            kind: "command".into(),
            required: true,
            status: screen::CloseoutMark::Failed,
            exit: Some(1),
            attempt: Some(1),
            tail: tail.join("\n"),
        },
    ];
    app
}

fn wheel(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn a_click_on_the_strip_opens_the_pane_and_a_row_selects_the_check() {
    let app = closeout_app();
    let area = app.area;
    let model = screen_model(&app);
    let split = screen::split_of(&model, area);
    assert_eq!(split.input.y, split.session.y + split.session.height);
    assert_ne!(
        mouse(
            click(split.session.x + 2, split.input.y.saturating_sub(1)),
            &model,
            area
        ),
        Some(Effect::OpenCloseout)
    );
    let mut opened = closeout_app();
    opened.right_open = true;
    opened.right_panes.insert(RightPane::Closeout);
    let model = screen_model(&opened);
    let (x, y) = hit(&model, area, screen::closeout_row_at, "lint");
    assert_eq!(
        mouse(click(x, y), &model, area),
        Some(Effect::OpenCheck("lint".into()))
    );
    let pane = screen::closeout_pane_rect(&model, area);
    assert_eq!(mouse(click(pane.x + 2, pane.y), &model, area), None);
    assert_eq!(
        mouse(click(pane.x, pane.y), &model, area),
        Some(Effect::DragRight(pane.x))
    );
    let todos = todos_app(false);
    let todo_model = screen_model(&todos);
    let progress = screen::split_of(&todo_model, area);
    assert_ne!(
        mouse(
            click(progress.session.x + 2, progress.input.y.saturating_sub(1)),
            &todo_model,
            area
        ),
        Some(Effect::TogglePane(RightPane::Todos))
    );
}

#[test]
fn esc_leaves_the_closeout_pane_open() {
    let mut app = closeout_app();
    app.right_open = true;
    app.right_panes.insert(RightPane::Closeout);
    app.open_check = Some("lint".into());
    app.closeout_scroll = 4;
    assert_eq!(
        key(press(KeyCode::Esc), Mode::Idle, false),
        Some(Effect::CloseOverlay)
    );
    close_overlay(&mut app);
    assert_eq!(app.open_check.as_deref(), Some("lint"));
    assert!(app.right_panes.contains(&RightPane::Closeout));
    assert_eq!(app.closeout_scroll, 4);
}

#[tokio::test]
async fn wheel_over_the_tail_scrolls_the_tail_and_not_the_chat() {
    let mut app = closeout_app();
    app.area = Rect::new(0, 0, 76, 14);
    app.right_open = true;
    app.right_panes.insert(RightPane::Closeout);
    app.open_check = Some("lint".into());
    app.scroll = 2;
    app.follow = false;
    assert!(follow_tail(&app) >= 2);
    let area = app.area;
    let model = screen_model(&app);
    let (column, row) = hit(&model, area, screen::closeout_row_at, "lint");
    assert!(screen::pane_scroll_max(&model, area, RightPane::Closeout) > 0);
    app.pointer = Some((column, row));
    assert_eq!(
        mouse(wheel(MouseEventKind::ScrollDown, column, row), &model, area),
        Some(Effect::ScrollDown)
    );
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    apply(&mut app, &client, Effect::ScrollDown).await.unwrap();
    assert_eq!(app.closeout_scroll, 1);
    assert_eq!(app.scroll, 2);
    apply(&mut app, &client, Effect::PageDown).await.unwrap();
    assert!(app.closeout_scroll > 1);
    assert_eq!(app.scroll, 2);
    let chat = screen::session_inner_of(&screen_model(&app), area);
    assert!(chat.height >= 1, "{chat:?}");
    app.pointer = Some((chat.x, chat.y));
    let before = app.closeout_scroll;
    assert_eq!(
        mouse(
            wheel(MouseEventKind::ScrollUp, chat.x, chat.y),
            &screen_model(&app),
            area
        ),
        Some(Effect::ScrollUp)
    );
    apply(&mut app, &client, Effect::ScrollUp).await.unwrap();
    assert_eq!(app.closeout_scroll, before);
    assert_eq!(app.scroll, 1);
}

#[test]
fn todos_toggle_preserves_the_column_width_like_other_panes() {
    for pane in [RightPane::Todos, RightPane::Tasks, RightPane::Schedules] {
        let mut app = todos_app(false);
        app.area = Rect::new(0, 0, 160, 40);
        app.right_width = 28;
        toggle_pane(&mut app, pane);
        assert!(app.right_open);
        assert_eq!(app.right_width, 28);
        toggle_pane(&mut app, pane);
        toggle_pane(&mut app, pane);
        assert_eq!(app.right_width, 28);
    }
}
#[tokio::test]
async fn mouse_wheel_scrolls_the_help_command_list() {
    let mut app = todos_app(false);
    app.area = Rect::new(0, 0, 76, 24);
    open_help(&mut app);
    let model = screen_model(&app);
    let pane = screen::split_of(&model, app.area).session;
    let column = pane.x + pane.width / 2;
    let row = pane.y + pane.height / 2;
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    let effect = track_mouse(
        &mut app,
        Rect::new(0, 0, 76, 24),
        wheel(MouseEventKind::ScrollDown, column, row),
    )
    .expect("help scroll");
    apply(&mut app, &client, effect).await.unwrap();
    assert!(matches!(
        app.command_ui,
        Some(CommandUi::Help { scroll: 1 })
    ));
    let effect = track_mouse(
        &mut app,
        Rect::new(0, 0, 76, 24),
        wheel(MouseEventKind::ScrollUp, column, row),
    )
    .expect("help scroll");
    apply(&mut app, &client, effect).await.unwrap();
    assert!(matches!(
        app.command_ui,
        Some(CommandUi::Help { scroll: 0 })
    ));
}
#[test]
fn input_wrapping_uses_the_same_layout_for_scroll_and_rendering() {
    let mut app = todos_app(false);
    app.area = Rect::new(0, 0, 76, 24);
    app.ask = "x".repeat(150);
    let layout = screen::split_of(&layout_model(&app), app.area);
    let drawn = screen::split_of(&screen_model(&app), app.area);
    assert_eq!(layout.input.height, 3);
    assert_eq!(layout.session, drawn.session);
}

#[test]
fn mac_delete_shortcuts_match_grok_word_and_line_actions() {
    assert_eq!(
        key(
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
            Mode::Idle,
            false
        ),
        Some(Effect::DeleteWord)
    );
    assert_eq!(
        key(
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::SUPER),
            Mode::Idle,
            false
        ),
        Some(Effect::DeleteLine)
    );
    assert_eq!(
        key(ctrl('w'), Mode::Idle, false),
        Some(Effect::ConfirmDelete)
    );
    assert_eq!(key(ctrl('u'), Mode::Idle, false), Some(Effect::DeleteLine));
    let mut app = todos_app(false);
    app.ask = "hello-world".into();
    edit_delete(&mut app, Effect::DeleteWord);
    assert_eq!(app.ask, "hello-");
    app.ask = "one\ntwo words".into();
    edit_delete(&mut app, Effect::DeleteLine);
    assert_eq!(app.ask, "one\n");
    app.ask = "hello cafe\u{301}".into();
    edit_delete(&mut app, Effect::DeleteWord);
    assert_eq!(app.ask, "hello ");
    app.ask.clear();
    edit_delete(&mut app, Effect::DeleteWord);
    assert!(app.ask.is_empty());
}
#[test]
fn fast_delete_edits_active_query_and_leaves_hidden_file_draft_alone() {
    let mut app = todos_app(false);
    app.ask = "preserve draft".into();
    open_palette(&mut app);
    if let Some(CommandUi::Palette { query, .. }) = &mut app.command_ui {
        *query = "model effort".into();
    }
    edit_delete(&mut app, Effect::DeleteWord);
    assert!(matches!(&app.command_ui, Some(CommandUi::Palette { query, .. }) if query == "model "));
    app.command_ui = None;
    app.open_file = Some(FileView {
        path: "x.rs".into(),
        text: "code".into(),
        truncated: false,
    });
    edit_delete(&mut app, Effect::DeleteLine);
    assert_eq!(app.ask, "preserve draft");
}
#[tokio::test]
async fn file_preview_wheel_and_page_scroll_without_moving_chat() {
    let mut app = todos_app(false);
    app.area = Rect::new(0, 0, 100, 24);
    app.open_file = Some(FileView {
        path: "x.txt".into(),
        text: (0..80).map(|i| format!("line {i}\n")).collect(),
        truncated: false,
    });
    app.overlay = true;
    let model = screen_model(&app);
    let pane = screen::split_of(&model, app.area).session;
    let client = Client::at(PathBuf::from("/tmp/kyotoagent-no-socket"));
    let effect = mouse(
        wheel(
            MouseEventKind::ScrollDown,
            pane.x + pane.width / 2,
            pane.y + pane.height / 2,
        ),
        &model,
        app.area,
    )
    .expect("file wheel");
    apply(&mut app, &client, effect).await.unwrap();
    assert_eq!(app.file_scroll, 1);
    assert_eq!(app.scroll, 0);
    apply(&mut app, &client, Effect::PageDown).await.unwrap();
    assert!(app.file_scroll > 1);
    let rendered = draw_app(&app);
    assert!(!rendered.contains("line 0 "), "{rendered}");
    scroll_file(&mut app, false, usize::MAX);
    assert_eq!(
        app.file_scroll,
        screen::file_scroll_max(&screen_model(&app), app.area)
    );
    scroll_file(&mut app, true, usize::MAX);
    assert_eq!(app.file_scroll, 0);
}

#[tokio::test]
async fn clicking_an_older_large_result_opens_its_full_text_and_scrolls() {
    let mut app = todos_app(false);
    app.area = Rect::new(0, 0, 100, 30);
    app.follow = false;
    app.cards = vec![
        Card::result(&(0..80).map(|i| format!("older {i}\n")).collect::<String>()),
        Card::result("newest result"),
    ];
    let model = screen_model(&app);
    let effect = (0..app.area.height)
        .flat_map(|y| (0..app.area.width).map(move |x| (x, y)))
        .find_map(|(x, y)| {
            let effect = press_at(&model, app.area, x, y)?;
            matches!(&effect, Effect::OpenText(_)).then_some(effect)
        })
        .expect("large card is clickable");
    let client = Client::at(PathBuf::from("/unused"));
    apply(&mut app, &client, effect).await.unwrap();
    assert!(
        matches!(&screen_model(&app).overlay, Some(Overlay::Text { text }) if text.contains("older 79"))
    );
    assert_eq!(keystroke(&app, press(KeyCode::Enter)), None);
    scroll_file(&mut app, false, usize::MAX);
    assert!(draw_app(&app).contains("older 79"));
    close_overlay(&mut app);
    assert!(app.open_text.is_none());
    assert!(!app.overlay);
}

#[tokio::test]
async fn a_large_bracketed_paste_is_a_clickable_preview_with_the_original_draft() {
    let mut app = todos_app(false);
    app.ask = "before ".into();
    let text = String::from("🐕\n").repeat(80);
    let client = Client::at(PathBuf::from("/unused"));
    apply(&mut app, &client, Effect::Paste(text.clone()))
        .await
        .unwrap();
    assert_eq!(app.ask, format!("before {text}"));
    apply(&mut app, &client, Effect::Type('!')).await.unwrap();
    let model = screen_model(&app);
    assert!(model.bottom.contains("[Pasted input:"));
    assert!(model.bottom.ends_with('!'));
    assert_eq!(model.pasted_text, Some(format!("before {text}!")));
    let rect = screen::split_of(&model, app.area).input;
    assert!(matches!(
        press_at(&model, app.area, rect.x, rect.y),
        Some(Effect::OpenText(_))
    ));
    apply(&mut app, &client, Effect::Backspace).await.unwrap();
    apply(&mut app, &client, Effect::Backspace).await.unwrap();
    assert_eq!(app.ask, "before ");
    assert!(screen_model(&app).pasted_text.is_none());
}

#[tokio::test]
async fn a_small_bracketed_paste_keeps_newlines_and_does_not_submit() {
    let mut app = todos_app(false);
    let client = Client::at(PathBuf::from("/unused"));
    apply(&mut app, &client, Effect::Paste("hello\nworld".into()))
        .await
        .unwrap();
    assert_eq!(app.ask, "hello\nworld");
    assert!(app.pastes.is_empty());
}

pub(super) async fn saved_test_server(
    home: &Path,
    name: &str,
) -> (
    std::sync::Arc<server::Server>,
    tokio::task::JoinHandle<()>,
    String,
) {
    let root = home.join(name);
    let certs = root.join("certs");
    std::fs::create_dir_all(&certs).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    std::fs::write(certs.join("server.crt"), cert.pem()).unwrap();
    std::fs::write(certs.join("server.key"), key.serialize_pem()).unwrap();
    let text = format!("base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"unavailable-model\"\nlisten = \"127.0.0.1:0\"\n[projects.test]\npath = \"{}\"\n", root.display());
    std::fs::write(root.join("config.toml"), &text).unwrap();
    let config = crate::config::Config::from_toml(&text).unwrap();
    let serving = std::sync::Arc::new(server::Server::new(&root, &config).unwrap());
    let cloned = serving.clone();
    let task = tokio::spawn(async move {
        cloned.serve().await.unwrap();
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let addr = loop {
        if let Some(addr) = serving.https_addr() {
            break addr;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let token = crate::pairing::PairingKey::load(&root)
        .unwrap()
        .token()
        .unwrap();
    let uri = format!("kyotoagent://{addr}?token={token}");
    let client = Client::at_url(&uri).unwrap();
    let body = serde_json::json!({"workspace": root}).to_string();
    assert_eq!(
        client
            .request("POST", "/v1/sessions", Some(&body))
            .await
            .unwrap()
            .0,
        201
    );
    let path = home.join(".kyotoagent/config.toml");
    let id = crate::pairing::Connections::remember(&path, &uri, false).unwrap();
    (serving, task, id)
}

#[tokio::test]
async fn server_switching_preserves_drafts_and_fetches_only_the_connected_servers_models() {
    let home = std::env::temp_dir().join(format!("kyotoagent-tui-servers-{}", std::process::id()));
    let (_, first_task, first) = saved_test_server(&home, "first").await;
    let (_, second_task, second) = saved_test_server(&home, "second").await;
    let path = home.join(".kyotoagent/config.toml");
    crate::pairing::Connections::select(&path, Some(&first)).unwrap();
    let saved = crate::pairing::Connections::load(&path).unwrap();
    let mut client = Client::at_url(&saved.servers[&first]).unwrap();
    let mut app = App::new(home.clone(), home.clone(), String::new());
    app.server = Some(first.clone());
    poll(&mut app, &client).await.unwrap();
    let first_session = app.selected.clone();
    app.ask = "first draft".to_string();
    let image =
        crate::attachment::ImageAttachment::from_bytes("dog.png", crate::splash::PNG).unwrap();
    app.images.insert(app.selected.clone(), vec![image]);
    app.picker = Some(Picker::Effort {
        rows: EFFORTS.iter().map(|row| (*row).to_string()).collect(),
        highlight: 0,
    });
    connections::open_server_picker(&mut app);
    assert_eq!(mode(&app), Mode::Question { choices: 8 });
    let names = app.server_step.clone().unwrap();
    let index = names.iter().position(|name| name == &second).unwrap() + 1;
    let drawn = draw_app(&app);
    assert!(drawn.contains("Which server?"));
    assert!(!drawn.contains("token="));
    assert_eq!(
        keystroke(
            &app,
            press(KeyCode::Char(
                char::from_digit(index as u32 + 1, 10).unwrap()
            ))
        ),
        Some(Effect::Choose(index))
    );
    apply(&mut app, &client, Effect::Choose(index))
        .await
        .unwrap();
    assert!(app.server_step.is_none());
    let target = app.server_requested.take().unwrap();
    let mut cached = BTreeMap::new();
    connections::switch_server(&mut app, &mut client, &mut cached, target)
        .await
        .unwrap();
    assert_eq!(app.server.as_ref(), Some(&second));
    poll(&mut app, &client).await.unwrap();
    assert!(app.sessions[0].workspace.ends_with("second"));
    assert!(app.ask.is_empty());
    assert!(app.picker.is_none());
    assert!(images::pending(&app).is_empty());
    assert_eq!(
        crate::pairing::Connections::load(&path)
            .unwrap()
            .server
            .as_ref(),
        Some(&second)
    );
    open_model_picker(&mut app, &client).await;
    settle_catalog(&mut app).await;
    assert!(app.picker.is_none());
    assert_eq!(
        app.notice.as_deref(),
        Some("default: Could not reach the model endpoint.")
    );
    app.ask = "second draft".to_string();
    connections::switch_server(&mut app, &mut client, &mut cached, Some(first.clone()))
        .await
        .unwrap();
    assert_eq!(app.ask, "first draft");
    poll(&mut app, &client).await.unwrap();
    assert_eq!(app.selected, first_session);
    assert!(app.sessions[0].workspace.ends_with("first"));
    assert_eq!(images::pending(&app).len(), 1);
    assert!(app.picker.is_none());
    let before = std::fs::read_to_string(&path).unwrap();
    assert!(connections::switch_server(
        &mut app,
        &mut client,
        &mut cached,
        Some("missing".to_string())
    )
    .await
    .is_err());
    assert_eq!(app.server.as_ref(), Some(&first));
    assert_eq!(app.ask, "first draft");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    let bad_uri = format!("kyotoagent://{second}?token=invalid-token");
    crate::pairing::Connections::remember(&path, &bad_uri, false).unwrap();
    assert!(
        connections::switch_server(&mut app, &mut client, &mut cached, Some(second.clone()))
            .await
            .is_err()
    );
    assert_eq!(app.server.as_ref(), Some(&first));
    assert_eq!(app.ask, "first draft");
    assert_eq!(
        crate::pairing::Connections::load(&path)
            .unwrap()
            .server
            .as_ref(),
        Some(&first)
    );
    crate::pairing::Connections::remember(&path, &saved.servers[&second], false).unwrap();
    connections::switch_server(&mut app, &mut client, &mut cached, Some(second))
        .await
        .unwrap();
    assert_eq!(app.ask, "second draft");
    first_task.abort();
    second_task.abort();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn newer_yolo_model_and_turn_updates_reject_an_older_poll_response() {
    let home = std::env::temp_dir().join(format!(
        "kyotoagent-refresh-revision-{}",
        std::process::id()
    ));
    let (_, serving, id) = saved_test_server(&home, "host").await;
    std::fs::write(
        home.join("host/config.toml"),
        "base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"unavailable-model\"\n",
    )
    .unwrap();
    let saved = crate::pairing::Connections::load(&home.join(".kyotoagent/config.toml")).unwrap();
    let client = Client::at_url(&saved.servers[&id]).unwrap();
    let mut app = App::new(home.clone(), home.clone(), String::new());
    app.server = Some(id);
    poll(&mut app, &client).await.unwrap();
    let context = super::poll::PollContext::new(&app);
    let old = super::poll::fetch(&context, &client).await.unwrap();
    post_yolo(&mut app, &client, true).await.unwrap();
    assert!(!context.matches(&app));
    if context.matches(&app) {
        super::poll::apply_poll(&mut app, old);
    }
    assert!(app.yolo);
    let context = super::poll::PollContext::new(&app);
    let old = super::poll::fetch(&context, &client).await.unwrap();
    assert!(post_model(&mut app, &client, "new-model", Some("low"), None).await);
    assert!(!context.matches(&app));
    if context.matches(&app) {
        super::poll::apply_poll(&mut app, old);
    }
    assert_eq!(app.model, "new-model");
    assert_eq!(app.effort.as_deref(), Some("low"));
    let context = super::poll::PollContext::new(&app);
    let body = serde_json::json!({"text":"Start a task"}).to_string();
    assert_eq!(
        client
            .request(
                "POST",
                &format!("/v1/sessions/{}/messages", app.selected),
                Some(&body)
            )
            .await
            .unwrap()
            .0,
        202
    );
    poll(&mut app, &client).await.unwrap();
    assert!(!context.matches(&app));
    serving.abort();
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn remote_defaults_paths_and_efforts_come_from_the_selected_server() {
    let home = std::env::temp_dir().join(format!(
        "ra-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000_000
    ));
    std::fs::create_dir_all(&home).unwrap();
    let (_, serving, id) = saved_test_server(&home, "remote-authority").await;
    let saved = crate::pairing::Connections::load(&home.join(".kyotoagent/config.toml")).unwrap();
    let client = Client::at_url(&saved.servers[&id]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let catalog_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let models = axum::Router::new().route("/v1/models", axum::routing::get(|| async {
        axum::Json(serde_json::json!({"data":[{"id":"remote-model","reasoning_efforts":["low","high"]}]}))
    }));
    let catalog = tokio::spawn(async move {
        axum::serve(listener, models).await.unwrap();
    });
    let remote_root = home.join("remote-authority");
    std::fs::write(remote_root.join("config.toml"), format!("base_url = \"{catalog_url}\"\nmodel = \"remote-model\"\neffort = \"high\"\nyolo = true\nenhance = true\nshow_closeout = false\n")).unwrap();
    let mut app = App::new(
        PathBuf::from("/client-only/workspace"),
        home.clone(),
        String::new(),
    );
    app.server = Some(id);
    poll(&mut app, &client).await.unwrap();
    assert_eq!(app.workspace, std::env::current_dir().unwrap());
    let session = app.selected.clone();
    assert_eq!(
        client
            .request("DELETE", &format!("/v1/sessions/{session}"), None)
            .await
            .unwrap()
            .0,
        204
    );
    poll(&mut app, &client).await.unwrap();
    assert!(app.selected.is_empty());
    assert_eq!(app.model, "remote-model");
    assert_eq!(app.effort.as_deref(), Some("high"));
    assert!(app.yolo && app.enhance);
    assert!(!app.show_closeout);
    begin_new_session(&mut app, &client).await.unwrap();
    settle_catalog(&mut app).await;
    assert!(matches!(&app.workspace_step, WorkspaceStep::Projects(rows) if rows.is_empty()));
    let target = home.join("remote-workspace");
    std::fs::create_dir(&target).unwrap();
    let opaque = home.join("remote-link");
    std::os::unix::fs::symlink(&target, &opaque).unwrap();
    let project =
        serde_json::json!({"id":"remote-path", "name":"Remote path", "path":opaque}).to_string();
    assert_eq!(
        client
            .request("POST", "/v1/projects", Some(&project))
            .await
            .unwrap()
            .0,
        201
    );
    create_session(&mut app, &client, &opaque, false, None)
        .await
        .unwrap();
    let meta = crate::session::Session::at(&remote_root.join("sessions").join(&app.selected))
        .meta()
        .unwrap();
    assert_eq!(meta.requested_workspace.as_deref(), opaque.to_str());
    assert_eq!(app.model, "remote-model");
    open_effort_picker(&mut app, &client).await;
    settle_catalog(&mut app).await;
    assert!(
        matches!(&app.picker, Some(Picker::Effort { rows, .. }) if rows == &vec!["low".to_string(), "high".to_string()])
    );
    apply_effort(&mut app, &client, "medium").await;
    assert_eq!(app.effort.as_deref(), Some("high"));
    assert!(app.notice.as_deref().unwrap().contains("not available"));
    apply_effort(&mut app, &client, "low").await;
    poll(&mut app, &client).await.unwrap();
    assert_eq!(app.effort.as_deref(), Some("low"));
    apply_model_id(&mut app, &client, "client-only-model").await;
    assert_eq!(app.model, "remote-model");
    assert!(app.notice.as_deref().unwrap().contains("not available"));
    serving.abort();
    catalog.abort();
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn remote_session_matching_never_resolves_paths_on_the_client() {
    let home = std::env::temp_dir().join(format!(
        "ra-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000_000
    ));
    std::fs::create_dir_all(&home).unwrap();
    let target = home.join("target");
    std::fs::create_dir(&target).unwrap();
    let link = home.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut row = crate::mock::working().sessions.remove(0);
    row.workspace = link;
    assert!(newest_for_server(&[row.clone()], &target, true).is_none());
    assert_eq!(
        newest_for_server(&[row.clone()], &target, false),
        Some(row.id)
    );
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn a_failed_remote_view_cannot_restore_cached_execution_or_consume_drafts() {
    let home = std::env::temp_dir().join(format!("rv-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let (_, first_task, first) = saved_test_server(&home, "a").await;
    let (_, second_task, second) = saved_test_server(&home, "b").await;
    let path = home.join(".kyotoagent/config.toml");
    let saved = crate::pairing::Connections::load(&path).unwrap();
    let mut client = Client::at_url(&saved.servers[&first]).unwrap();
    let mut app = App::new(PathBuf::new(), home.clone(), String::new());
    app.server = Some(first.clone());
    poll(&mut app, &client).await.unwrap();
    let first_session = app.selected.clone();
    app.ask = "keep first draft".into();
    let mut cached = BTreeMap::new();
    connections::switch_server(&mut app, &mut client, &mut cached, Some(second.clone()))
        .await
        .unwrap();
    app.ask = "keep second draft".into();
    let before = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        home.join("a/sessions")
            .join(&first_session)
            .join(crate::session::EVENTS_FILE),
        "invalid event\n",
    )
    .unwrap();
    let first_client = Client::at_url(&saved.servers[&first]).unwrap();
    assert_eq!(
        first_client
            .request("GET", "/v1/sessions", None)
            .await
            .unwrap()
            .0,
        200
    );
    assert_ne!(
        first_client
            .request("GET", &format!("/v1/sessions/{first_session}/view"), None)
            .await
            .unwrap()
            .0,
        200
    );
    assert!(
        connections::switch_server(&mut app, &mut client, &mut cached, Some(first.clone()))
            .await
            .is_err()
    );
    assert_eq!(app.server.as_ref(), Some(&second));
    assert_eq!(client.server_id().as_ref(), Some(&second));
    assert_eq!(app.ask, "keep second draft");
    assert_eq!(cached.get(&Some(first)).unwrap().ask, "keep first draft");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    first_task.abort();
    second_task.abort();
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn closing_previews_leaves_underlying_enhancements_and_questions_unanswered() {
    for image in [false, true] {
        for enhance in [false, true] {
            let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), "91bc".into());
            app.sessions = vec![blank_row()];
            app.sessions[0].id = "91bc".into();
            app.sessions[0].status = Status::Waiting;
            app.sessions[0].waiting = Some(if enhance {
                Wait::Enhance
            } else {
                Wait::Question
            });
            app.cards = vec![if enhance {
                Card::Enhance {
                    source: "draft".into(),
                    text: "enhanced".into(),
                    error: None,
                    event_id: "e1".into(),
                }
            } else {
                Card::question("Choose", &[("yes", false), ("no", false)])
            }];
            let cards = app.cards.clone();
            app.overlay = true;
            let client = Client::at(PathBuf::from("/unused"));
            let effect = if image {
                Effect::OpenImage(
                    crate::attachment::ImageAttachment::from_bytes("dog.png", crate::splash::PNG)
                        .unwrap(),
                )
            } else {
                Effect::OpenText("complete text".into())
            };
            apply(&mut app, &client, effect).await.unwrap();
            assert!(!enhance_is_esc_target(&app));
            apply(&mut app, &client, Effect::CloseOverlay)
                .await
                .unwrap();
            assert_eq!(app.cards, cards);
            assert!(!app.overlay);
            assert!(app.open_image.is_none() && app.open_text.is_none());
        }
    }
}

#[tokio::test]
async fn shift_enter_inserts_a_newline_without_submitting_or_choosing() {
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), String::new());
    app.ask = "first".to_string();
    let client = Client::at(PathBuf::from("/missing-socket"));
    let shifted = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
    let effect = keystroke(&app, shifted).unwrap();
    assert_eq!(effect, Effect::Type('\n'));
    apply(&mut app, &client, effect).await.unwrap();
    apply(&mut app, &client, Effect::Type('s')).await.unwrap();
    assert_eq!(app.ask, "first\ns");
    assert_eq!(keystroke(&app, press(KeyCode::Enter)), Some(Effect::Submit));
    assert_eq!(keystroke(&app, ctrl('j')), Some(Effect::Type('\n')));
    for mode in [
        Mode::Idle,
        Mode::Working,
        Mode::Permission,
        Mode::Question { choices: 2 },
    ] {
        assert_eq!(key(shifted, mode, true), None);
    }
    app.ask = "/skill write something".into();
    assert_eq!(keystroke(&app, shifted), Some(Effect::Type('\n')));
    app.ask = "first\ns".into();
    open_palette(&mut app);
    assert_eq!(keystroke(&app, shifted), None);
    assert_eq!(app.ask, "first\ns");
}

#[tokio::test]
async fn queued_popup_preserves_draft_and_uses_ids_for_selection() {
    let mut app = App::new(PathBuf::from("/w"), PathBuf::from("/home/u"), "s".into());
    app.ask = "unfinished draft".into();
    app.queue = vec!["duplicate".into(), "duplicate".into(), "".into()];
    app.queue_items = vec![
        view::QueuedMessage {
            id: "one".into(),
            text: "duplicate".into(),
            image_count: 0,
            enhance: false,
        },
        view::QueuedMessage {
            id: "two".into(),
            text: "duplicate".into(),
            image_count: 0,
            enhance: true,
        },
        view::QueuedMessage {
            id: "image".into(),
            text: "".into(),
            image_count: 1,
            enhance: false,
        },
    ];
    queue::open(&mut app);
    queue::nudge(&mut app, false);
    assert_eq!(app.queue_highlight.as_deref(), Some("two"));
    app.queue_items.remove(0);
    queue::reconcile(&mut app);
    assert_eq!(app.queue_highlight.as_deref(), Some("two"));
    let model = screen_model(&app);
    assert!(
        matches!(model.overlay, Some(Overlay::Queue { ref rows, highlight: 0, .. }) if rows[1].contains("Image message") && rows[1].contains("1 image"))
    );
    assert!(!esc_stops(&app));
    assert_eq!(
        keystroke(&app, press(KeyCode::Delete)),
        Some(Effect::RemoveQueued)
    );
    assert_eq!(keystroke(&app, press(KeyCode::Enter)), None);
    let client = Client::at(PathBuf::from("/missing"));
    apply(&mut app, &client, Effect::Paste("accidental paste".into()))
        .await
        .unwrap();
    assert_eq!(app.ask, "unfinished draft");
    close_overlay(&mut app);
    assert!(screen_model(&app).overlay.is_none());
    assert_eq!(app.ask, "unfinished draft");
    assert!(command_catalog(&[])
        .iter()
        .any(|row| row.action == CommandAction::OpenQueue));
}

#[test]
fn clicking_the_queue_footer_opens_messages_without_opening_thinking() {
    for phase in [None, Some(screen::Phase::Thinking)] {
        let mut model = crate::mock::working();
        model.phase = phase;
        model.queue = 2;
        let area = Rect::new(0, 0, 100, 30);
        let mut hits = 0;
        for row in 0..area.height {
            for column in 0..area.width {
                if screen::queue_at(&model, area, column, row) {
                    assert_eq!(
                        mouse(click(column, row), &model, area),
                        Some(Effect::OpenQueue)
                    );
                    hits += 1;
                }
            }
        }
        assert_eq!(hits, " queued 2".len());
    }
}

async fn settle_catalog(app: &mut App) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while app.catalog_job.is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
            catalog::advance(app).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn delayed_catalog_can_be_cancelled_without_losing_the_draft() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let router = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(|| async {
                tokio::time::sleep(Duration::from_millis(250)).await;
                axum::Json(Vec::<ModelRow>::new())
            }),
        );
        axum::serve(listener, router).await.unwrap();
    });
    let client = Client {
        transport: Transport::Url {
            base: format!("http://{address}"),
            http: reqwest::Client::new(),
        },
        projects: Arc::new(Mutex::new(None)),
    };
    let mut app = App::new(
        PathBuf::from("/work"),
        PathBuf::from("/tmp"),
        "session".into(),
    );
    app.ask = "draft".into();
    open_model_picker(&mut app, &client).await;
    assert!(
        matches!(screen_model(&app).overlay, Some(Overlay::Text { text }) if text.contains("Fetching models"))
    );
    assert!(app.catalog_job.is_some());
    apply(&mut app, &client, Effect::CloseOverlay)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    catalog::advance(&mut app).await;
    assert!(app.catalog_popup.is_none());
    assert!(app.picker.is_none());
    assert_eq!(app.ask, "draft");
    server.abort();
}

#[test]
fn remote_new_sessions_offer_projects_and_registration_without_the_client_directory() {
    let mut app = App::new(
        PathBuf::from("/client-only"),
        PathBuf::from("/tmp"),
        String::new(),
    );
    app.server = Some("remote".into());
    open_project_prompt(
        &mut app,
        vec![ListedProject {
            id: "notes".into(),
            name: "Notes".into(),
            path: "/server/notes".into(),
        }],
    );
    assert!(
        matches!(screen_model(&app).overlay, Some(Overlay::Question { choices, .. }) if choices.iter().map(|choice| choice.label.as_str()).collect::<Vec<_>>() == vec!["Notes", "Add project"])
    );
    assert!(workspace_choice(&mut app, 0).is_none());
    assert_eq!(
        workspace_choice(&mut app, 0),
        Some((PathBuf::from("/server/notes"), false))
    );
    open_project_prompt(&mut app, Vec::new());
    assert!(workspace_choice(&mut app, 0).is_none());
    assert!(app.project_popup.is_some());
}

#[test]
fn notices_expire_without_replacing_drafts_and_new_notices_restart_the_timer() {
    let mut app = App::new(
        PathBuf::from("/work"),
        PathBuf::from("/tmp"),
        "session".into(),
    );
    app.ask = "keep this draft".into();
    app.notice = Some("Saving layout…".into());
    let start = Instant::now();
    advance_notice(&mut app, start);
    advance_notice(&mut app, start + Duration::from_secs(4));
    assert_eq!(screen_model(&app).bottom, "keep this draft");
    assert_eq!(screen_model(&app).toast.as_deref(), Some("Saving layout…"));
    app.notice = Some("Layout saved".into());
    advance_notice(&mut app, start + Duration::from_secs(4));
    advance_notice(&mut app, start + Duration::from_secs(8));
    assert_eq!(screen_model(&app).toast.as_deref(), Some("Layout saved"));
    advance_notice(&mut app, start + Duration::from_secs(9));
    assert!(screen_model(&app).toast.is_none());
    assert_eq!(app.ask, "keep this draft");
}

#[test]
fn toast_mouse_clicks_do_not_reach_the_underlying_pane() {
    let mut app = todos_app(true);
    app.notice = Some("Layout saved".into());
    app.area = Rect::new(0, 0, 76, 24);
    let model = screen_model(&app);
    let rect = screen::toast_rect(&model, app.area).unwrap();
    assert_eq!(mouse(click(rect.x + 1, rect.y + 1), &model, app.area), None);
    assert_eq!(
        mouse(click(rect.right() - 1, rect.y), &model, app.area),
        Some(Effect::DismissNotice)
    );
}
