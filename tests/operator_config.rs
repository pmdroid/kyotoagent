use std::{fs, path::Path, process::Command};

use kyotoagent::{
    permit::Answer,
    session::{Session, SessionMeta},
    tools::Tools,
};

#[test]
fn agent_tools_cannot_overwrite_operator_configuration() {
    if std::env::var_os("KYOTO_CONFIG_PROTECTION_CHILD").is_some() {
        let home = std::env::var_os("HOME").unwrap();
        let home = Path::new(&home);
        let config = home.join(".kyotoagent/config.toml");
        let workspace = home.join("workspace");
        let session = Session::at(&home.join("session"));
        session
            .create(&SessionMeta::new("protected", &workspace, "test", "now"))
            .unwrap();
        let tools = Tools::at(&session).unwrap();
        tools.gate().queue(Answer::allow_once());
        assert!(tools
            .write_file("t1", config.to_str().unwrap(), "overwritten")
            .is_err());
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            "operator configuration"
        );
        tools.gate().queue(Answer::allow_once());
        assert!(tools
            .search_replace("t1", config.to_str().unwrap(), "operator", "agent", false)
            .is_err());
        tools.gate().queue(Answer::allow_once());
        assert!(tools
            .write_file("t1", "config-link", "overwritten")
            .is_err());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let argv = vec![
                "sh".into(),
                "-c".into(),
                "printf allowed > ordinary; printf overwritten > \"$HOME/.kyotoagent/config.toml\""
                    .into(),
            ];
            let (_cancel_tx, mut cancel) = tokio::sync::watch::channel(false);
            let output = tools
                .execute_cancellable(&argv, Some(5), &mut cancel)
                .await
                .unwrap();
            assert_ne!(output.exit, Some(0), "{}", output.stderr);
            assert_eq!(
                fs::read_to_string(workspace.join("ordinary"))
                    .unwrap_or_else(|error| panic!("{error}: {}", output.stderr)),
                "allowed"
            );
            assert_eq!(
                fs::read_to_string(&config).unwrap(),
                "operator configuration"
            );
            let script = format!(
                "env HOME={} KYOTOAGENT_ROOT={} sh -c 'printf overwritten > {}'",
                home.join("temporary-home").display(),
                home.join("temporary-root").display(),
                config.display()
            );
            let task = tools
                .tasks()
                .start("t1", &["sh".into(), "-c".into(), script], Some(5))
                .await
                .unwrap();
            tools.tasks().wait_idle().await;
            assert_ne!(tools.tasks().check(&task.id).unwrap().exit, Some(0));
            assert_eq!(
                fs::read_to_string(&config).unwrap(),
                "operator configuration"
            );
            let argv = vec![
                "rm".into(),
                "-rf".into(),
                home.join(".kyotoagent").display().to_string(),
            ];
            let output = tools
                .execute_cancellable(&argv, Some(5), &mut cancel)
                .await
                .unwrap();
            assert_ne!(output.exit, Some(0));
            assert_eq!(
                fs::read_to_string(&config).unwrap(),
                "operator configuration"
            );
            let argv = vec![
                "sh".into(),
                "-c".into(),
                "printf isolated > \"$KYOTOAGENT_ROOT/config.toml\"".into(),
            ];
            let output = tools
                .execute_cancellable(&argv, Some(5), &mut cancel)
                .await
                .unwrap();
            assert_eq!(output.exit, Some(0), "{}", output.stderr);
        });
        return;
    }
    let root = std::env::temp_dir().join(format!("kyoto-config-protection-{}", std::process::id()));
    fs::create_dir_all(root.join(".kyotoagent")).unwrap();
    fs::create_dir_all(root.join("workspace")).unwrap();
    fs::create_dir_all(root.join("temporary-root")).unwrap();
    fs::write(
        root.join(".kyotoagent/config.toml"),
        "operator configuration",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        root.join(".kyotoagent/config.toml"),
        root.join("workspace/config-link"),
    )
    .unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "agent_tools_cannot_overwrite_operator_configuration",
            "--nocapture",
        ])
        .env("HOME", &root)
        .env("KYOTOAGENT_ROOT", root.join("temporary-root"))
        .env("KYOTO_CONFIG_PROTECTION_CHILD", "1")
        .output()
        .unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
