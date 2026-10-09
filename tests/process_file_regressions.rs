use kyotoagent::permit::Answer;
use kyotoagent::{
    session::{Session, SessionMeta},
    tools::Tools,
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    session: Session,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("kyoto-file-process-{}-{name}", std::process::id()));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let session = Session::at(&root.join("session"));
        session
            .create(&SessionMeta::new(
                "s",
                &workspace,
                "local-model",
                "2026-10-03T00:00:00.000Z",
            ))
            .unwrap();
        Self {
            root,
            workspace,
            session,
        }
    }

    fn evidence(&self, value: Value) {
        fs::write(
            self.root.join("observed.json"),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

async fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(Instant::now() < deadline, "condition did not become true");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn write_preserves_executable_mode() {
    let f = Fixture::new("write-mode");
    let file = f.workspace.join("script");
    fs::write(&file, "exit 0\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    let tools = Tools::at(&f.session).unwrap();
    tools.gate().queue(Answer::allow_once());
    tools.write_file("t1", "script", "exit 1\n").unwrap();
    let mode = fs::metadata(file).unwrap().permissions().mode() & 0o777;
    f.evidence(json!({"before":"755","after":format!("{mode:o}")}));
    assert_eq!(mode, 0o755, "editing an executable makes it non-executable");
}

#[test]
fn reads_and_listings_preserve_parent_directory_components() {
    let f = Fixture::new("read-parent-components");
    fs::create_dir_all(f.workspace.join("sub")).unwrap();
    fs::create_dir_all(f.workspace.join("entries")).unwrap();
    fs::write(f.workspace.join("expected.txt"), "expected\n").unwrap();
    fs::write(f.workspace.join("sub/expected.txt"), "wrong\n").unwrap();
    fs::write(f.workspace.join("entries/visible.txt"), "visible\n").unwrap();
    fs::write(f.workspace.join("sub/entries"), "not a directory\n").unwrap();
    fs::write(f.root.join("outside.txt"), "approved\n").unwrap();
    fs::write(f.workspace.join("outside.txt"), "unapproved\n").unwrap();
    let tools = Tools::at(&f.session).unwrap();
    for path in ["sub/../expected.txt", "./sub/../../workspace/expected.txt"] {
        let read = tools.read_file("t1", path, None, None, None).unwrap();
        assert_eq!(read.text, "1→expected");
    }
    let listing = tools.list_dir("t1", "sub/../entries").unwrap();
    assert!(listing.summary().contains("visible.txt"));
    assert!(tools
        .read_file("t1", "../outside.txt", None, None, None)
        .is_err());
    fs::create_dir_all(f.root.join("alternate")).unwrap();
    fs::write(f.root.join("alternate/outside.txt"), "unapproved\n").unwrap();
    tools.gate().queue(Answer::allow_once());
    let read = tools
        .read_file(
            "t1",
            f.root.join("alternate/../outside.txt").to_str().unwrap(),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(read.text, "1→approved");
}

#[test]
fn outside_read_rejects_a_symlink_after_approval() {
    let f = Fixture::new("read-symlink");
    let approved = f.root.join("approved.txt");
    let secret = f.root.join("secret.txt");
    fs::write(&approved, "approved\n").unwrap();
    fs::write(&secret, "secret\n").unwrap();
    let tools = Tools::at(&f.session).unwrap();
    tools.gate().queue(Answer::allow_once());
    let _ = tools
        .read_file("t0", approved.to_str().unwrap(), None, None, None)
        .unwrap();
    tools.gate().queue(Answer::allow_once());
    fs::remove_file(&approved).unwrap();
    std::os::unix::fs::symlink(&secret, &approved).unwrap();
    let error = tools
        .read_file("t1", approved.to_str().unwrap(), None, None, None)
        .unwrap_err();
    f.evidence(
        json!({"error":error.to_string(),"secret_leaked":error.to_string().contains("secret")}),
    );
    assert!(!error.to_string().contains("secret"));
}

#[tokio::test]
async fn missing_write_contents_does_not_erase_an_approved_file() {
    let f = Fixture::new("missing-contents");
    let file = f.workspace.join("data.txt");
    fs::write(&file, "kept\n").unwrap();
    f.session
        .update(|meta| {
            meta.allow
                .remember(Some(file.to_str().unwrap()), None, None);
            true
        })
        .unwrap();
    let missing = serde_json::json!({"path":"data.txt"});
    let error = kyotoagent::tools::Tools::required_string(&missing, "contents").unwrap_err();
    assert_eq!(fs::read_to_string(file).unwrap(), "kept\n");
    assert!(error.to_string().contains("contents"));
}

#[tokio::test]
async fn reading_a_fifo_returns_without_a_writer() {
    let f = Fixture::new("fifo-read");
    let pipe = f.workspace.join("pipe");
    let name = std::ffi::CString::new(pipe.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let tools = Tools::at(&f.session).unwrap();
    let started = Instant::now();
    let error = tokio::task::spawn_blocking(move || {
        tools.read_file("t1", "pipe", None, None, None).unwrap_err()
    })
    .await
    .unwrap();
    f.evidence(json!({"elapsed_millis":started.elapsed().as_millis(),"error":error.to_string()}));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn background_task_kills_descendants() {
    let f = Fixture::new("task-descendant");
    let tools = Tools::at(&f.session).unwrap();
    let argv = vec![
        "sh".into(),
        "-c".into(),
        "sleep 20 & echo $! > child.pid; wait".into(),
    ];
    let started = tools.tasks().start("t1", &argv, None).await.unwrap();
    let pid = child_pid(&f.workspace.join("child.pid")).await;
    tools.tasks().cancel_id(&started.id);
    wait(|| !tools.tasks().is_running(&started.id)).await;
    let alive = process_alive(pid);
    let report = tools.tasks().check(&started.id).unwrap();
    f.evidence(json!({"descendant_alive_after_cancel":alive,"task_report":report.summary()}));
    unsafe { libc::kill(pid, libc::SIGKILL) };
    assert!(
        !alive,
        "a background task's descendant survives cancellation"
    );
}

#[tokio::test]
async fn permission_revalidates_directory_after_answer() {
    let f = Fixture::new("swapped-directory");
    let inside = f.workspace.join("dir");
    let outside = f.root.join("outside");
    fs::create_dir_all(&inside).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(inside.join("file"), "same old bytes").unwrap();
    fs::write(outside.join("file"), "same old bytes").unwrap();
    let tools = Tools::at(&f.session).unwrap();
    let worker_tools = tools.clone();
    let writer = tokio::task::spawn_blocking(move || {
        worker_tools.write_file("t1", "dir/file", "new contents")
    });
    wait(|| tools.gate().open_permission().is_some()).await;
    fs::rename(&inside, f.workspace.join("original-dir")).unwrap();
    std::os::unix::fs::symlink(&outside, &inside).unwrap();
    tools.gate().answer(Answer::allow_once()).unwrap();
    let result = writer.await.unwrap();
    let observed = fs::read_to_string(outside.join("file")).unwrap();
    f.evidence(
        json!({"outside_file_after_approval":observed,"write_result":format!("{result:?}")}),
    );
    assert_eq!(
        observed, "same old bytes",
        "approving an inside path writes through a swapped directory to an outside file"
    );
}

#[tokio::test]
async fn signalled_background_task_is_not_exit_zero() {
    let f = Fixture::new("signal-exit");
    let tools = Tools::at(&f.session).unwrap();
    let started = tools
        .tasks()
        .start(
            "t1",
            &["sh".into(), "-c".into(), "kill -TERM $$".into()],
            None,
        )
        .await
        .unwrap();
    wait(|| !tools.tasks().is_running(&started.id)).await;
    let report = tools.tasks().check(&started.id).unwrap();
    f.evidence(json!({"task_report":report.summary()}));
    assert_ne!(
        report.exit,
        Some(0),
        "a process terminated by signal is reported as successful"
    );
}

async fn child_pid(path: &Path) -> i32 {
    wait(|| {
        fs::read_to_string(path).is_ok_and(|text| {
            text.ends_with('\n') && text.trim().parse::<i32>().is_ok_and(|pid| pid > 0)
        })
    })
    .await;
    fs::read_to_string(path).unwrap().trim().parse().unwrap()
}

fn process_alive(pid: i32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&output.stdout)
        .trim()
        .chars()
        .next();
    state.is_some() && state != Some('Z')
}

#[tokio::test]
async fn cancellation_kills_a_stubborn_descendant_after_its_parent_exits() {
    let f = Fixture::new("stubborn-descendant");
    let tools = Tools::at(&f.session).unwrap();
    let argv = vec![
        "sh".into(),
        "-c".into(),
        "sh -c 'trap \"\" TERM; echo $$ > child.pid; exec sleep 20' & wait".into(),
    ];
    let started = tools.tasks().start("t1", &argv, None).await.unwrap();
    let pid = child_pid(&f.workspace.join("child.pid")).await;
    tools.tasks().cancel_id(&started.id);
    wait(|| !tools.tasks().is_running(&started.id)).await;
    let alive = process_alive(pid);
    unsafe { libc::kill(pid, libc::SIGKILL) };
    assert!(!alive);
}

#[tokio::test]
async fn timeout_kills_background_descendants() {
    let f = Fixture::new("timeout-descendant");
    let tools = Tools::at(&f.session).unwrap();
    let argv = vec![
        "sh".into(),
        "-c".into(),
        "sleep 20 & echo $! > child.pid; wait".into(),
    ];
    let started = tools.tasks().start("t1", &argv, Some(1)).await.unwrap();
    let pid = child_pid(&f.workspace.join("child.pid")).await;
    wait(|| !tools.tasks().is_running(&started.id)).await;
    let alive = process_alive(pid);
    unsafe { libc::kill(pid, libc::SIGKILL) };
    assert!(!alive);
    assert_ne!(tools.tasks().check(&started.id).unwrap().exit, Some(0));
}
