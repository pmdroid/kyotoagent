use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn lock(path: &Path) -> Result<Option<File>, std::io::Error> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(error)
    }
}

pub(super) fn serving_lock(root: &Path) -> Result<File, super::ServerError> {
    lock(&root.join("serve.lock"))
        .map_err(super::ServerError::RootIo)?
        .ok_or(super::ServerError::AlreadyServing)
}

pub async fn ensure(socket: &Path) -> Result<(), String> {
    if super::socket_is_live(socket) {
        return Ok(());
    }
    let root = socket.parent().ok_or("local server directory is missing")?;
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(8);
    let _starting = loop {
        if super::socket_is_live(socket) {
            return Ok(());
        }
        if let Some(file) = lock(&root.join("startup.lock")).map_err(|error| error.to_string())? {
            break file;
        }
        if Instant::now() >= deadline {
            return Err("Local server startup timed out.".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    if super::socket_is_live(socket) {
        return Ok(());
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(root.join("serve.log"))
        .map_err(|error| error.to_string())?;
    let mut command = Command::new(std::env::current_exe().map_err(|error| error.to_string())?);
    command
        .arg("serve")
        .env("KYOTOAGENT_ROOT", root)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot start local server: {error}"))?;
    loop {
        if super::socket_is_live(socket) {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!(
                "Local server exited with {status}. See {}.",
                root.join("serve.log").display()
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Local server startup timed out. See {}.",
                root.join("serve.log").display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
