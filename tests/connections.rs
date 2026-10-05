use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use kyotoagent::config::Config;
use kyotoagent::pairing::Connections;

#[test]
fn saved_servers_preserve_settings_replace_tokens_and_remember_selection() {
    let home = std::env::temp_dir().join(format!("kyotoagent-connections-{}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    let path = home.join("config.toml");
    let initial = "base_url = \"https://models.example/v1\"\nmodel = \"my-model\"\n";
    fs::write(&path, initial).unwrap();
    let first = "kyotoagent://first.example:7841?token=old-token";
    let second = "kyotoagent://second.example:7841?token=second-token";
    let id = Connections::remember(&path, first, true).unwrap();
    Connections::remember(&path, second, false).unwrap();
    let refreshed = "kyotoagent://first.example:7841?token=new-token";
    Connections::remember(&path, refreshed, false).unwrap();
    let saved = Connections::load(&path).unwrap();
    assert_eq!(saved.servers.len(), 2);
    assert_eq!(saved.server.as_deref(), Some(id.as_str()));
    assert_eq!(saved.selected(), Some(refreshed));
    assert_eq!(Config::load(&path).unwrap().model, "my-model");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let before = fs::read_to_string(&path).unwrap();
    assert!(Connections::remember(&path, "kyotoagent://invalid", true).is_err());
    assert!(Connections::select(&path, Some("missing")).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    Connections::select(&path, Some("second.example:7841")).unwrap();
    assert_eq!(Connections::load(&path).unwrap().selected(), Some(second));
    Connections::select(&path, None).unwrap();
    assert!(Connections::load(&path).unwrap().selected().is_none());
    assert_eq!(Connections::load(&path).unwrap().servers.len(), 2);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn renaming_and_removing_servers_preserves_credentials_settings_and_private_permissions() {
    let home = std::env::temp_dir().join(format!("ka-server-settings-{}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    let path = home.join("config.toml");
    for table in [
        "servers = { first = \"kyotoagent://first.example:7841?token=first-token\", second = \"kyotoagent://second.example:7841?token=second-token\" }\n",
        "[servers]\nfirst = \"kyotoagent://first.example:7841?token=first-token\"\nsecond = \"kyotoagent://second.example:7841?token=second-token\"\n",
    ] {
        fs::write(&path, format!("base_url = \"http://localhost:3456/v1\"\nmodel = \"my-model\"\nserver = \"first\"\n{table}\n[providers.custom]\nbase_url = \"http://localhost:3456/v1\"\nmodel = \"provider-model\"\n")).unwrap();
        Connections::rename(&path, "first", " Office ").unwrap();
        let saved = Connections::load(&path).unwrap();
        assert_eq!(saved.server_names["first"], "Office");
        assert_eq!(saved.server.as_deref(), Some("first"));
        assert_eq!(saved.selected(), Some("kyotoagent://first.example:7841?token=first-token"));
        Connections::remove(&path, "second").unwrap();
        assert_eq!(Connections::load(&path).unwrap().server.as_deref(), Some("first"));
        Connections::remove(&path, "first").unwrap();
        let saved = Connections::load(&path).unwrap();
        assert!(saved.servers.is_empty());
        assert!(saved.server_names.is_empty());
        assert!(saved.server.is_none());
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("token="));
        let config = Config::load(&path).unwrap();
        assert_eq!(config.model, "my-model");
        assert_eq!(config.providers["custom"].model, "provider-model");
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn invalid_server_edits_leave_config_unchanged_and_repairing_preserves_names() {
    let home = std::env::temp_dir().join(format!("ka-server-validation-{}", std::process::id()));
    let path = home.join("config.toml");
    let first =
        Connections::remember(&path, "kyotoagent://first.example:7841?token=old", true).unwrap();
    let second = Connections::remember(
        &path,
        "kyotoagent://second.example:7841?token=second",
        false,
    )
    .unwrap();
    Connections::rename(&path, &second, "Office").unwrap();
    let before = fs::read_to_string(&path).unwrap();
    for name in [
        "",
        "   ",
        "local",
        "LOCAL",
        "Office",
        "office",
        "bad\nname",
        "bad\u{1b}name",
    ] {
        assert!(Connections::rename(&path, &first, name).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }
    assert!(Connections::rename(&path, "missing", "New").is_err());
    assert!(Connections::remove(&path, "missing").is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    let refreshed = "kyotoagent://second.example:7841?token=new";
    assert_eq!(
        Connections::remember(&path, refreshed, true).unwrap(),
        second
    );
    let saved = Connections::load(&path).unwrap();
    assert_eq!(saved.server_names[&second], "Office");
    assert_eq!(saved.selected(), Some(refreshed));
    assert_eq!(saved.servers.len(), 2);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn pairing_command_does_not_save_issued_codes_or_invalid_imports() {
    let home = std::env::temp_dir().join(format!("kyotoagent-pair-save-{}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    let root = home.join(".kyotoagent");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    let before = "model = \"server-model\"\n";
    fs::write(&path, before).unwrap();
    let pair = |value: &str| {
        Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
            .args(["pair", value])
            .env("HOME", &home)
            .env("KYOTOAGENT_ROOT", &root)
            .env_remove("KYOTOAGENT_URL")
            .output()
            .unwrap()
    };
    let output = pair("first.example:7841");
    assert!(output.status.success());
    let issued = String::from_utf8(output.stdout).unwrap();
    assert!(issued.lines().last().unwrap().starts_with("kyotoagent://"));
    let saved = Connections::load(&path).unwrap();
    assert!(saved.servers.is_empty());
    assert!(saved.server.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    assert!(!pair("kyotoagent://invalid").status.success());
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    fs::remove_dir_all(home).unwrap();
}
