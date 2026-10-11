use std::{fs, path::Path, process::Command};

use kyotoagent::{
    permit::Answer,
    session::{Session, SessionMeta},
    tools::Tools,
};

#[test]
fn commands_run_without_a_sandbox_by_default() {
    if std::env::var_os("KYOTO_SANDBOX_OFF_CHILD").is_some() {
        check_unsandboxed_commands();
        return;
    }
    let home = std::env::temp_dir().join(format!(
        "kyoto-sandbox-off-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(home.join(".kyotoagent")).unwrap();
    let home = home.canonicalize().unwrap();
    let root = home.join("temporary-root");
    fs::create_dir(&root).unwrap();
    assert_eq!(home.canonicalize().unwrap(), home);
    assert_eq!(root.canonicalize().unwrap(), root);
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "commands_run_without_a_sandbox_by_default",
            "--nocapture",
        ])
        .env("HOME", &home)
        .env("KYOTOAGENT_ROOT", &root)
        .env("KYOTO_SANDBOX_OFF_CHILD", "1")
        .output()
        .unwrap();
    fs::remove_dir_all(home).unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn check_unsandboxed_commands() {
    let home = std::env::var_os("HOME").unwrap();
    let home = Path::new(&home);
    let workspace = home.join("workspace");
    fs::create_dir(&workspace).unwrap();
    let config = home.join(".kyotoagent/config.toml");
    fs::write(&config, "fixture configuration").unwrap();
    let session = Session::at(&home.join("session"));
    session
        .create(&SessionMeta::new("unsandboxed", &workspace, "test", "now"))
        .unwrap();
    let tools = Tools::at(&session).unwrap();
    tools.gate().queue(Answer::allow_once());
    assert!(tools
        .write_file("t1", config.to_str().unwrap(), "overwritten")
        .is_err());
    tools.gate().queue(Answer::allow_once());
    assert!(tools
        .search_replace(
            "t1",
            config.to_str().unwrap(),
            "fixture",
            "overwritten",
            false
        )
        .is_err());
    let argv = vec![
        "sh".into(),
        "-c".into(),
        "printf direct > \"$HOME/.kyotoagent/command-output\"".into(),
    ];
    tools.gate().queue(Answer::allow_once());
    let output = tools.run("t1", &argv, Some(5)).unwrap();
    assert_eq!(output.exit, Some(0), "{}", output.stderr);
    assert_eq!(
        fs::read_to_string(home.join(".kyotoagent/command-output")).unwrap(),
        "direct"
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (_cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
        let output = tools
            .execute_cancellable(&argv, Some(5), &mut cancel)
            .await
            .unwrap();
        assert_eq!(output.exit, Some(0), "{}", output.stderr);
        let task = tools.tasks().start("t1", &argv, Some(5)).await.unwrap();
        tools.tasks().wait_idle().await;
        let output = tools.tasks().check(&task.id).unwrap();
        assert_eq!(output.exit, Some(0), "{}", output.tail);
        fs::create_dir(workspace.join(".agents")).unwrap();
        let hook = "printf hook > \"$HOME/.kyotoagent/hook-output\" || exit 2";
        fs::write(
            workspace.join(".agents/hooks.json"),
            serde_json::json!({"hooks":{
                "PreToolUse":[{"hooks":[{"type":"command","command":hook}]}],
                "PostToolUse":[{"hooks":[{"type":"command","command":hook}]}],
                "Stop":[{"hooks":[{"type":"command","command":hook}]}]
            }})
            .to_string(),
        )
        .unwrap();
        let hooks = kyotoagent::hooks::load_in(&workspace, home);
        assert!(hooks
            .pre_tool_use(&workspace, "run", &serde_json::json!({}))
            .await
            .is_none());
        assert_eq!(
            fs::read_to_string(home.join(".kyotoagent/hook-output")).unwrap(),
            "hook"
        );
        fs::remove_file(home.join(".kyotoagent/hook-output")).unwrap();
        assert!(hooks
            .post_tool_use(&workspace, "run", &serde_json::json!({}), "ok")
            .await
            .is_none());
        assert_eq!(
            fs::read_to_string(home.join(".kyotoagent/hook-output")).unwrap(),
            "hook"
        );
        fs::remove_file(home.join(".kyotoagent/hook-output")).unwrap();
        assert!(hooks.stop(&workspace).await.is_none());
        assert_eq!(
            fs::read_to_string(home.join(".kyotoagent/hook-output")).unwrap(),
            "hook"
        );
    });
    assert_eq!(fs::read_to_string(config).unwrap(), "fixture configuration");
}

#[test]
fn agent_tools_cannot_overwrite_operator_configuration() {
    if let Ok(case) = std::env::var("KYOTO_CONFIG_PROTECTION_CHILD") {
        check_protected_configuration(&case);
        return;
    }
    for case in [
        "external",
        "managed",
        "managed-config-link",
        "worktrees-link",
    ] {
        let root = std::env::temp_dir().join(format!(
            "kyoto-config-protection-{}-{case}",
            std::process::id()
        ));
        let workspace = if case.starts_with("managed") {
            root.join(".kyotoagent/worktrees/project")
        } else {
            root.join("workspace")
        };
        fs::create_dir_all(root.join(".kyotoagent")).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(root.join("temporary-root")).unwrap();
        let config = root.join(".kyotoagent/config.toml");
        if case == "managed-config-link" {
            fs::write(workspace.join("actual-config"), "operator configuration").unwrap();
            std::os::unix::fs::symlink(workspace.join("actual-config"), &config).unwrap();
        } else {
            fs::write(&config, "operator configuration").unwrap();
        }
        if case == "worktrees-link" {
            std::os::unix::fs::symlink(
                root.join(".kyotoagent"),
                root.join(".kyotoagent/worktrees"),
            )
            .unwrap();
        }
        fs::write(root.join(".kyotoagent/auth.json"), "credentials").unwrap();
        std::os::unix::fs::symlink(&config, workspace.join("config-link")).unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent_tools_cannot_overwrite_operator_configuration",
                "--nocapture",
            ])
            .env("HOME", &root)
            .env("KYOTOAGENT_ROOT", root.join("temporary-root"))
            .env("KYOTO_CONFIG_PROTECTION_CHILD", case)
            .output()
            .unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(
            result.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

fn check_protected_configuration(case: &str) {
    let home = std::env::var_os("HOME").unwrap();
    let home = Path::new(&home);
    let config = home.join(".kyotoagent/config.toml");
    let workspace = if case.starts_with("managed") {
        home.join(".kyotoagent/worktrees/project")
    } else {
        home.join("workspace")
    };
    let session = Session::at(&home.join("session"));
    session
        .create(&SessionMeta::new("protected", &workspace, "test", "now"))
        .unwrap();
    let tools = Tools::at(&session).unwrap().with_sandbox(true);
    tools.gate().queue(Answer::allow_once());
    tools
        .write_file("t1", "source.txt", "before")
        .expect("workspace writes remain allowed");
    tools.gate().queue(Answer::allow_once());
    tools
        .search_replace("t1", "source.txt", "before", "after", false)
        .expect("workspace replacements remain allowed");
    assert_eq!(
        fs::read_to_string(workspace.join("source.txt")).unwrap(),
        "after"
    );
    for path in [
        config.clone(),
        workspace.join("config-link"),
        home.join(".kyotoagent/auth.json"),
    ] {
        tools.gate().queue(Answer::allow_once());
        assert!(tools
            .write_file("t1", path.to_str().unwrap(), "overwritten")
            .is_err());
    }
    tools.gate().queue(Answer::allow_once());
    assert!(tools
        .search_replace("t1", config.to_str().unwrap(), "operator", "agent", false)
        .is_err());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        if !cfg!(target_os = "linux") || !Path::new("/usr/bin/bwrap").is_file() {
            let argv = vec!["sh".into(), "-c".into(), "printf unsafe > should-not-run".into()];
            let (_cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
            let error = tools.execute_cancellable(&argv, Some(5), &mut cancel).await.unwrap_err();
            assert!(error.to_string().contains("sandbox = true requires"), "{error}");
            assert!(tools.tasks().start("t1", &argv, Some(5)).await.is_err());
            assert!(!workspace.join("should-not-run").exists());
            return;
        }
        fs::create_dir_all(workspace.join(".agents")).unwrap();
        let hook = "printf blocked > \"$HOME/.kyotoagent/auth.json\" || exit 2";
        fs::write(workspace.join(".agents/hooks.json"), serde_json::json!({"hooks":{
            "PreToolUse":[{"hooks":[{"type":"command","command":hook}]}],
            "PostToolUse":[{"hooks":[{"type":"command","command":hook}]}],
            "Stop":[{"hooks":[{"type":"command","command":hook}]}]
        }}).to_string()).unwrap();
        let hooks = kyotoagent::hooks::load_in(&workspace, home).with_sandbox(true);
        assert!(hooks.pre_tool_use(&workspace, "run", &serde_json::json!({})).await.is_some());
        assert!(hooks.post_tool_use(&workspace, "run", &serde_json::json!({}), "ok").await.is_some());
        assert!(hooks.stop(&workspace).await.is_some());
        assert_eq!(fs::read_to_string(home.join(".kyotoagent/auth.json")).unwrap(), "credentials");
        let argv = vec!["sh".into(), "-c".into(), "printf allowed > ordinary; printf overwritten > \"$HOME/.kyotoagent/config.toml\"".into()];
        let (_cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
        let output = tools.execute_cancellable(&argv, Some(5), &mut cancel).await.unwrap();
        assert_ne!(output.exit, Some(0), "{}", output.stderr);
        assert_eq!(fs::read_to_string(workspace.join("ordinary")).unwrap(), "allowed");
        assert_eq!(fs::read_to_string(&config).unwrap(), "operator configuration");
        let script = format!("env HOME={} KYOTOAGENT_ROOT={} sh -c 'printf task > task-output; printf overwritten > {}'", home.join("temporary-home").display(), home.join("temporary-root").display(), config.display());
        let task = tools.tasks().start("t1", &["sh".into(), "-c".into(), script], Some(5)).await.unwrap();
        tools.tasks().wait_idle().await;
        assert_ne!(tools.tasks().check(&task.id).unwrap().exit, Some(0));
        assert_eq!(fs::read_to_string(workspace.join("task-output")).unwrap(), "task");
        assert_eq!(fs::read_to_string(&config).unwrap(), "operator configuration");
        let argv = vec!["sh".into(), "-c".into(), "printf isolated > \"$KYOTOAGENT_ROOT/config.toml\"".into()];
        let output = tools.execute_cancellable(&argv, Some(5), &mut cancel).await.unwrap();
        assert_eq!(output.exit, Some(0), "{}", output.stderr);
        let argv = vec!["sh".into(), "-c".into(), "printf changed > \"$HOME/.kyotoagent/auth.json\"".into()];
        let output = tools.execute_cancellable(&argv, Some(5), &mut cancel).await.unwrap();
        assert_ne!(output.exit, Some(0));
        assert_eq!(fs::read_to_string(home.join(".kyotoagent/auth.json")).unwrap(), "credentials");
        let argv = vec!["rm".into(), "-rf".into(), home.join(".kyotoagent").display().to_string()];
        let output = tools.execute_cancellable(&argv, Some(5), &mut cancel).await.unwrap();
        assert_ne!(output.exit, Some(0));
        assert_eq!(fs::read_to_string(&config).unwrap(), "operator configuration");
    });
}
