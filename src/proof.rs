use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use ring::digest::{Context, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::events::{Event, EventKind, ProofBody, ResultBody};
use crate::session::Session;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofFile {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofResponse {
    pub event_id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofVersion {
    pub version: u64,
    pub event_id: String,
    pub turn_id: String,
    pub at: String,
    pub response: Option<ProofResponse>,
    pub proof: ProofBody,
}

pub fn versions(events: &[Event]) -> Result<Vec<ProofVersion>, serde_json::Error> {
    let mut versions = Vec::new();
    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::Proof {
            continue;
        }
        let response = events[..index]
            .iter()
            .rev()
            .find(|prior| prior.kind == EventKind::Result && prior.turn_id == event.turn_id)
            .map(|result| {
                result.body_as::<ResultBody>().map(|body| ProofResponse {
                    event_id: result.id.clone(),
                    text: body.text,
                })
            })
            .transpose()?;
        versions.push(ProofVersion {
            version: versions.len() as u64 + 1,
            event_id: event.id.clone(),
            turn_id: event.turn_id.clone(),
            at: event.at.clone(),
            response,
            proof: event.body_as()?,
        });
    }
    Ok(versions)
}

pub fn artifact_history(events: &[Event]) -> Result<Vec<ProofVersion>, serde_json::Error> {
    let mut history: Vec<ProofVersion> = Vec::new();
    for event in events {
        let mut proof = match event.kind {
            EventKind::Artifact => {
                let artifact: crate::events::ArtifactBody = event.body_as()?;
                ProofBody {
                    files: vec![artifact.file],
                    ..Default::default()
                }
            }
            EventKind::Proof => {
                let recorded: ProofBody = event.body_as()?;
                ProofBody {
                    files: recorded.files,
                    ..Default::default()
                }
            }
            _ => continue,
        };
        if proof.files.is_empty() {
            continue;
        }
        if let Some(version) = history.iter_mut().find(|v| v.turn_id == event.turn_id) {
            for file in &version.proof.files {
                if !proof.files.iter().any(|f| f.id == file.id) {
                    proof.files.push(file.clone());
                }
            }
            version.proof = proof;
        } else {
            history.push(ProofVersion {
                version: history.len() as u64 + 1,
                event_id: event.id.clone(),
                turn_id: event.turn_id.clone(),
                at: event.at.clone(),
                response: None,
                proof,
            });
        }
    }
    for version in &mut history {
        version.response = events
            .iter()
            .rev()
            .find(|event| event.kind == EventKind::Result && event.turn_id == version.turn_id)
            .map(|event| {
                event.body_as::<ResultBody>().map(|body| ProofResponse {
                    event_id: event.id.clone(),
                    text: body.text,
                })
            })
            .transpose()?;
    }
    Ok(history)
}

pub fn published_files(events: &[Event]) -> Result<Vec<ProofFile>, serde_json::Error> {
    Ok(artifact_history(events)?
        .into_iter()
        .flat_map(|v| v.proof.files)
        .collect())
}

pub fn downloadable_files(events: &[Event]) -> Result<Vec<ProofFile>, serde_json::Error> {
    let mut files = published_files(events)?;
    for event in events
        .iter()
        .filter(|event| event.kind == EventKind::CloseoutRun)
    {
        let run: crate::events::CloseoutRunBody = event.body_as()?;
        if let Some(file) = run.transcript {
            files.push(file);
        }
    }
    Ok(files)
}

pub fn resolve_git_sha(workspace: &Path, reference: &str) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .current_dir(workspace)
        .args([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ])
        .output()
        .map_err(|error| format!("Cannot resolve git_sha: {error}"))?;
    if !output.status.success() {
        return Err("git_sha must identify an existing commit in the session workspace. Use git rev-parse HEAD to find the current commit.".into());
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("git_sha did not resolve to a full commit SHA.".into());
    }
    Ok(sha)
}

pub fn publish(
    session: &Session,
    turn: &str,
    artifact: &crate::events::ArtifactBody,
) -> Result<(), crate::session::SessionError> {
    let event = Event::new(
        &session.next_event_id()?,
        &crate::events::now(),
        turn,
        EventKind::Artifact,
    )
    .with_body(artifact)
    .map_err(|source| crate::session::SessionError::Json {
        path: session.dir().join("events.jsonl"),
        source,
    })?;
    session.append(&event)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn store_file(session: &Session, name: &str, source: File) -> io::Result<ProofFile> {
    if !source.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Artifact must be a regular file",
        ));
    }
    store_reader(session, name, source)
}

pub fn store_bytes(session: &Session, name: &str, bytes: &[u8]) -> io::Result<ProofFile> {
    store_reader(session, name, io::Cursor::new(bytes))
}

fn store_reader(session: &Session, name: &str, mut source: impl Read) -> io::Result<ProofFile> {
    if name.is_empty()
        || name.len() > 255
        || name.chars().any(char::is_control)
        || Path::new(name).file_name().and_then(|part| part.to_str()) != Some(name)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid proof filename",
        ));
    }
    let directory = session.dir().join("proof");
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let mut random = [0; 16];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| io::Error::other("Cannot create proof ID"))?;
    let id = hex(&random);
    let temporary = directory.join(format!(".{id}"));
    let result = (|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let mut digest = Context::new(&SHA256);
        let mut buffer = [0; 65536];
        let mut prefix = Vec::new();
        let mut size = 0u64;
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            if prefix.len() < 4096 {
                let take = count.min(4096 - prefix.len());
                prefix.extend_from_slice(&buffer[..take]);
            }
            output.write_all(&buffer[..count])?;
            digest.update(&buffer[..count]);
            size += count as u64;
        }
        output.sync_all()?;
        let sha256 = hex(digest.finish().as_ref());
        fs::hard_link(&temporary, directory.join(&id))?;
        Ok(ProofFile {
            id,
            name: name.to_string(),
            media_type: media_type(name, &prefix).to_string(),
            size,
            sha256,
            git_sha: None,
        })
    })();
    let cleanup = fs::remove_file(&temporary);
    if result.is_ok() {
        cleanup?;
    }
    result
}

pub fn open_file(session: &Session, id: &str) -> io::Result<File> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No such proof file",
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(session.dir().join("proof").join(id))?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Proof must be a regular file",
        ));
    }
    Ok(file)
}

fn media_type(name: &str, bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png";
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return "image/jpeg";
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return "image/webp";
    }
    if bytes.starts_with(b"%PDF-") {
        return "application/pdf";
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return "application/zip";
    }
    if bytes.get(4..8) == Some(b"ftyp") {
        return "video/mp4";
    }
    if bytes.starts_with(b"\x1a\x45\xdf\xa3") {
        return "video/webm";
    }
    if bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
        return "application/octet-stream";
    }
    match Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md" | "markdown") => "text/markdown",
        Some("html" | "htm") => "text/html",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        _ => "text/plain",
    }
}
