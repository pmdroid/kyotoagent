use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn protected_directory() -> io::Result<Option<PathBuf>> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| io::Error::other("operator HOME is missing"))?;
    let directory = PathBuf::from(home).join(".kyotoagent");
    if !directory.exists() {
        return Ok(None);
    }
    Ok(Some(std::fs::canonicalize(directory)?))
}

fn writable_worktrees(directory: &Path) -> Option<PathBuf> {
    let worktrees = directory.join("worktrees");
    std::fs::symlink_metadata(&worktrees)
        .ok()
        .filter(|meta| meta.is_dir())
        .map(|_| worktrees)
}

pub(crate) fn check_write(path: &Path) -> io::Result<()> {
    let Some(directory) = protected_directory()? else {
        return Ok(());
    };
    let config_target = std::fs::canonicalize(directory.join("config.toml")).ok();
    let in_worktrees =
        writable_worktrees(&directory).is_some_and(|worktrees| path.starts_with(worktrees));
    if (path.starts_with(&directory) && !in_worktrees) || config_target.as_deref() == Some(path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "agent writes to operator configuration are blocked; use a temporary HOME",
        ));
    }
    Ok(())
}

pub(crate) fn command(program: &str, args: &[String], sandbox: bool) -> io::Result<Command> {
    if !sandbox {
        let mut command = Command::new(program);
        command.args(args);
        return Ok(command);
    }
    if !cfg!(target_os = "linux") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "sandbox = true requires Linux and Bubblewrap; set sandbox = false to run commands directly",
        ));
    }
    if !Path::new("/usr/bin/bwrap").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "sandbox = true requires Bubblewrap at /usr/bin/bwrap; install Bubblewrap or set sandbox = false",
        ));
    }
    let mut command = Command::new("/usr/bin/bwrap");
    command.args([
        "--bind",
        "/",
        "/",
        "--dev-bind",
        "/dev",
        "/dev",
        "--proc",
        "/proc",
    ]);
    if let Some(directory) = protected_directory()? {
        command.arg("--ro-bind").arg(&directory).arg(&directory);
        let worktrees = writable_worktrees(&directory);
        if let Some(worktrees) = &worktrees {
            command.arg("--bind").arg(worktrees).arg(worktrees);
        }
        if let Ok(target) = std::fs::canonicalize(directory.join("config.toml")) {
            if !target.starts_with(&directory)
                || worktrees.is_some_and(|worktrees| target.starts_with(worktrees))
            {
                command.arg("--ro-bind").arg(&target).arg(&target);
            }
        }
    }
    command.arg("--").arg(program).args(args);
    Ok(command)
}
