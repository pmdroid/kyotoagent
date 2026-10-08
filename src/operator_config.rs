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

pub(crate) fn check_write(path: &Path) -> io::Result<()> {
    let Some(directory) = protected_directory()? else {
        return Ok(());
    };
    if path.starts_with(&directory) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "agent writes to operator configuration are blocked; use a temporary HOME",
        ));
    }
    let config = directory.join("config.toml");
    if let Ok(config) = std::fs::canonicalize(config) {
        if path == config {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "agent writes to operator configuration are blocked; use a temporary HOME",
            ));
        }
    }
    Ok(())
}

pub(crate) fn command(program: &str, args: &[String]) -> io::Result<Command> {
    let Some(directory) = protected_directory()? else {
        let mut command = Command::new(program);
        command.args(args);
        return Ok(command);
    };
    let mut command = Command::new("/usr/bin/bwrap");
    command
        .args([
            "--bind",
            "/",
            "/",
            "--dev-bind",
            "/dev",
            "/dev",
            "--proc",
            "/proc",
            "--ro-bind",
        ])
        .arg(&directory)
        .arg(&directory);
    let config = directory.join("config.toml");
    if let Ok(target) = std::fs::canonicalize(&config) {
        if !target.starts_with(&directory) {
            command.arg("--ro-bind").arg(&target).arg(&target);
        }
    }
    command.arg("--").arg(program).args(args);
    Ok(command)
}
