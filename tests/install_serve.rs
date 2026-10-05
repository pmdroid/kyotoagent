use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "kyotoagent-install-serve-{}-{n}-{tick}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("the scratch directory is created");
    Scratch(dir)
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).expect("the fake binary is written");
    let mut perms = fs::metadata(path)
        .expect("the fake binary has metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("the fake binary is executable");
}

fn bin_dir(scratch: &Path) -> PathBuf {
    let dir = scratch.join("bin");
    fs::create_dir_all(&dir).expect("the fake bin directory is created");
    for name in ["systemctl", "loginctl", "launchctl"] {
        executable(
            &dir.join(name),
            "#!/bin/sh\nif [ -n \"${KYOTOAGENT_INSTALL_LOG:-}\" ]; then printf '%s\\n' \"$0 $*\" >> \"$KYOTOAGENT_INSTALL_LOG\"; fi\necho \"install-serve called $0\" >&2\nexit 99\n",
        );
    }
    dir
}

fn fake_named(bin_dir: &Path, name: &str) -> PathBuf {
    let path = bin_dir.join(name);
    executable(&path, "#!/bin/sh\nexit 0\n");
    fs::canonicalize(&path).expect("the fake binary path resolves")
}

fn fake_kyotoagent(bin_dir: &Path) -> PathBuf {
    fake_named(bin_dir, "kyotoagent")
}

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/install-serve.sh")
}

fn run(home: &Path, path: &Path, os: &str, extra: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = Command::new("sh");
    command.arg(script());
    command.args(args);
    command.env("HOME", home);
    command.env("PATH", format!("{}:/usr/bin:/bin", path.display()));
    command.env("KYOTOAGENT_INSTALL_OS", os);
    command.env_remove("XDG_CONFIG_HOME");
    command.env_remove("DESTDIR");
    command.env_remove("KYOTOAGENT_INSTALL_EUID");
    for (key, value) in extra {
        command.env(key, value);
    }
    command.output().expect("install-serve.sh runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn linux_unit(home: &Path) -> PathBuf {
    home.join(".config/systemd/user/kyotoagent.service")
}

fn darwin_unit(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents/ai.kyotoagent.serve.plist")
}

#[test]
fn linux_dry_run_writes_execstart_with_the_fake_binary() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let binary = fake_kyotoagent(&bins);
    let output = run(&home, &bins, "Linux", &[], &["--print"]);
    assert!(
        output.status.success(),
        "linux --print succeeds: {}",
        text(&output.stderr)
    );
    let unit = fs::read_to_string(linux_unit(&home)).expect("the user unit is written");
    let exec = format!("ExecStart={} serve", binary.display());
    assert!(unit.contains(&exec), "{unit}");
    assert!(!unit.contains("--listen"), "{unit}");
    assert!(
        !linux_unit(&home)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .contains("pagent.service"),
        "the legacy service name is absent"
    );
    let printed = text(&output.stdout);
    assert!(printed.contains("kyotoagent.service"), "{printed}");
    assert!(!printed.contains("pagent.service"), "{printed}");
    assert!(unit.contains("Restart=on-failure"), "{unit}");
    assert!(!unit.contains("Restart=always"), "{unit}");
}

#[test]
fn darwin_dry_run_writes_the_binary_in_program_arguments() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let binary = fake_kyotoagent(&bins);
    let output = run(&home, &bins, "Darwin", &[], &["--print"]);
    assert!(
        output.status.success(),
        "darwin --print succeeds: {}",
        text(&output.stderr)
    );
    let unit = fs::read_to_string(darwin_unit(&home)).expect("the launch agent is written");
    let argument = format!("<string>{}</string>", binary.display());
    let arguments = unit
        .split("<key>ProgramArguments</key>")
        .nth(1)
        .unwrap_or("");
    assert!(arguments.contains(&argument), "{unit}");
    assert!(arguments.contains("<string>serve</string>"), "{unit}");
    assert!(!arguments.contains("--listen"), "{unit}");
    assert!(
        unit.contains("<string>ai.kyotoagent.serve</string>"),
        "{unit}"
    );
    assert!(!unit.contains("ai.pagent.serve"), "{unit}");
    assert!(unit.contains("<key>HOME</key>"), "{unit}");
    assert!(
        unit.contains(&format!("<string>{}</string>", home.display())),
        "{unit}"
    );
}

#[test]
fn uninstall_removes_the_unit_in_the_fake_home() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let _binary = fake_kyotoagent(&bins);
    for os in ["Linux", "Darwin"] {
        let output = run(&home, &bins, os, &[], &["--print"]);
        assert!(
            output.status.success(),
            "{os} install: {}",
            text(&output.stderr)
        );
        let output = run(&home, &bins, os, &[], &["--print", "--uninstall"]);
        assert!(
            output.status.success(),
            "{os} uninstall: {}",
            text(&output.stderr)
        );
        assert!(!linux_unit(&home).exists(), "{os} leaves the systemd unit");
        assert!(!darwin_unit(&home).exists(), "{os} leaves the launch agent");
    }
}

#[test]
fn the_script_exits_nonzero_as_root() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let _binary = fake_kyotoagent(&bins);
    let output = run(
        &home,
        &bins,
        "Linux",
        &[("KYOTOAGENT_INSTALL_EUID", "0")],
        &["--print"],
    );
    assert!(
        !output.status.success(),
        "root install exits 0: {}",
        text(&output.stdout)
    );
    assert!(!linux_unit(&home).exists(), "root install wrote a unit");
    let output = run(
        &home,
        &bins,
        "Linux",
        &[("KYOTOAGENT_INSTALL_EUID", "0")],
        &["--uninstall"],
    );
    assert!(!output.status.success(), "root uninstall exits 0");
}

#[test]
fn the_script_exits_nonzero_when_kyotoagent_is_missing() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let output = run(&home, &bins, "Linux", &[], &["--print"]);
    assert!(
        !output.status.success(),
        "a missing kyotoagent exits 0: {}",
        text(&output.stderr)
    );
    assert!(
        !linux_unit(&home).exists(),
        "a missing kyotoagent wrote a unit"
    );
}

#[test]
fn a_lone_kyoto_is_linked_beside_itself() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let binary = fake_named(&bins, "kyoto");
    let output = run(&home, &bins, "Linux", &[], &["--print"]);
    assert!(
        output.status.success(),
        "linux --print succeeds: {}",
        text(&output.stderr)
    );
    let link = bins.join("kyotoagent");
    assert!(link
        .symlink_metadata()
        .expect("kyotoagent is linked")
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read_link(&link).expect("the link target"),
        Path::new("kyoto")
    );
    let unit = fs::read_to_string(linux_unit(&home)).expect("the user unit is written");
    let exec = format!("ExecStart={} serve", binary.display());
    assert!(unit.contains(&exec), "{unit}");
    assert!(!unit.contains("--listen"), "{unit}");
}

#[test]
fn a_lone_kyotoagent_is_linked_as_kyoto() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let _binary = fake_named(&bins, "kyotoagent");
    let output = run(&home, &bins, "Linux", &[], &["--print"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let link = bins.join("kyoto");
    assert!(link
        .symlink_metadata()
        .expect("kyoto is linked")
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read_link(&link).expect("the link target"),
        Path::new("kyotoagent")
    );
}

#[test]
fn both_names_keep_kyotoagent_in_the_unit() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let agent = fake_named(&bins, "kyotoagent");
    let kyoto = fake_named(&bins, "kyoto");
    let output = run(&home, &bins, "Linux", &[], &["--print"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(!kyoto
        .symlink_metadata()
        .expect("kyoto")
        .file_type()
        .is_symlink());
    assert!(!agent
        .symlink_metadata()
        .expect("kyotoagent")
        .file_type()
        .is_symlink());
    let unit = fs::read_to_string(linux_unit(&home)).expect("the user unit is written");
    assert!(
        unit.contains(&format!("ExecStart={} ", agent.display())),
        "{unit}"
    );
    assert!(
        !unit.contains(&format!("ExecStart={} ", kyoto.display())),
        "{unit}"
    );
}

#[test]
fn uninstall_stops_the_new_unit_and_a_leftover_linux_service() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let _binary = fake_kyotoagent(&bins);
    let log = scratch.0.join("commands");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" >> '{}'\nexit 0\n",
        log.display()
    );
    executable(&bins.join("systemctl"), &body);
    let output = run(&home, &bins, "Linux", &[], &["--print"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let output = run(&home, &bins, "Linux", &[], &["--uninstall"]);
    assert!(
        output.status.success(),
        "uninstall: {}",
        text(&output.stderr)
    );
    assert!(
        !linux_unit(&home).exists(),
        "uninstall leaves kyotoagent.service"
    );
    let recorded = fs::read_to_string(&log).expect("systemctl was recorded");
    assert!(
        recorded.contains("disable --now kyotoagent.service"),
        "{recorded}"
    );
    assert!(
        recorded.contains("disable --now kyotoagent.service"),
        "{recorded}"
    );
}

#[test]
fn uninstall_stops_a_leftover_launch_agent() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let _binary = fake_kyotoagent(&bins);
    let log = scratch.0.join("commands");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" >> '{}'\nexit 0\n",
        log.display()
    );
    executable(&bins.join("launchctl"), &body);
    let output = run(&home, &bins, "Darwin", &[], &["--print"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let output = run(&home, &bins, "Darwin", &[], &["--uninstall"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(
        !darwin_unit(&home).exists(),
        "uninstall leaves the new plist"
    );
    let recorded = fs::read_to_string(&log).expect("launchctl was recorded");
    assert!(recorded.contains("ai.pagent.serve"), "{recorded}");
}

#[test]
fn getting_started_names_install_serve() {
    let text = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/getting-started.md"),
    )
    .expect("getting started reads");
    let install = text
        .split("## Install")
        .nth(1)
        .expect("getting started has an Install section")
        .split("## ")
        .next()
        .expect("the Install section ends");
    assert!(install.contains("scripts/install-serve.sh"), "{install}");
    assert!(install.contains("--uninstall"), "{install}");
    assert!(
        install
            .find("cargo install --path .")
            .expect("the install command is there")
            < install
                .find("scripts/install-serve.sh")
                .expect("the service script is there"),
        "install-serve.sh follows cargo install"
    );
}

fn assert_not_started(log: &Path, output: &Output) {
    assert!(
        output.status.success(),
        "--print exits: {}",
        text(&output.stderr)
    );
    assert!(
        !text(&output.stderr).contains("install-serve called"),
        "--print started the service: {}",
        text(&output.stderr)
    );
    assert!(!log.exists(), "--print ran a service command");
}

#[test]
fn linux_print_listen_writes_the_address_on_execstart() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let binary = fake_kyotoagent(&bins);
    let log = scratch.0.join("calls.log");
    let output = run(
        &home,
        &bins,
        "Linux",
        &[("KYOTOAGENT_INSTALL_LOG", log.to_str().expect("log path"))],
        &["--print", "--listen", "0.0.0.0:7841"],
    );
    assert_not_started(&log, &output);
    let unit = fs::read_to_string(linux_unit(&home)).expect("the user unit is written");
    let exec = format!("ExecStart={} serve --listen 0.0.0.0:7841", binary.display());
    assert!(unit.contains(&exec), "{unit}");
}

#[test]
fn darwin_print_listen_writes_two_arguments() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let binary = fake_kyotoagent(&bins);
    let log = scratch.0.join("calls.log");
    let output = run(
        &home,
        &bins,
        "Darwin",
        &[("KYOTOAGENT_INSTALL_LOG", log.to_str().expect("log path"))],
        &["--print", "--listen", "0.0.0.0:7841"],
    );
    assert_not_started(&log, &output);
    let unit = fs::read_to_string(darwin_unit(&home)).expect("the launch agent is written");
    let arguments = unit
        .split("<key>ProgramArguments</key>")
        .nth(1)
        .unwrap_or("");
    let binary_arg = format!("<string>{}</string>", binary.display());
    assert!(arguments.contains(&binary_arg), "{unit}");
    let serve_at = arguments
        .find("<string>serve</string>")
        .expect("serve stays an argument");
    let rest = &arguments[serve_at..];
    let flag_at = rest
        .find("<string>--listen</string>")
        .expect("listen is its own argument");
    let address_at = rest
        .find("<string>0.0.0.0:7841</string>")
        .expect("the address is its own argument");
    assert!(flag_at < address_at, "{unit}");
}

#[test]
fn darwin_listen_uses_the_same_xml_escape_as_the_binary() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let _binary = fake_kyotoagent(&bins);
    let output = run(
        &home,
        &bins,
        "Darwin",
        &[],
        &["--print", "--listen", "a<b&c>"],
    );
    assert!(
        output.status.success(),
        "darwin --listen escapes: {}",
        text(&output.stderr)
    );
    let unit = fs::read_to_string(darwin_unit(&home)).expect("the launch agent is written");
    assert!(unit.contains("<string>a&lt;b&amp;c&gt;</string>"), "{unit}");
    let linux = run(
        &home,
        &bins,
        "Linux",
        &[],
        &["--print", "--listen", "a<b&c>"],
    );
    assert!(linux.status.success(), "{}", text(&linux.stderr));
    let service = fs::read_to_string(linux_unit(&home)).expect("the user unit is written");
    assert!(service.contains("serve --listen a<b&c>"), "{service}");
}

#[test]
fn listen_without_an_address_exits_2() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let _binary = fake_kyotoagent(&bins);
    for args in [
        vec!["--listen"],
        vec!["--print", "--listen"],
        vec!["--listen", ""],
    ] {
        let output = run(&home, &bins, "Linux", &[], &args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "args {args:?}: {}",
            text(&output.stderr)
        );
        assert!(!linux_unit(&home).exists(), "args {args:?} wrote a unit");
    }
}

#[test]
fn uninstall_ignores_a_listen_argument() {
    let scratch = scratch();
    let bins = bin_dir(&scratch.0);
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).expect("the fake home is created");
    let _binary = fake_kyotoagent(&bins);
    for os in ["Linux", "Darwin"] {
        let output = run(
            &home,
            &bins,
            os,
            &[],
            &["--print", "--listen", "0.0.0.0:7841"],
        );
        assert!(
            output.status.success(),
            "{os} install: {}",
            text(&output.stderr)
        );
        let output = run(
            &home,
            &bins,
            os,
            &[],
            &["--uninstall", "--listen", "0.0.0.0:7841", "--print"],
        );
        assert!(
            output.status.success(),
            "{os} uninstall: {}",
            text(&output.stderr)
        );
        assert!(!linux_unit(&home).exists(), "{os} leaves the systemd unit");
        assert!(!darwin_unit(&home).exists(), "{os} leaves the launch agent");
    }
}

#[test]
fn getting_started_names_listen_beside_the_serve_example() {
    let text = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/getting-started.md"),
    )
    .expect("getting started reads");
    let example_at = text
        .find("kyotoagent serve --listen 0.0.0.0:7841")
        .expect("the serve --listen example");
    let section = text[example_at..]
        .split("## ")
        .next()
        .expect("the example section ends");
    assert!(
        section.contains("scripts/install-serve.sh --listen"),
        "{section}"
    );
}
