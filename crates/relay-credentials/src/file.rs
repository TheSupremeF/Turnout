//! Plain credentials file: `credentials.json` in Turnout's data directory.
//!
//! This is the default store. It trades the OS keyring's per-application
//! access prompts for the same protection many developer CLIs use (Codex's
//! `auth.json`, Claude Code's credentials): a file only the current user can
//! read (`0600` in a `0700` directory on Unix; the profile ACL on Windows).
//! It is not encrypted at rest. Use the vault or the keyring when that matters.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use relay_core::{CredentialRef, RelayError, Result};

use crate::CredentialStore;

type Entries = BTreeMap<String, BTreeMap<String, String>>;

pub struct FileStore {
    path: PathBuf,
    /// Serializes read-modify-write cycles inside this process.
    lock: Mutex<()>,
}

impl FileStore {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<Entries> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) if text.trim().is_empty() => Ok(Entries::new()),
            Ok(text) => serde_json::from_str(&text).map_err(|error| {
                RelayError::Credential(format!(
                    "{} is not valid ({error}); fix or remove it",
                    self.path.display()
                ))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Entries::new()),
            Err(error) => Err(RelayError::Credential(format!(
                "cannot read {}: {error}",
                self.path.display()
            ))),
        }
    }

    /// Writes atomically with owner-only permissions from the first byte, so a
    /// secret is never briefly world-readable.
    fn write(&self, entries: &Entries) -> Result<()> {
        let io = |error: std::io::Error| {
            RelayError::Credential(format!("cannot write {}: {error}", self.path.display()))
        };
        let dir = self
            .path
            .parent()
            .ok_or_else(|| RelayError::Credential("credentials path has no directory".into()))?;
        std::fs::create_dir_all(dir).map_err(io)?;
        let _ = relay_config::perms::owner_only(dir, true);
        let text = serde_json::to_string_pretty(entries)
            .map_err(|error| RelayError::Credential(error.to_string()))?;
        let temp = self
            .path
            .with_extension(format!("tmp{}", std::process::id()));
        {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp).map_err(io)?;
            file.write_all(text.as_bytes()).map_err(io)?;
            file.sync_all().map_err(io)?;
        }
        std::fs::rename(&temp, &self.path).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            io(error)
        })?;
        // Windows has no `0600` at creation time, so the ACL is applied after
        // the rename; on Unix this is a no-op repetition of the mode already
        // requested above.
        let _ = relay_config::perms::owner_only(&self.path, false);
        Ok(())
    }

    fn guard(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.lock
            .lock()
            .map_err(|_| RelayError::Credential("credentials lock poisoned".into()))
    }
}

impl CredentialStore for FileStore {
    fn name(&self) -> &'static str {
        "file"
    }

    fn get(&self, cred: &CredentialRef) -> Result<Option<String>> {
        let _guard = self.guard()?;
        Ok(self
            .read()?
            .get(&cred.service)
            .and_then(|accounts| accounts.get(&cred.account))
            .cloned())
    }

    fn set(&self, cred: &CredentialRef, secret: &str) -> Result<()> {
        let _guard = self.guard()?;
        let mut entries = self.read()?;
        entries
            .entry(cred.service.clone())
            .or_default()
            .insert(cred.account.clone(), secret.to_string());
        self.write(&entries)
    }

    fn delete(&self, cred: &CredentialRef) -> Result<bool> {
        let _guard = self.guard()?;
        let mut entries = self.read()?;
        let removed = entries
            .get_mut(&cred.service)
            .is_some_and(|accounts| accounts.remove(&cred.account).is_some());
        if removed {
            entries.retain(|_, accounts| !accounts.is_empty());
            self.write(&entries)?;
        }
        Ok(removed)
    }

    fn list(&self) -> Result<Vec<CredentialRef>> {
        let _guard = self.guard()?;
        Ok(self
            .read()?
            .into_iter()
            .flat_map(|(service, accounts)| {
                accounts.into_keys().map(move |account| CredentialRef {
                    service: service.clone(),
                    account,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(account: &str) -> CredentialRef {
        CredentialRef {
            service: "relay".into(),
            account: account.into(),
        }
    }

    #[test]
    fn round_trip_survives_a_new_handle_like_a_second_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join("credentials.json");
        let store = FileStore::open(&path);
        assert_eq!(store.get(&cred("provider:evren")).unwrap(), None);
        assert!(!store.has(&cred("provider:evren")).unwrap());
        store.set(&cred("provider:evren"), "sk-one").unwrap();
        store.set(&cred("oauth:openai"), "{\"t\":1}").unwrap();

        let other = FileStore::open(&path);
        assert_eq!(
            other.get(&cred("provider:evren")).unwrap().as_deref(),
            Some("sk-one")
        );
        assert_eq!(other.list().unwrap().len(), 2);
        assert!(other.delete(&cred("provider:evren")).unwrap());
        assert!(!other.delete(&cred("provider:evren")).unwrap());
        assert_eq!(
            store.get(&cred("oauth:openai")).unwrap().as_deref(),
            Some("{\"t\":1}")
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_and_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay").join("credentials.json");
        let store = FileStore::open(&path);
        store.set(&cred("a"), "secret").unwrap();
        store.set(&cred("a"), "secret-2").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        let leftovers = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1, "temp files must not be left behind");
    }

    #[test]
    fn a_corrupt_file_is_reported_not_silently_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, "{ not json").unwrap();
        let store = FileStore::open(&path);
        assert!(store.get(&cred("a")).is_err());
        assert!(store.set(&cred("a"), "x").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }
}
