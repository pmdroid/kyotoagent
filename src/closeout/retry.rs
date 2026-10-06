use super::{CloseoutError, CloseoutFile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RetryPolicy {
    pub max_failed_attempts_per_item: u32,
    pub scope: RetryScope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryScope {
    Task,
    Candidate,
}

impl RetryPolicy {
    pub fn validate(&self) -> Result<(), CloseoutError> {
        if !(1..=100000).contains(&self.max_failed_attempts_per_item) {
            return Err(CloseoutError::BadDefinition {
                message: "maxFailedAttemptsPerItem must be between 1 and 100000".into(),
            });
        }
        Ok(())
    }
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn skill_files(
    dir: &Path,
    files: &mut BTreeMap<PathBuf, String>,
) -> Result<(), CloseoutError> {
    fn walk(
        dir: &Path,
        files: &mut BTreeMap<PathBuf, String>,
        ancestors: &mut std::collections::HashSet<PathBuf>,
    ) -> Result<(), CloseoutError> {
        let canonical = dir.canonicalize().map_err(|error| io_error(dir, error))?;
        if !ancestors.insert(canonical.clone()) {
            return Err(io_error(dir, "skill directory contains a symlink cycle"));
        }
        let entries = std::fs::read_dir(dir).map_err(|error| io_error(dir, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| io_error(dir, error))?;
            let path = entry.path();
            let metadata = std::fs::metadata(&path).map_err(|error| io_error(&path, error))?;
            if metadata.is_dir() {
                walk(&path, files, ancestors)?;
            } else if metadata.is_file() {
                let bytes = std::fs::read(&path).map_err(|error| io_error(&path, error))?;
                files.insert(path, hash(&bytes));
            } else {
                return Err(io_error(
                    &path,
                    "skill directory entries must be regular files",
                ));
            }
        }
        ancestors.remove(&canonical);
        Ok(())
    }
    walk(dir, files, &mut std::collections::HashSet::new())
}

fn io_error(path: &Path, error: impl std::fmt::Display) -> CloseoutError {
    CloseoutError::Parse {
        path: path.to_path_buf(),
        source: error.to_string(),
    }
}

impl CloseoutFile {
    pub(super) fn digest(&self, root: &Path) -> Result<String, CloseoutError> {
        let mut files = BTreeMap::new();
        for (path, sha256) in &self.policy_files {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| io_error(path, error))?;
            files.insert(relative.to_string_lossy().into_owned(), sha256.clone());
        }
        let mut files: Vec<_> = files
            .into_iter()
            .map(|(path, sha256)| serde_json::json!({"path":path,"sha256":sha256}))
            .collect();
        files.sort_by(|left, right| {
            left["path"]
                .as_str()
                .unwrap()
                .encode_utf16()
                .cmp(right["path"].as_str().unwrap().encode_utf16())
        });
        let requirement = |item: &super::CloseoutItem| {
            let mut value = if let Some(review) = self.reviews.get(&item.id) {
                serde_json::json!({"id":item.id,"kind":"review","gate":"beforePR","skill":review.policy_skill,"independence":{"differentSession":review.independence.different_session,"differentModel":review.independence.different_model},"failOn":format!("{:?}",review.fail_on)})
            } else {
                let (argv, timeout) = self.execution(item);
                serde_json::json!({"id":item.id,"kind":item.kind.label(),"gate":"beforePR","exec":argv,"timeoutSeconds":timeout})
            };
            if !item.paths.is_empty() {
                value["paths"] = serde_json::json!(item.paths);
            }
            value
        };
        let mut identity = serde_json::json!({"specVersion":"0.1","files":files,"items":self.items.iter().map(requirement).collect::<Vec<_>>()});
        if !self.setup.is_empty() {
            identity["setup"] =
                serde_json::json!(self.setup.iter().map(requirement).collect::<Vec<_>>());
        }
        if let Some(retry) = &self.retry {
            identity["retry"] = serde_json::json!(retry);
        }
        Ok(format!(
            "sha256:{}",
            hash(&serde_json::to_vec(&identity).map_err(|error| io_error(root, error))?)
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct AttemptIdentity {
    pub item_id: String,
    pub policy_digest: String,
    pub base: String,
    pub head: String,
    pub task: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    identity: AttemptIdentity,
    attempt: u32,
    state: String,
}

pub(crate) struct RetryLedger {
    file: File,
    entries: Vec<Attempt>,
}

impl RetryLedger {
    pub async fn lock(
        directory: &Path,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        let path = directory.join("retry.json");
        let (mut file, created) = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => (file, true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .map_err(|error| error.to_string())?,
                false,
            ),
            Err(error) => return Err(error.to_string()),
        };
        loop {
            if *cancel.borrow() {
                return Err("Closeout retry wait cancelled".into());
            }
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(error) => return Err(error.to_string()),
            }
            tokio::select! {
                _ = cancel.changed() => {},
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
            }
        }
        if created {
            file.write_all(b"[]")
                .and_then(|()| file.sync_all())
                .map_err(|error| error.to_string())?;
        }
        let mut bytes = Vec::new();
        file.rewind()
            .and_then(|()| file.read_to_end(&mut bytes))
            .map_err(|error| error.to_string())?;
        let entries: Vec<Attempt> = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Closeout retry evidence is invalid: {error}"))?;
        let mut attempts = std::collections::HashSet::new();
        for entry in &entries {
            if entry.attempt == 0
                || !attempts.insert((
                    &entry.identity.item_id,
                    &entry.identity.policy_digest,
                    &entry.identity.head,
                    entry.attempt,
                ))
                || !matches!(
                    entry.state.as_str(),
                    "failed" | "passed" | "stale" | "invalid" | "independence"
                )
            {
                return Err("Closeout retry evidence has an invalid attempt".into());
            }
        }
        Ok(Self { file, entries })
    }

    pub fn failures(&self, identity: &AttemptIdentity, scope: RetryScope) -> u32 {
        self.entries
            .iter()
            .filter(|entry| {
                entry.state == "failed"
                    && entry.identity.item_id == identity.item_id
                    && entry.identity.policy_digest == identity.policy_digest
                    && match scope {
                        RetryScope::Task => entry.identity.task == identity.task,
                        RetryScope::Candidate => {
                            entry.identity.base == identity.base
                                && entry.identity.head == identity.head
                        }
                    }
            })
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }

    pub fn start(&mut self, identity: AttemptIdentity) -> Result<u32, String> {
        let attempt = self
            .entries
            .iter()
            .filter(|entry| {
                entry.identity.item_id == identity.item_id
                    && entry.identity.policy_digest == identity.policy_digest
                    && entry.identity.head == identity.head
            })
            .map(|entry| entry.attempt)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("Closeout attempt counter overflow")?;
        self.entries.push(Attempt {
            identity,
            attempt,
            state: "invalid".into(),
        });
        self.save()?;
        Ok(attempt)
    }

    pub fn finish(&mut self, state: &str) -> Result<(), String> {
        self.entries
            .last_mut()
            .ok_or("Closeout attempt is missing")?
            .state = state.into();
        self.save()
    }

    fn save(&mut self) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self.entries).map_err(|error| error.to_string())?;
        self.file
            .rewind()
            .and_then(|()| self.file.set_len(0))
            .and_then(|()| self.file.write_all(&bytes))
            .and_then(|()| self.file.sync_all())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> AttemptIdentity {
        AttemptIdentity {
            item_id: "check".into(),
            policy_digest: "sha256:policy".into(),
            base: "base".into(),
            head: "head".into(),
            task: "task".into(),
        }
    }

    #[tokio::test]
    async fn only_failed_attempts_persist_in_their_selected_scope() {
        let dir = std::env::temp_dir().join(crate::session::new_task_id());
        let (_, mut cancel) = tokio::sync::watch::channel(false);
        let mut ledger = RetryLedger::lock(&dir, &mut cancel).await.unwrap();
        let first = identity();
        for state in ["failed", "passed", "invalid", "stale", "independence"] {
            ledger.start(first.clone()).unwrap();
            ledger.finish(state).unwrap();
        }
        assert_eq!(ledger.failures(&first, RetryScope::Task), 1);
        drop(ledger);
        let mut ledger = RetryLedger::lock(&dir, &mut cancel).await.unwrap();
        assert_eq!(ledger.failures(&first, RetryScope::Candidate), 1);
        let mut next = first.clone();
        next.head = "next-head".into();
        next.base = "next-base".into();
        assert_eq!(ledger.failures(&next, RetryScope::Task), 1);
        assert_eq!(ledger.failures(&next, RetryScope::Candidate), 0);
        next = first.clone();
        next.task = "another-task".into();
        assert_eq!(ledger.failures(&next, RetryScope::Task), 0);
        assert_eq!(ledger.failures(&next, RetryScope::Candidate), 1);
        next = first.clone();
        next.policy_digest = "changed-policy".into();
        assert_eq!(ledger.failures(&next, RetryScope::Task), 0);
        assert_eq!(ledger.start(first).unwrap(), 6);
        drop(ledger);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn concurrent_attempts_wait_for_recorded_failure_and_cancellation_releases_the_wait() {
        let dir = std::env::temp_dir().join(crate::session::new_task_id());
        let (sender, mut cancel) = tokio::sync::watch::channel(false);
        let mut first = RetryLedger::lock(&dir, &mut cancel).await.unwrap();
        first.start(identity()).unwrap();
        let mut other_cancel = cancel.clone();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(120),
            RetryLedger::lock(&dir, &mut other_cancel)
        )
        .await
        .is_err());
        sender.send(true).unwrap();
        assert!(RetryLedger::lock(&dir, &mut other_cancel).await.is_err());
        first.finish("failed").unwrap();
        drop(first);
        sender.send(false).unwrap();
        let next = RetryLedger::lock(&dir, &mut cancel).await.unwrap();
        assert_eq!(next.failures(&identity(), RetryScope::Task), 1);
        drop(next);
        std::fs::write(dir.join("retry.json"), "invalid evidence").unwrap();
        assert!(RetryLedger::lock(&dir, &mut cancel).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
