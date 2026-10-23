//! Backup + rollback for config changes.
//!
//! Every apply takes a backup first; rollback restores every recorded file
//! (and removes files that did not exist before the apply).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use relay_core::{short_hash, BackupId, RelayError, Result};

use crate::atomic;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackupFile {
    pub path: PathBuf,
    pub existed: bool,
    /// Name of the stored copy inside the backup dir (when `existed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default)]
    pub len: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackupRecord {
    pub id: BackupId,
    pub created_unix: u64,
    pub label: String,
    pub files: Vec<BackupFile>,
}

/// Backup directory layout:
/// ```text
/// <root>/<backup-id>/manifest.json
/// <root>/<backup-id>/files/<index>-<basename>
/// ```
pub struct BackupStore {
    root: PathBuf,
}

impl BackupStore {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        BackupStore { root: root.into() }
    }

    /// Default location: `<data dir>/Turnout/backups`.
    pub fn open_default() -> Result<Self> {
        let dir = relay_core::brand::default_data_dir()
            .ok_or_else(|| RelayError::other("cannot locate a writable data directory"))?;
        Ok(BackupStore::at(dir.join("backups")))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Snapshot the given files and return the backup record.
    pub fn create(&self, label: &str, files: &[PathBuf]) -> Result<BackupRecord> {
        let created_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let id = BackupId(format!(
            "b{}-{}",
            created_unix,
            short_hash(&format!("{label}:{}", files.len()))
        ));
        let dir = self.root.join(id.as_str());
        let store_dir = dir.join("files");
        fs::create_dir_all(&store_dir).map_err(|e| RelayError::io(&store_dir, e))?;

        let mut records = Vec::with_capacity(files.len());
        for (index, path) in files.iter().enumerate() {
            if path.exists() {
                let contents = fs::read(path).map_err(|e| RelayError::io(path, e))?;
                let base = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let stored = format!("{index}-{base}");
                fs::write(store_dir.join(&stored), &contents)
                    .map_err(|e| RelayError::io(path, e))?;
                records.push(BackupFile {
                    path: path.clone(),
                    existed: true,
                    stored: Some(stored),
                    sha256: Some(sha256_hex(&contents)),
                    len: contents.len() as u64,
                });
            } else {
                records.push(BackupFile {
                    path: path.clone(),
                    existed: false,
                    stored: None,
                    sha256: None,
                    len: 0,
                });
            }
        }

        let record = BackupRecord {
            id,
            created_unix,
            label: label.to_string(),
            files: records,
        };
        let manifest =
            serde_json::to_string_pretty(&record).map_err(|e| RelayError::other(e.to_string()))?;
        atomic::write_file(&dir.join("manifest.json"), &manifest)?;
        Ok(record)
    }

    pub fn get(&self, id: &BackupId) -> Result<BackupRecord> {
        if !id.is_safe() {
            return Err(RelayError::BackupNotFound(id.to_string()));
        }
        let manifest = self.root.join(id.as_str()).join("manifest.json");
        let text = fs::read_to_string(&manifest)
            .map_err(|_| RelayError::BackupNotFound(id.to_string()))?;
        serde_json::from_str(&text).map_err(|e| RelayError::parse(manifest, e.to_string()))
    }

    /// Restore every file from a backup; returns the restored paths.
    pub fn restore(&self, id: &BackupId) -> Result<Vec<PathBuf>> {
        let record = self.get(id)?;
        let store_dir = self.root.join(id.as_str()).join("files");
        let mut restored = Vec::with_capacity(record.files.len());
        for file in &record.files {
            if file.existed {
                let Some(stored) = &file.stored else {
                    continue;
                };
                // `stored` comes from the manifest on disk: never let it point
                // outside this backup's own `files` directory.
                let stored_path = Path::new(stored);
                if stored_path.components().count() != 1
                    || stored_path
                        .components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    return Err(RelayError::other(format!(
                        "backup {id} names an unsafe stored file: {stored}"
                    )));
                }
                let bytes =
                    fs::read(store_dir.join(stored)).map_err(|e| RelayError::io(&file.path, e))?;
                let contents = String::from_utf8_lossy(&bytes).to_string();
                atomic::write_file(&file.path, &contents)?;
            } else if file.path.exists() {
                fs::remove_file(&file.path).map_err(|e| RelayError::io(&file.path, e))?;
            }
            restored.push(file.path.clone());
        }
        Ok(restored)
    }

    /// All backups, newest first.
    pub fn list(&self) -> Result<Vec<BackupRecord>> {
        let mut records = Vec::new();
        if !self.root.exists() {
            return Ok(records);
        }
        let entries = fs::read_dir(&self.root).map_err(|e| RelayError::io(&self.root, e))?;
        for entry in entries.flatten() {
            let manifest = entry.path().join("manifest.json");
            if !manifest.exists() {
                continue;
            }
            let text = match fs::read_to_string(&manifest) {
                Ok(text) => text,
                Err(_) => continue,
            };
            if let Ok(record) = serde_json::from_str::<BackupRecord>(&text) {
                records.push(record);
            }
        }
        records.sort_by(|a, b| b.created_unix.cmp(&a.created_unix));
        Ok(records)
    }

    pub fn latest(&self) -> Result<Option<BackupRecord>> {
        Ok(self.list()?.into_iter().next())
    }

    /// Drop everything except the `keep` most recent backups.
    pub fn prune(&self, keep: usize) -> Result<usize> {
        let records = self.list()?;
        let mut removed = 0;
        for record in records.into_iter().skip(keep) {
            let dir = self.root.join(record.id.as_str());
            if dir.exists() {
                fs::remove_dir_all(&dir).map_err(|e| RelayError::io(&dir, e))?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Convenience: hash of a file's contents (for change detection).
pub fn file_hash(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|e| RelayError::io(path, e))?;
    Ok(Some(sha256_hex(&bytes)))
}

/// Map of path → hash for a set of files (used by `relay doctor`).
pub fn hash_all(files: &[PathBuf]) -> Result<BTreeMap<PathBuf, Option<String>>> {
    let mut out = BTreeMap::new();
    for file in files {
        out.insert(file.clone(), file_hash(file)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_restore_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::at(dir.path().join("backups"));

        let config = dir.path().join("config.toml");
        fs::write(&config, "a = 1\n").unwrap();
        let missing = dir.path().join("new.json");

        let record = store
            .create("apply", &[config.clone(), missing.clone()])
            .unwrap();
        assert_eq!(record.files.len(), 2);
        assert!(record.files[0].existed);
        assert!(!record.files[1].existed);

        // Change state, then roll back.
        fs::write(&config, "a = 2\n").unwrap();
        fs::write(&missing, "{}").unwrap();

        let restored = store.restore(&record.id).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(fs::read_to_string(&config).unwrap(), "a = 1\n");
        assert!(!missing.exists(), "file created after backup is removed");
    }

    #[test]
    fn lists_newest_first_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::at(dir.path().join("backups"));
        let file = dir.path().join("f");
        fs::write(&file, "x").unwrap();
        store.create("one", &[file.clone()]).unwrap();
        store.create("two", &[file.clone()]).unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].created_unix >= listed[1].created_unix);
        store.prune(1).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn missing_backup_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::at(dir.path().join("backups"));
        let result = store.restore(&BackupId("nope".to_string()));
        assert!(matches!(result, Err(RelayError::BackupNotFound(_))));
    }

    #[test]
    fn a_backup_id_cannot_escape_the_backups_directory() {
        // `turnout rollback ../..` used to join straight onto the store root and
        // read `<data dir>/manifest.json`, i.e. a file outside any backup.
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::at(dir.path().join("backups"));
        for id in ["../..", "..", "/etc", "a/b", "..\\x", ".hidden"] {
            let result = store.restore(&BackupId(id.to_string()));
            assert!(
                matches!(result, Err(RelayError::BackupNotFound(_))),
                "{id} was accepted"
            );
        }
    }

    #[test]
    fn a_manifest_cannot_point_at_a_file_outside_its_own_backup() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::at(dir.path().join("backups"));
        let secret = dir.path().join("secret.txt");
        fs::write(&secret, "SENSITIVE").unwrap();
        let target = dir.path().join("target");
        fs::write(&target, "original").unwrap();
        let record = store.create("one", &[target.clone()]).unwrap();

        // Rewrite the manifest so `stored` reaches out of `files/`.
        let manifest = store.root().join(record.id.as_str()).join("manifest.json");
        let text = fs::read_to_string(&manifest).unwrap();
        let tampered = text.replace(
            &record.files[0].stored.clone().unwrap(),
            "../../secret.txt",
        );
        assert_ne!(text, tampered);
        fs::write(&manifest, tampered).unwrap();

        assert!(store.restore(&record.id).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "original");
    }
}
