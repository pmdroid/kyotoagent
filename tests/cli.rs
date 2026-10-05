use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn without_a_socket_the_bare_command_names_serve() {
    let home = std::env::temp_dir().join(format!("kyotoagent-bare-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&home);
    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    assert!(
        !output.status.success(),
        "kyotoagent exits when the socket is down"
    );
    let stderr = String::from_utf8(output.stderr).expect("the error is text");
    assert!(
        stderr.starts_with("kyotoagent:"),
        "the error names the launch command: {stderr}"
    );
    assert!(
        stderr.contains("kyotoagent serve"),
        "the error says what to do: {stderr}"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_kyoto"))
        .env("HOME", &home)
        .output()
        .expect("the kyoto binary runs");
    let stderr = String::from_utf8(output.stderr).expect("the error is text");
    assert!(
        stderr.starts_with("kyoto:"),
        "the error names the launch command: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn help_names_the_product_and_the_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .arg("--help")
        .output()
        .expect("the kyotoagent binary runs");
    assert!(output.status.success(), "kyotoagent --help succeeds");
    let stdout = String::from_utf8(output.stdout).expect("help is text");
    assert!(
        stdout.contains("Kyoto Agent"),
        "help names the product: {stdout}"
    );
    assert!(
        stdout.contains("kyotoagent serve"),
        "help still names serve: {stdout}"
    );
    assert!(
        stdout.contains("kyotoagent attach"),
        "help still names attach: {stdout}"
    );
    assert!(
        stdout.contains("--yolo"),
        "help names the yolo switch: {stdout}"
    );
    assert!(
        stdout.contains("--url"),
        "help names the url switch: {stdout}"
    );
    assert!(
        stdout.contains("kyotoagent new"),
        "help still names new: {stdout}"
    );
    assert!(
        !stdout.contains("kyotoagent login"),
        "help uses Providers for authentication: {stdout}"
    );
    assert!(
        !stdout.contains("kyotoagent logout"),
        "help uses Providers for credentials: {stdout}"
    );
    assert!(
        stdout.contains("kyotoagent provider"),
        "help names provider: {stdout}"
    );
    assert!(
        stdout.contains("kyotoagent doctor"),
        "help names doctor: {stdout}"
    );
    assert!(
        stdout.contains("--listen"),
        "help names the listen switch: {stdout}"
    );
    assert!(
        stdout.contains("KYOTOAGENT_URL"),
        "help names KYOTOAGENT_URL: {stdout}"
    );
    assert!(
        !stdout.contains("PAGENT_URL"),
        "help uses the current connection URL environment variable: {stdout}"
    );
    assert!(
        stdout.contains("Usage: kyotoagent ["),
        "the heading matches the launch name: {stdout}"
    );
}

#[test]
fn kyoto_help_uses_its_own_heading() {
    let output = Command::new(env!("CARGO_BIN_EXE_kyoto"))
        .arg("--help")
        .output()
        .expect("the kyoto binary runs");
    assert!(output.status.success(), "kyoto --help succeeds");
    let stdout = String::from_utf8(output.stdout).expect("help is text");
    assert!(
        stdout.contains("Usage: kyoto ["),
        "the heading matches the launch name: {stdout}"
    );
    assert!(
        stdout.contains("Kyoto Agent, a small remote-first coding agent."),
        "help names the product: {stdout}"
    );
    assert!(
        stdout.contains("kyotoagent serve"),
        "the sample command is kyotoagent serve: {stdout}"
    );
}

#[test]
fn provider_prints_the_selected_table() {
    let home = std::env::temp_dir().join(format!("kyotoagent-cli-provider-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let root = home.join(".kyotoagent");
    std::fs::create_dir_all(&root).expect("the root exists");
    std::fs::write(
        root.join("config.toml"),
        r#"
provider = "office"

[providers.office]
base_url = "https://openrouter.ai/api/v1"
model = "openai/gpt-4o"
api_key_env = "OPENROUTER_API_KEY"

[providers.local]
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5-coder"
"#,
    )
    .expect("the file writes");

    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["provider"])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    assert!(
        output.status.success(),
        "kyotoagent provider succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is text");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        ["office", "https://openrouter.ai/api/v1", "openai/gpt-4o"]
    );

    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["provider", "use", "local"])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    assert!(
        output.status.success(),
        "kyotoagent provider use succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["provider"])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    let stdout = String::from_utf8(output.stdout).expect("stdout is text");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        ["local", "http://127.0.0.1:11434/v1", "qwen2.5-coder"]
    );

    let before = std::fs::read(root.join("config.toml")).expect("the file reads");
    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["provider", "use", "missing"])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    assert!(
        !output.status.success(),
        "kyotoagent provider use missing exits non-zero"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is text");
    assert!(stderr.contains("missing"), "{stderr}");
    assert_eq!(
        std::fs::read(root.join("config.toml")).expect("the file still reads"),
        before
    );

    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args([
            "provider",
            "add",
            "office",
            "--base-url",
            "http://127.0.0.1:8080/v1",
            "--model",
            "replaced",
        ])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    assert!(
        output.status.success(),
        "kyotoagent provider add succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::fs::read_to_string(root.join("config.toml")).expect("the file reads");
    assert!(text.contains("provider = \"office\""), "{text}");
    assert!(text.contains("http://127.0.0.1:8080/v1"), "{text}");
    assert!(text.contains("replaced"), "{text}");
    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["provider"])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    let stdout = String::from_utf8(output.stdout).expect("stdout is text");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, ["office", "http://127.0.0.1:8080/v1", "replaced"]);

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn provider_add_creates_the_file_and_selects_the_table() {
    let home = std::env::temp_dir().join(format!(
        "kyotoagent-cli-provider-add-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args([
            "provider",
            "add",
            "local",
            "--base-url",
            "http://127.0.0.1:11434/v1",
            "--model",
            "qwen2.5-coder",
        ])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    assert!(
        output.status.success(),
        "kyotoagent provider add succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::fs::read_to_string(home.join(".kyotoagent").join("config.toml"))
        .expect("the file exists");
    assert!(text.contains("provider = \"local\""), "{text}");
    assert!(
        !home.join(".pagent").join("config.toml").exists(),
        "a new config is not written under .pagent"
    );
    assert!(text.contains("qwen2.5-coder"), "{text}");
    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["provider"])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    let stdout = String::from_utf8(output.stdout).expect("stdout is text");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        ["local", "http://127.0.0.1:11434/v1", "qwen2.5-coder"]
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_config_only_under_kyotoagent_is_not_read() {
    let home =
        std::env::temp_dir().join(format!("kyotoagent-cli-old-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let old = home.join(".pagent");
    std::fs::create_dir_all(&old).expect("the old root exists");
    std::fs::write(
        old.join("config.toml"),
        "provider = \"office\"\n\n[providers.office]\nbase_url = \"https://openrouter.ai/api/v1\"\nmodel = \"hidden\"\n",
    )
    .expect("the old file writes");
    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["provider"])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    assert!(
        !output.status.success(),
        "provider does not read a file that exists only under .pagent"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is text");
    assert!(!stdout.contains("hidden"), "{stdout}");
    assert!(!home.join(".kyotoagent").join("config.toml").exists());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn authentication_commands_are_rejected_without_changing_credentials() {
    let home = std::env::temp_dir().join(format!(
        "kyotoagent-cli-provider-auth-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    let root = home.join(".kyotoagent");
    std::fs::create_dir_all(&root).unwrap();
    for file in [
        "config.toml",
        "auth.json",
        "codex-auth.json",
        "opencode-auth.json",
    ] {
        std::fs::write(root.join(file), "credential-fixture").unwrap();
    }
    for args in [
        vec!["login"],
        vec!["login", "codex"],
        vec!["logout"],
        vec!["logout", "opencode"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
            .args(args)
            .env("HOME", &home)
            .env_remove("KYOTOAGENT_URL")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"));
        for file in [
            "config.toml",
            "auth.json",
            "codex-auth.json",
            "opencode-auth.json",
        ] {
            assert_eq!(
                std::fs::read_to_string(root.join(file)).unwrap(),
                "credential-fixture"
            );
        }
    }
    std::fs::remove_dir_all(&home).unwrap();
}

fn kyotoagent_with_stdin(
    home: &std::path::Path,
    args: &[&str],
    stdin_text: &str,
) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(args)
        .env("HOME", home)
        .env("OPENCODE_API_KEY", "stale-opencode-env")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the kyotoagent binary runs");
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin
            .write_all(stdin_text.as_bytes())
            .expect("the key is written");
    }
    child.wait_with_output().expect("kyotoagent finishes")
}

fn assert_output_hides(output: &std::process::Output, secret: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(secret) && !stderr.contains(secret),
        "the command printed the key"
    );
}

#[test]
fn provider_add_opencode_stores_the_key_outside_config() {
    let home = std::env::temp_dir().join(format!("kyotoagent-cli-opencode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let key = "oc-cli-key";
    let output =
        kyotoagent_with_stdin(&home, &["provider", "add", "opencode"], &format!("{key}\n"));
    assert!(
        output.status.success(),
        "kyotoagent provider add opencode succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_output_hides(&output, key);
    let root = home.join(".kyotoagent");
    let text = std::fs::read_to_string(root.join("config.toml")).expect("config exists");
    assert!(text.contains("provider = \"opencode\""), "{text}");
    assert!(text.contains("kind = \"opencode\""), "{text}");
    assert!(text.contains("kimi-k2.7-code"), "{text}");
    assert!(text.contains("opencode.ai/zen/v1"), "{text}");
    assert!(!text.contains(key), "config holds the key");
    assert!(!text.contains("api_key_env"), "{text}");
    let auth = root.join("opencode-auth.json");
    let mode = std::fs::metadata(&auth)
        .expect("the auth file exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let stored = kyotoagent::auth::load_opencode_key(&auth).expect("the key loads");
    assert_eq!(stored, key);
    let again = kyotoagent_with_stdin(
        &home,
        &["provider", "add", "opencode", "--model", "gpt-6.1-sol"],
        "oc-cli-key-2\n",
    );
    assert!(again.status.success(), "a second add succeeds");
    assert_output_hides(&again, "oc-cli-key-2");
    let text = std::fs::read_to_string(root.join("config.toml")).expect("config reads");
    assert!(text.contains("gpt-6.1-sol"), "{text}");
    assert!(!text.contains("kimi-k2.7-code"), "{text}");
    assert!(!text.contains("oc-cli-key-2"));
    assert_eq!(
        kyotoagent::auth::load_opencode_key(&auth).expect("the new key loads"),
        "oc-cli-key-2"
    );
    let env_add = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["provider", "add", "opencode", "--api-key-env", "ZEN_TOKEN"])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    assert!(env_add.status.success(), "api-key-env add succeeds");
    assert!(!auth.exists(), "api-key-env writes no auth file");
    let text = std::fs::read_to_string(root.join("config.toml")).expect("config reads");
    assert!(text.contains("api_key_env = \"ZEN_TOKEN\""), "{text}");
    assert!(!text.contains("oc-cli-key"));
    let empty = kyotoagent_with_stdin(&home, &["provider", "add", "opencode"], "\n");
    assert!(!empty.status.success(), "an empty key is an error");
    assert_output_hides(&empty, "oc-cli-key-2");
    let missing = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["provider", "add", "local"])
        .env("HOME", &home)
        .output()
        .expect("the kyotoagent binary runs");
    assert!(
        !missing.status.success(),
        "other ids still require the flags"
    );
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("--base-url is required"), "{stderr}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn provider_add_opencode_hides_the_key_on_a_terminal() {
    let home = std::env::temp_dir().join(format!(
        "kyotoagent-cli-opencode-tty-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    let script = r#"
import os, pty, select, sys
binary = sys.argv[1]
home = sys.argv[2]
key = os.environ["KYOTOAGENT_TEST_KEY"].encode()
master, slave = pty.openpty()
env = os.environ.copy()
env["HOME"] = home
proc = __import__("subprocess").Popen(
    [binary, "provider", "add", "opencode"],
    stdin=slave,
    stdout=slave,
    stderr=slave,
    env=env,
)
os.close(slave)
buf = b""
seen_prompt = False
deadline = __import__("time").time() + 5
while __import__("time").time() < deadline:
    ready, _, _ = select.select([master], [], [], 0.2)
    if master in ready:
        try:
            chunk = os.read(master, 4096)
        except OSError:
            break
        if not chunk:
            break
        buf += chunk
        if b"OpenCode API key:" in buf:
            seen_prompt = True
            break
    if proc.poll() is not None:
        break
if not seen_prompt:
    sys.exit(3)
if key in buf:
    sys.exit(2)
os.write(master, key + b"\n")
rest = b""
deadline = __import__("time").time() + 5
while __import__("time").time() < deadline:
    ready, _, _ = select.select([master], [], [], 0.2)
    if master in ready:
        try:
            chunk = os.read(master, 4096)
        except OSError:
            break
        if not chunk:
            break
        rest += chunk
    elif proc.poll() is not None:
        break
if key in rest:
    sys.exit(4)
code = proc.wait(timeout=5)
sys.exit(0 if code == 0 else 5)
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_kyotoagent"))
        .arg(&home)
        .env("KYOTOAGENT_TEST_KEY", "oc-tty-key")
        .output()
        .expect("python runs");
    assert!(
        output.status.success(),
        "terminal add status {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let text =
        std::fs::read_to_string(home.join(".kyotoagent").join("config.toml")).expect("config");
    assert!(!text.contains("oc-tty-key"));
    assert_eq!(
        kyotoagent::auth::load_opencode_key(&home.join(".kyotoagent").join("opencode-auth.json"))
            .expect("the key loads"),
        "oc-tty-key"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn docs_cover_commands_controls_and_model_settings() {
    let text =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .expect("README.md reads");
    assert!(!text.trim().is_empty(), "README.md is non-empty");
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .expect("README.md has a first line");
    assert!(
        first.contains("Kyoto Agent"),
        "the first heading or sentence names Kyoto Agent: {first}"
    );
    assert!(
        text.contains("docs/README.md"),
        "README.md links to the guides"
    );
    let text = ["cli.md", "tui.md", "slash.md", "model.md"]
        .into_iter()
        .map(|name| {
            std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("docs")
                    .join(name),
            )
            .expect("technical guide reads")
        })
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "attach",
        "serve",
        "new",
        "sessions",
        "log",
        "cancel",
        "Providers",
        "provider",
        "doctor",
        "--yolo",
        "--url",
        "--listen",
        "KYOTOAGENT_URL",
        "--base-url",
        "Ctrl-C",
        "Ctrl-M",
        "/model",
        "/effort",
        "/compact",
        "title_model",
    ] {
        assert!(text.contains(needle), "technical guides name {needle}");
    }
}

#[test]
fn pair_prints_a_temporary_code_without_a_lifetime_option() {
    let home = std::env::temp_dir().join(format!("kyotoagent-pair-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let root = home.join(".kyotoagent");
    let output = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["pair", "127.0.0.1:7841"])
        .env("HOME", &home)
        .env("KYOTOAGENT_ROOT", &root)
        .output()
        .expect("pair runs");
    assert!(output.status.success(), "{:?}", output.stderr);
    let stdout = String::from_utf8(output.stdout).expect("pair text");
    let uri = stdout.lines().last().expect("pairing URI");
    let (_, token) = kyotoagent::pairing::connection(uri).expect("valid pairing URI");
    let token = token.expect("credential");
    assert!(!kyotoagent::pairing::PairingKey::load(&root)
        .expect("private key reload")
        .accepts(&token));
    assert!(stdout.contains("10 minutes"));
    assert!(!root.join("config.toml").exists());
    let help = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["pair", "--help"])
        .output()
        .expect("pair help runs");
    assert!(help.status.success());
    assert!(!String::from_utf8(help.stdout).unwrap().contains("--days"));
    let obsolete = Command::new(env!("CARGO_BIN_EXE_kyotoagent"))
        .args(["pair", "127.0.0.1:7841", "--days", "30"])
        .env("HOME", &home)
        .env("KYOTOAGENT_ROOT", &root)
        .output()
        .expect("old option runs");
    assert!(!obsolete.status.success());
    assert!(String::from_utf8(obsolete.stderr)
        .unwrap()
        .contains("--days"));
    let _ = std::fs::remove_dir_all(home);
}
