use super::*;
use crate::proof::ProofFile;
use ring::digest::{Context, SHA256};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) enum Popup {
    Preview(ProofFile),
    Actions {
        file: ProofFile,
        highlight: usize,
    },
    Save {
        file: ProofFile,
        path: String,
        overwrite: bool,
    },
    Loading(String),
    Error(String),
}

pub(super) struct Job {
    session: String,
    server: Option<String>,
    cancelled: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<Result<Completed, String>>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.task.abort();
    }
}

enum Completed {
    Open(ProofFile, PathBuf),
    Saved(PathBuf),
    External(PathBuf),
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(super) fn open(app: &mut App, client: &Client, file: ProofFile) {
    app.proof_popup = None;
    app.open_text = None;
    app.open_image = None;
    app.open_file = None;
    app.context_open = false;
    app.thinking_open = false;
    let preview = previewable(&file);
    app.proof_file_popup = Some(Popup::Actions { highlight: 1, file });
    app.overlay = true;
    if preview {
        choose(app, client, 0);
    }
}

pub(super) fn close(app: &mut App) {
    app.proof_file_job = None;
    app.open_text = None;
    app.open_image = None;
    app.proof_file_popup = None;
    app.overlay = false;
}

fn previewable(file: &ProofFile) -> bool {
    matches!(
        file.media_type.as_str(),
        "text/plain"
            | "text/markdown"
            | "application/json"
            | "text/csv"
            | "image/png"
            | "image/jpeg"
            | "image/webp"
    ) && file.size <= 5 * 1024 * 1024
}

fn actions(file: &ProofFile) -> Vec<Choice> {
    let mut choices = vec![choice_row("Cancel"), choice_row("Download")];
    if file.git_sha.is_some() {
        choices.push(choice_row("Copy Git SHA"));
    }
    choices.push(choice_row("Open externally"));
    choices.push(choice_row("Save a copy…"));
    choices
}

pub(super) fn overlay(app: &App) -> Option<Overlay> {
    Some(match app.proof_file_popup.as_ref()? {
        Popup::Preview(file) => {
            return app.open_text.as_ref().map(|text| Overlay::Text {
                text: format!(
                    "{} · Ctrl-S download · Ctrl-O open externally · Ctrl-Shift-S save a copy{} · Esc close{}\n\n{text}",
                    file.name,
                    if file.git_sha.is_some() {
                        " · Ctrl-G copy Git SHA"
                    } else {
                        ""
                    },
                    file.git_sha
                        .as_ref()
                        .map(|sha| format!("\nGit SHA: {sha}"))
                        .unwrap_or_default()
                ),
            });
        }
        Popup::Actions { file, highlight } => Overlay::Question {
            text: format!(
                "Download {}?\n{} · {} bytes{}",
                file.name,
                file.media_type,
                file.size,
                file.git_sha
                    .as_ref()
                    .map(|sha| format!("\nGit SHA: {sha}"))
                    .unwrap_or_default()
            ),
            choices: actions(file)
                .into_iter()
                .enumerate()
                .map(|(index, mut choice)| {
                    choice.marked = index == *highlight;
                    choice
                })
                .collect(),
            prompt: String::new(),
        },
        Popup::Save {
            path, overwrite, ..
        } => Overlay::Question {
            text: if *overwrite {
                format!("Replace existing file?\n{path}")
            } else {
                "Save artifact to a local path · Enter save · Esc cancel".into()
            },
            choices: Vec::new(),
            prompt: path.clone(),
        },
        Popup::Loading(name) => Overlay::Text {
            text: format!("Downloading {name}…\nEsc cancels"),
        },
        Popup::Error(error) => Overlay::Text {
            text: format!("Artifact: {error}\nEsc closes"),
        },
    })
}

pub(super) fn key(app: &App, event: KeyEvent) -> Option<Effect> {
    if event.code == KeyCode::Esc {
        return Some(Effect::CloseOverlay);
    }
    if event.code == KeyCode::Char('c') && event.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(Effect::Exit);
    }
    match app.proof_file_popup.as_ref()? {
        Popup::Preview(file) => match event.code {
            KeyCode::Char('g')
                if event.modifiers.contains(KeyModifiers::CONTROL) && file.git_sha.is_some() =>
            {
                Some(Effect::ProofFileChoose(2))
            }
            KeyCode::Char('o') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Effect::ProofFileChoose(if file.git_sha.is_some() {
                    3
                } else {
                    2
                }))
            }
            KeyCode::Char('s' | 'S')
                if event
                    .modifiers
                    .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) =>
            {
                Some(Effect::ProofFileChoose(if file.git_sha.is_some() {
                    4
                } else {
                    3
                }))
            }
            KeyCode::Char('s') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Effect::ProofFileChoose(1))
            }
            KeyCode::Up => Some(Effect::ScrollUp),
            KeyCode::Down => Some(Effect::ScrollDown),
            KeyCode::PageUp => Some(Effect::PageUp),
            KeyCode::PageDown => Some(Effect::PageDown),
            _ => None,
        },
        Popup::Actions { highlight, .. } => match event.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                Some(Effect::ProofFileMove)
            }
            KeyCode::Enter => Some(Effect::ProofFileChoose(*highlight)),
            _ => None,
        },
        Popup::Save { overwrite, .. } => match event.code {
            KeyCode::Enter => Some(Effect::ProofFileChoose(1)),
            KeyCode::Backspace if !overwrite => Some(Effect::Backspace),
            KeyCode::Char('u') if !overwrite && event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Effect::DeleteLine)
            }
            KeyCode::Char(character)
                if !overwrite
                    && !character.is_control()
                    && !event.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                Some(Effect::Type(character))
            }
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn handle(app: &mut App, client: &Client, effect: &Effect) -> bool {
    if app.proof_file_popup.is_none() {
        return false;
    }
    match effect {
        Effect::ProofFileMove => {
            if let Some(Popup::Actions { highlight, file }) = &mut app.proof_file_popup {
                *highlight = (*highlight + 1) % actions(file).len();
            }
        }
        Effect::ProofFileChoose(index) | Effect::Choose(index) => choose(app, client, *index),
        Effect::Type(character) => {
            if let Some(Popup::Save {
                path,
                overwrite: false,
                ..
            }) = &mut app.proof_file_popup
            {
                path.push(*character);
            }
        }
        Effect::Backspace => {
            if let Some(Popup::Save {
                path,
                overwrite: false,
                ..
            }) = &mut app.proof_file_popup
            {
                path.pop();
            }
        }
        Effect::DeleteLine => {
            if let Some(Popup::Save {
                path,
                overwrite: false,
                ..
            }) = &mut app.proof_file_popup
            {
                path.clear();
            }
        }
        Effect::Paste(text) => {
            if let Some(Popup::Save {
                path,
                overwrite: false,
                ..
            }) = &mut app.proof_file_popup
            {
                path.push_str(text);
            }
        }
        Effect::CloseOverlay => close(app),
        Effect::Exit => return false,
        _ if matches!(app.proof_file_popup, Some(Popup::Preview(_))) => return false,
        _ => {}
    }
    true
}

fn choose(app: &mut App, client: &Client, index: usize) {
    let has_sha = match app.proof_file_popup.as_ref() {
        Some(Popup::Actions { file, .. } | Popup::Preview(file)) => file.git_sha.is_some(),
        _ => false,
    };
    if index == 2 && has_sha {
        if let Some(Popup::Actions { file, .. } | Popup::Preview(file)) = &app.proof_file_popup {
            if let Some(sha) = file.git_sha.clone() {
                commit_copy(app, &sha);
            }
        }
        return;
    }
    let external = index == if has_sha { 3 } else { 2 };
    let save_as = index == if has_sha { 4 } else { 3 };
    let download_only = index == 1 && !matches!(app.proof_file_popup, Some(Popup::Save { .. }));
    let (file, save) = match app.proof_file_popup.as_ref() {
        Some(Popup::Actions { file, .. } | Popup::Preview(file)) if save_as => {
            app.open_text = None;
            app.open_image = None;
            app.proof_file_popup = Some(Popup::Save {
                file: file.clone(),
                path: app.home.join(&file.name).to_string_lossy().into_owned(),
                overwrite: false,
            });
            return;
        }
        Some(Popup::Actions { file, .. } | Popup::Preview(file)) if download_only || external => {
            (file.clone(), None)
        }
        Some(Popup::Actions { file, .. }) if index == 0 => {
            if !previewable(file) {
                close(app);
                return;
            }
            (file.clone(), None)
        }
        Some(Popup::Save {
            file,
            path,
            overwrite,
        }) => {
            let destination = if let Some(relative) = path.strip_prefix("~/") {
                app.home.join(relative)
            } else {
                PathBuf::from(path)
            };
            if !destination.is_absolute() || destination.file_name().is_none() {
                app.proof_file_popup =
                    Some(Popup::Error("Enter an absolute local file path".into()));
                return;
            }
            if fs::symlink_metadata(&destination).is_ok() && !overwrite {
                app.proof_file_popup = Some(Popup::Save {
                    file: file.clone(),
                    path: path.clone(),
                    overwrite: true,
                });
                return;
            }
            (file.clone(), Some((destination, *overwrite)))
        }
        _ => return,
    };
    let request = client.clone();
    let session = app.selected.clone();
    let selected = session.clone();
    let cache = app.home.join(".kyotoagent/downloads");
    app.open_text = None;
    app.open_image = None;
    app.proof_file_popup = Some(Popup::Loading(file.name.clone()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_save = Arc::clone(&cancelled);
    app.proof_file_job = Some(Job {
        cancelled,
        session,
        server: app.server.clone(),
        task: tokio::spawn(async move {
            let path = tokio::time::timeout(
                Duration::from_secs(300),
                download(&request, &selected, &file, &cache),
            )
            .await
            .map_err(|_| "artifact download timed out")??;
            if let Some((destination, overwrite)) = save {
                let output = destination.clone();
                tokio::task::spawn_blocking(move || {
                    save_copy(&path, &destination, overwrite, &cancel_save)
                })
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
                Ok(Completed::Saved(output))
            } else if external {
                Ok(Completed::External(path))
            } else if download_only {
                Ok(Completed::Saved(path))
            } else {
                Ok(Completed::Open(file, path))
            }
        }),
    });
}

pub(super) async fn advance(app: &mut App) {
    if app
        .proof_file_job
        .as_ref()
        .is_some_and(|job| job.session != app.selected || job.server != app.server)
    {
        close(app);
    }
    if !app
        .proof_file_job
        .as_ref()
        .is_some_and(|job| job.task.is_finished())
    {
        return;
    }
    let mut job = app.proof_file_job.take().unwrap();
    let result = (&mut job.task).await;
    app.proof_file_popup = None;
    app.overlay = false;
    match result {
        Ok(Ok(Completed::Saved(path))) => app.notice = Some(format!("Saved {}", path.display())),
        Ok(Ok(Completed::External(path))) => match open_file(&path) {
            Ok(()) => app.notice = Some(format!("Opened {}", path.display())),
            Err(error) => {
                app.proof_file_popup = Some(Popup::Error(format!(
                    "Saved {} but could not open it: {error}",
                    path.display()
                )));
                app.overlay = true;
            }
        },
        Ok(Ok(Completed::Open(file, path))) => {
            if previewable(&file) {
                match tokio::fs::read(&path).await {
                    Ok(bytes) if file.media_type.starts_with("image/") => {
                        match crate::attachment::ImageAttachment::from_bytes(&file.name, &bytes) {
                            Ok(mut image) => {
                                image.name = format!("{} · Ctrl-S download · Ctrl-O open externally · Ctrl-Shift-S save a copy · Esc close", file.name);
                                app.open_image = Some(image);
                                app.overlay = true;
                            }
                            Err(error) => app.notice = Some(error.to_string()),
                        }
                    }
                    Ok(bytes) => match String::from_utf8(bytes) {
                        Ok(text) => {
                            app.open_text = Some(text);
                            app.file_scroll = 0;
                            app.overlay = true;
                        }
                        Err(error) => app.notice = Some(error.to_string()),
                    },
                    Err(error) => app.notice = Some(error.to_string()),
                }
            }
            if app.open_text.is_some() || app.open_image.is_some() {
                app.proof_file_popup = Some(Popup::Preview(file));
            } else {
                app.proof_file_popup = Some(Popup::Actions { file, highlight: 1 });
                app.overlay = true;
            }
        }
        Ok(Err(error)) => {
            app.proof_file_popup = Some(Popup::Error(error));
            app.overlay = true;
        }
        Err(error) => app.notice = Some(error.to_string()),
    }
}

fn valid_file(file: &ProofFile) -> bool {
    file.id.len() == 32
        && file
            .id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && !file.name.is_empty()
        && file.name.len() <= 255
        && file.name != "."
        && file.name != ".."
        && !file.name.contains(['/', '\\'])
        && !file.name.chars().any(char::is_control)
        && file.sha256.len() == 64
        && file
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn download(
    client: &Client,
    session: &str,
    file: &ProofFile,
    cache: &Path,
) -> Result<PathBuf, String> {
    if !valid_file(file) {
        return Err("invalid artifact file metadata".into());
    }
    let directory = cache.join(&file.id);
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    fs::set_permissions(cache, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let temporary_path = directory.join(format!(
        ".download-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary_path)
        .map_err(|error| error.to_string())?;
    let temporary = Temporary(temporary_path);
    let mut output = tokio::fs::File::from_std(target);
    let path = format!("/v1/sessions/{session}/artifacts/files/{}", file.id);
    let mut hash = Context::new(&SHA256);
    let mut size = 0u64;
    match &client.transport {
        Transport::Url { base, http } => {
            let mut response = http
                .get(format!("{base}{path}"))
                .timeout(Duration::from_secs(300))
                .send()
                .await
                .map_err(|error| error.to_string())?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(format!("download failed: HTTP {}", response.status()));
            }
            while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
                write_chunk(&mut output, &mut hash, &mut size, file.size, &chunk).await?;
            }
        }
        Transport::Socket(socket) => {
            let mut stream = UnixStream::connect(socket)
                .await
                .map_err(|error| error.to_string())?;
            stream
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nhost: kyotoagent\r\nconnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .map_err(|error| error.to_string())?;
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                if header.len() >= 16384 {
                    return Err("invalid download headers".into());
                }
                header.push(stream.read_u8().await.map_err(|error| error.to_string())?);
            }
            let header = String::from_utf8(header).map_err(|error| error.to_string())?;
            if header.split_whitespace().nth(1) != Some("200") {
                return Err("artifact download refused".into());
            }
            let length = header
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<u64>().ok())
                })
                .ok_or("missing download size")?;
            if length != file.size {
                return Err("artifact size changed".into());
            }
            let mut chunk = [0u8; 65536];
            while size < length {
                let limit = usize::try_from((length - size).min(chunk.len() as u64)).unwrap();
                let count = stream
                    .read(&mut chunk[..limit])
                    .await
                    .map_err(|error| error.to_string())?;
                if count == 0 {
                    return Err("incomplete artifact download".into());
                }
                write_chunk(&mut output, &mut hash, &mut size, length, &chunk[..count]).await?;
            }
        }
    }
    let digest = hash
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if size != file.size || digest != file.sha256 {
        return Err("artifact file integrity check failed".into());
    }
    output.sync_all().await.map_err(|error| error.to_string())?;
    drop(output);
    let path = directory.join(&file.name);
    fs::rename(&temporary.0, &path).map_err(|error| error.to_string())?;
    Ok(path)
}

async fn write_chunk(
    output: &mut tokio::fs::File,
    hash: &mut Context,
    size: &mut u64,
    expected: u64,
    chunk: &[u8],
) -> Result<(), String> {
    *size = size
        .checked_add(chunk.len() as u64)
        .ok_or("artifact file too large")?;
    if *size > expected {
        return Err("artifact file exceeds recorded size".into());
    }
    output
        .write_all(chunk)
        .await
        .map_err(|error| error.to_string())?;
    hash.update(chunk);
    Ok(())
}

fn save_copy(
    source: &Path,
    destination: &Path,
    overwrite: bool,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let temporary_path = destination.with_file_name(format!(
        ".kyoto-proof-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary_path)?;
    let temporary = Temporary(temporary_path);
    let mut input = File::open(source)?;
    let mut chunk = [0u8; 65536];
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "save cancelled"));
        }
        let count = std::io::Read::read(&mut input, &mut chunk)?;
        if count == 0 {
            break;
        }
        output.write_all(&chunk[..count])?;
    }
    output.sync_all()?;
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "save cancelled"));
    }
    if overwrite {
        fs::rename(&temporary.0, destination)?;
    } else {
        fs::hard_link(&temporary.0, destination)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventKind, ProofBody};
    use crate::session::{Session, SessionMeta};

    fn file(bytes: &[u8]) -> ProofFile {
        ProofFile {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "report.md".into(),
            media_type: "text/markdown".into(),
            size: bytes.len() as u64,
            git_sha: None,
            sha256: ring::digest::digest(&SHA256, bytes)
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }

    #[tokio::test]
    async fn artifact_actions_show_and_copy_the_full_commit_sha() {
        let _capture = capture_copy();
        let mut artifact = file(b"report");
        artifact.git_sha = Some("b".repeat(40));
        let mut app = App::new("/w".into(), "/home/u".into(), "session".into());
        app.ask = "Unsent draft".into();
        let client = Client::at("/unused".into());
        app.proof_file_popup = Some(Popup::Actions {
            file: artifact.clone(),
            highlight: 1,
        });
        let Some(Overlay::Question { text, choices, .. }) = overlay(&app) else {
            panic!("attachment actions")
        };
        assert!(text.contains(&"b".repeat(40)));
        assert_eq!(choices[2].label, "Copy Git SHA");
        choose(&mut app, &client, 2);
        app.open_text = Some("Report".into());
        app.proof_file_popup = Some(Popup::Preview(artifact));
        let Some(Overlay::Text { text }) = overlay(&app) else {
            panic!("preview")
        };
        assert!(text.contains("Ctrl-G copy Git SHA"));
        let effect = key(
            &app,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        )
        .unwrap();
        assert!(handle(&mut app, &client, &effect));
        assert_eq!(last_copied(), Some(osc52(&"b".repeat(40))));
        assert_eq!(app.ask, "Unsent draft");
    }

    #[tokio::test]
    async fn socket_download_checks_hash_and_rejects_bad_metadata_without_leaving_partial_files() {
        let root = std::env::temp_dir().join(format!(
            "ka-proof-files-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let socket = root.as_path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let bytes = b"# Report\n\0\xff\n";
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut input = [0u8; 2048];
                assert!(stream.read(&mut input).await.unwrap() > 0);
                stream
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", bytes.len())
                            .as_bytes(),
                    )
                    .await
                    .unwrap();
                stream.write_all(bytes).await.unwrap();
            }
        });
        let client = Client::at(socket);
        let cache = root.as_path().join("cache");
        let file = file(bytes);
        let path = download(&client, "session", &file, &cache).await.unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut wrong = file.clone();
        wrong.sha256 = "0".repeat(64);
        assert!(download(&client, "session", &wrong, &cache)
            .await
            .unwrap_err()
            .contains("integrity"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        wrong.name = "../../escape".into();
        assert!(download(&client, "session", &wrong, &cache).await.is_err());
        server.await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn authenticated_https_download_and_keyboard_save_require_explicit_overwrite() {
        let root = std::env::temp_dir().join(format!(
            "ka-proof-files-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let workspace = root.as_path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let bytes = b"# Archived report\n";
        let source = workspace.join("report.md");
        fs::write(&source, bytes).unwrap();
        let session = Session::at(&root.as_path().join("sessions/session"));
        session
            .create(&SessionMeta::new(
                "session",
                &workspace,
                "model",
                "2026-10-03T00:00:00Z",
            ))
            .unwrap();
        let file =
            crate::proof::store_file(&session, "report.md", File::open(&source).unwrap()).unwrap();
        session
            .append(
                &Event::new(
                    "proof-1",
                    "2026-10-03T00:00:00Z",
                    "turn-1",
                    EventKind::Proof,
                )
                .with_body(&ProofBody {
                    files: vec![file.clone()],
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        fs::remove_dir_all(&workspace).unwrap();
        fs::create_dir(root.as_path().join("certs")).unwrap();
        let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        fs::write(
            root.as_path().join("certs/server.crt"),
            certificate.cert.pem(),
        )
        .unwrap();
        fs::write(
            root.as_path().join("certs/server.key"),
            certificate.key_pair.serialize_pem(),
        )
        .unwrap();
        let config = crate::config::Config::from_toml(
            "base_url = \"http://127.0.0.1:1/v1\"\nmodel = \"model\"\nlisten = \"127.0.0.1:0\"\n",
        )
        .unwrap();
        let server = Arc::new(crate::server::Server::new(root.as_path(), &config).unwrap());
        let serving = Arc::clone(&server);
        let task = tokio::spawn(async move { serving.serve().await });
        let address = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(address) = server.https_addr() {
                    break address;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let token = crate::pairing::PairingKey::load(root.as_path())
            .unwrap()
            .token()
            .unwrap();
        let unauthorized = Client::at_url(&format!("https://{address}")).unwrap();
        let cache = root.as_path().join("cache");
        assert!(download(&unauthorized, "session", &file, &cache)
            .await
            .unwrap_err()
            .contains("401"));
        let client = Client::at_url(&format!("kyotoagent://{address}?token={token}")).unwrap();
        let path = download(&client, "session", &file, &cache).await.unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let destination = root.as_path().join("saved.md");
        fs::write(&destination, b"original").unwrap();
        let mut app = App::new(workspace, root.as_path().into(), "session".into());
        app.ask = "kept draft".into();
        open(&mut app, &client, file.clone());
        assert!(matches!(app.proof_file_popup, Some(Popup::Loading(_))));
        tokio::time::timeout(Duration::from_secs(10), async {
            while app.proof_file_job.is_some() {
                advance(&mut app).await;
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(app.open_text.as_deref(), Some("# Archived report\n"));
        assert!(!esc_stops(&app));
        assert_eq!(
            key(&app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            Some(Effect::ScrollDown)
        );
        let downloaded = root
            .join(".kyotoagent/downloads")
            .join(&file.id)
            .join(&file.name);
        assert_eq!(fs::read(&downloaded).unwrap(), bytes);
        let download = key(
            &app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        )
        .unwrap();
        assert_eq!(download, Effect::ProofFileChoose(1));
        handle(&mut app, &client, &download);
        tokio::time::timeout(Duration::from_secs(10), async {
            while app.proof_file_job.is_some() {
                advance(&mut app).await;
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fs::read(&downloaded).unwrap(), bytes);
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains(downloaded.to_str().unwrap()));
        app.proof_file_popup = Some(Popup::Preview(file.clone()));
        let capture = capture_open_url();
        let external = key(
            &app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        )
        .unwrap();
        handle(&mut app, &client, &external);
        tokio::time::timeout(Duration::from_secs(10), async {
            while app.proof_file_job.is_some() {
                advance(&mut app).await;
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(last_opened_url().as_deref(), downloaded.to_str());
        drop(capture);
        assert_eq!(app.ask, "kept draft");
        app.proof_file_popup = Some(Popup::Preview(file.clone()));
        let save = key(
            &app,
            KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        )
        .unwrap();
        assert_eq!(save, Effect::ProofFileChoose(3));
        handle(&mut app, &client, &save);
        handle(&mut app, &client, &Effect::DeleteLine);
        handle(
            &mut app,
            &client,
            &Effect::Paste(destination.to_string_lossy().into_owned()),
        );
        handle(&mut app, &client, &Effect::ProofFileChoose(1));
        assert!(matches!(
            app.proof_file_popup,
            Some(Popup::Save {
                overwrite: true,
                ..
            })
        ));
        assert!(app.proof_file_job.is_none());
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        handle(&mut app, &client, &Effect::ProofFileChoose(1));
        tokio::time::timeout(Duration::from_secs(10), async {
            while app.proof_file_job.is_some() {
                advance(&mut app).await;
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert_eq!(app.ask, "kept draft");
        assert!(app.notice.as_deref().unwrap().contains("Saved"));
        assert!(save_copy(&path, &destination, false, &AtomicBool::new(false)).is_err());
        assert!(save_copy(
            &path,
            &root.join("cancelled.md"),
            false,
            &AtomicBool::new(true)
        )
        .is_err());
        assert!(!root.join("cancelled.md").exists());
        task.abort();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn unsupported_artifacts_require_download_confirmation_and_cancel_pending_jobs() {
        let mut app = App::new("/w".into(), "/home/u".into(), "session".into());
        let mut artifact = file(b"binary");
        artifact.media_type = "application/zip".into();
        open(&mut app, &Client::at("/unused".into()), artifact);
        assert!(matches!(
            app.proof_file_popup,
            Some(Popup::Actions { highlight: 1, .. })
        ));
        assert!(app.proof_file_job.is_none());
        let prompt = overlay(&app).unwrap();
        assert!(
            matches!(prompt, Overlay::Question { text, .. } if text.contains("Download report.md?"))
        );
        handle(
            &mut app,
            &Client::at("/unused".into()),
            &Effect::ProofFileChoose(0),
        );
        assert!(app.proof_file_popup.is_none());
        let task =
            tokio::spawn(async { std::future::pending::<Result<Completed, String>>().await });
        let abort = task.abort_handle();
        app.proof_file_job = Some(Job {
            cancelled: Arc::new(AtomicBool::new(false)),
            session: "session".into(),
            server: None,
            task,
        });
        release_command_surfaces(&mut app);
        assert!(abort.is_finished() || app.proof_file_job.is_none());
        assert!(!app.overlay);
    }

    #[test]
    fn unsupported_formats_offer_download_and_large_previews_are_bounded() {
        let mut artifact = file(b"<script>window.alert(1)</script>");
        artifact.media_type = "text/html".into();
        assert!(!previewable(&artifact));
        assert_eq!(actions(&artifact)[1].label, "Download");
        artifact.media_type = "application/pdf".into();
        assert!(!previewable(&artifact));
        artifact.media_type = "image/png".into();
        assert!(previewable(&artifact));
        artifact.size = 5 * 1024 * 1024 + 1;
        assert!(!previewable(&artifact));
    }
}
