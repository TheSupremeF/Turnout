//! Atomic file writes with fsync + rename, preserving existing permissions.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use relay_core::{RelayError, Result};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `contents` to `path` atomically: temp file in the same directory,
/// fsync, rename over the target, then fsync the directory (unix).
pub fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| RelayError::io(parent, e))?;
        }
    }
    let mut temp_name = path.as_os_str().to_owned();
    temp_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = std::path::PathBuf::from(temp_name);

    let mut file = File::create(&temp).map_err(|e| RelayError::io(&temp, e))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| RelayError::io(&temp, e))?;
    file.sync_all().map_err(|e| RelayError::io(&temp, e))?;
    drop(file);

    #[cfg(unix)]
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::set_permissions(&temp, meta.permissions());
    }

    fs::rename(&temp, path).map_err(|e| {
        let _ = fs::remove_file(&temp);
        RelayError::io(path, e)
    })?;

    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
    }
    Ok(())
}

/// Read a file, mapping errors to [`RelayError::Io`].
pub fn read_file(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| RelayError::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");
        write_file(&path, "a = 1\n").unwrap();
        assert_eq!(read_file(&path).unwrap(), "a = 1\n");
        write_file(&path, "a = 2\n").unwrap();
        assert_eq!(read_file(&path).unwrap(), "a = 2\n");
        // No temp files left behind.
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn preserves_permissions_on_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        write_file(&path, "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&path, perms).unwrap();
            write_file(&path, "y").unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
