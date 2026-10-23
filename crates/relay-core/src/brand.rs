//! Product identity in one place.
//!
//! Crate names (`relay-core`, …) and the internal credential-store service id
//! (`"relay"`) are deliberately *not* product names and are not defined here.
//! Everything a user can see — the binary, data directory, background service
//! labels, environment variables — is read from these constants.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

/// Human-readable product name.
pub const APP_NAME: &str = "Turnout";
/// Command-line executable name.
pub const BIN_NAME: &str = "turnout";
/// Per-user data directory name (`~/Library/Application Support/Turnout`, …).
pub const DATA_DIR_NAME: &str = "Turnout";
/// Data directory used by releases before the rename; read only for the
/// one-time migration.
pub const LEGACY_DATA_DIR_NAME: &str = "Relay";
/// Name used in UI/messages for the background gateway process.
pub const AGENT_NAME: &str = "Turnout Agent";
/// Provider id written into harness configs for the local gateway route.
pub const GATEWAY_PROVIDER_ID: &str = "turnout";
/// Pre-rename provider id, removed from harness configs on the next apply.
pub const LEGACY_GATEWAY_PROVIDER_ID: &str = "relay";

pub const LAUNCHD_LABEL: &str = "dev.turnout.gateway";
pub const LEGACY_LAUNCHD_LABEL: &str = "dev.relay.gateway";
pub const SYSTEMD_UNIT: &str = "turnout-gateway.service";
pub const LEGACY_SYSTEMD_UNIT: &str = "relay-gateway.service";
pub const WINDOWS_TASK: &str = "TurnoutGateway";
pub const LEGACY_WINDOWS_TASK: &str = "RelayGateway";

/// Prefix used for every environment variable the tool documents.
pub const ENV_PREFIX: &str = "TURNOUT_";
pub const LEGACY_ENV_PREFIX: &str = "RELAY_";

/// Read `TURNOUT_<name>`, falling back to the pre-rename `RELAY_<name>`.
/// The new name always wins, so users can migrate at their own pace.
pub fn env_os(name: &str) -> Option<OsString> {
    std::env::var_os(format!("{ENV_PREFIX}{name}"))
        .or_else(|| std::env::var_os(format!("{LEGACY_ENV_PREFIX}{name}")))
}

/// Same as [`env_os`] for callers that need UTF-8.
pub fn env_var(name: &str) -> Result<String, std::env::VarError> {
    match env_os(name) {
        Some(value) => value
            .into_string()
            .map_err(|_| std::env::VarError::NotUnicode(OsString::new())),
        None => Err(std::env::VarError::NotPresent),
    }
}

/// Whether a boolean-like environment variable is set (`1`, `true`, …).
pub fn env_flag(name: &str) -> bool {
    env_os(name).is_some_and(|value| {
        let value = value.to_string_lossy().to_ascii_lowercase();
        !matches!(value.as_str(), "" | "0" | "false" | "no" | "off")
    })
}

/// `<platform data dir>/Turnout`.
pub fn default_data_dir() -> Option<PathBuf> {
    base_data_dir().map(|base| base.join(DATA_DIR_NAME))
}

fn base_data_dir() -> Option<PathBuf> {
    dirs::data_dir().or_else(dirs::home_dir)
}

/// Result of the one-time data directory migration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Migration {
    /// Nothing to move: no legacy directory.
    Fresh,
    /// The legacy directory was renamed to the new name.
    Moved(PathBuf),
    /// Both directories exist. Nothing was touched; the caller must warn.
    Conflict { active: PathBuf, legacy: PathBuf },
}

/// Move `<base>/Relay` to `<base>/Turnout` the first time the renamed build
/// runs. Never overwrites: if the new directory exists, both stay and a
/// conflict is reported so the caller can print a warning.
///
/// After a move the directory is `0700` and `credentials.json` is `0600`, even
/// when the old install had looser permissions.
pub fn migrate_data_dir(base: &Path) -> io::Result<Migration> {
    let active = base.join(DATA_DIR_NAME);
    let legacy = base.join(LEGACY_DATA_DIR_NAME);
    if active.exists() {
        if legacy.exists() {
            return Ok(Migration::Conflict { active, legacy });
        }
        return Ok(Migration::Fresh);
    }
    if !legacy.exists() {
        return Ok(Migration::Fresh);
    }
    match std::fs::rename(&legacy, &active) {
        Ok(()) => {
            tighten_permissions(&active)?;
            Ok(Migration::Moved(active))
        }
        // Another process may have completed the same move concurrently.
        Err(error) if active.exists() && error.kind() != io::ErrorKind::PermissionDenied => {
            Ok(Migration::Conflict { active, legacy })
        }
        Err(error) => Err(error),
    }
}

fn tighten_permissions(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let credentials = dir.join("credentials.json");
        if credentials.exists() {
            std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    {
        // No mode bits outside Unix; `relay-config`'s `perms` helper applies
        // an owner-only ACL instead, and the credentials store tightens its
        // own file on every write.
        let _ = dir;
    }
    Ok(())
}

/// Resolve the data directory and perform the one-time migration.
///
/// `$TURNOUT_HOME` (or legacy `$RELAY_HOME`) wins outright and is never
/// migrated: an explicit location is already the user's choice. A conflict is
/// reported through `warn` so the caller decides where to print it.
pub fn resolve_data_dir(mut warn: impl FnMut(&str)) -> Option<PathBuf> {
    if let Some(explicit) = env_os("HOME") {
        return Some(PathBuf::from(explicit));
    }
    let base = base_data_dir()?;
    match migrate_data_dir(&base) {
        Ok(Migration::Fresh) => {}
        Ok(Migration::Moved(path)) => warn(&format!(
            "{APP_NAME}: moved the old data directory to {} (credentials kept at 0600).",
            path.display()
        )),
        Ok(Migration::Conflict { active, legacy }) => warn(&format!(
            "{APP_NAME}: both {} and {} exist; nothing was moved. Set {}HOME to choose one.",
            active.display(),
            legacy.display(),
            ENV_PREFIX
        )),
        Err(error) => warn(&format!(
            "{APP_NAME}: could not move the old data directory ({error}); using {}.",
            base.join(DATA_DIR_NAME).display()
        )),
    }
    Some(base.join(DATA_DIR_NAME))
}

/// Path helper used by tests and callers that already know the base directory.
pub fn data_dir_at(base: impl AsRef<Path>) -> PathBuf {
    base.as_ref().join(DATA_DIR_NAME)
}

/// True when `value` is one of the legacy environment variable names.
pub fn is_legacy_env_key(value: &OsStr) -> bool {
    value.to_string_lossy().starts_with(LEGACY_ENV_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_env_name_wins_over_the_legacy_one() {
        // Unique names keep this test isolated from parallel tests.
        std::env::set_var("TURNOUT_TEST_COMPAT", "new");
        std::env::set_var("RELAY_TEST_COMPAT", "old");
        assert_eq!(env_var("TEST_COMPAT").unwrap(), "new");
        std::env::remove_var("TURNOUT_TEST_COMPAT");
        assert_eq!(env_var("TEST_COMPAT").unwrap(), "old");
        std::env::remove_var("RELAY_TEST_COMPAT");
        assert!(env_var("TEST_COMPAT").is_err());
    }

    #[test]
    fn env_flags_treat_false_values_as_off() {
        std::env::set_var("TURNOUT_TEST_FLAG", "0");
        assert!(!env_flag("TEST_FLAG"));
        std::env::set_var("TURNOUT_TEST_FLAG", "1");
        assert!(env_flag("TEST_FLAG"));
        std::env::remove_var("TURNOUT_TEST_FLAG");
        std::env::set_var("RELAY_TEST_FLAG", "yes");
        assert!(env_flag("TEST_FLAG"));
        std::env::remove_var("RELAY_TEST_FLAG");
    }

    #[test]
    fn legacy_names_are_recognised() {
        assert!(is_legacy_env_key(OsStr::new("RELAY_HOME")));
        assert!(!is_legacy_env_key(OsStr::new("TURNOUT_HOME")));
    }

    #[cfg(unix)]
    #[test]
    fn moves_the_legacy_directory_and_tightens_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_DATA_DIR_NAME);
        std::fs::create_dir_all(&legacy).unwrap();
        let credentials = legacy.join("credentials.json");
        std::fs::write(&credentials, "{}").unwrap();
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o644)).unwrap();

        let result = migrate_data_dir(dir.path()).unwrap();
        let active = dir.path().join(DATA_DIR_NAME);
        assert_eq!(result, Migration::Moved(active.clone()));
        assert!(!legacy.exists());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&active), 0o700);
        assert_eq!(mode(&active.join("credentials.json")), 0o600);
    }

    #[test]
    fn never_overwrites_an_existing_new_directory() {
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join(DATA_DIR_NAME);
        let legacy = dir.path().join(LEGACY_DATA_DIR_NAME);
        std::fs::create_dir_all(&active).unwrap();
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(active.join("state.json"), "new").unwrap();
        std::fs::write(legacy.join("state.json"), "old").unwrap();

        let result = migrate_data_dir(dir.path()).unwrap();
        assert_eq!(
            result,
            Migration::Conflict {
                active: active.clone(),
                legacy: legacy.clone()
            }
        );
        assert_eq!(
            std::fs::read_to_string(active.join("state.json")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("state.json")).unwrap(),
            "old"
        );
    }

    #[test]
    fn a_fresh_install_has_nothing_to_move() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(migrate_data_dir(dir.path()).unwrap(), Migration::Fresh);
        assert!(!dir.path().join(DATA_DIR_NAME).exists());
    }
}
