#[cfg(target_os = "linux")]
#[test]
fn disposable_verification_runs_use_the_real_binary() {
    let home = std::env::temp_dir().join(format!(
        "kyoto-verify-suite-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&home).unwrap();
    let output = std::process::Command::new("/usr/bin/python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/.agents/skills/verify-kyotoagent/helpers/test_run.py"
        ))
        .arg(env!("CARGO_BIN_EXE_kyotoagent"))
        .env("HOME", &home)
        .env("KYOTOAGENT_ROOT", home.join(".kyotoagent"))
        .output()
        .unwrap();
    std::fs::remove_dir_all(home).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
