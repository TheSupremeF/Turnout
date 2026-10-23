//! Shared building blocks for harness adapters: planning file edits, safe
//! apply with backup + rollback, executable discovery and version probing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use relay_core::format::ConfigFormat;
use relay_core::{
    BackupId, ChangePlan, ConfigFileSnapshot, InstallSource, PlannedFileChange, RelayError, Result,
    Validation, ValidationMessage, Version,
};

use crate::atomic;
use crate::backup::BackupStore;
use crate::document::Document;
use crate::path::ConfigPath;

/// One edit applied to a config document.
#[derive(Clone, Debug)]
pub enum Edit {
    Set(ConfigPath, Value),
    Remove(ConfigPath),
}

impl Edit {
    pub fn set(path: impl Into<ConfigPath>, value: impl Into<Value>) -> Self {
        Edit::Set(path.into(), value.into())
    }

    pub fn remove(path: impl Into<ConfigPath>) -> Self {
        Edit::Remove(path.into())
    }
}

/// Open the document at `path`, or an empty one in `default_format` when the
/// file does not exist yet. Unparseable files are an error — never clobbered.
pub fn read_document(path: &Path, default_format: ConfigFormat) -> Result<Document> {
    if path.exists() {
        Document::open(path)
    } else {
        Ok(Document::empty(default_format))
    }
}

pub fn snapshot_file(path: &Path, default_format: ConfigFormat) -> Result<ConfigFileSnapshot> {
    let exists = path.exists();
    let doc = read_document(path, default_format)?;
    Ok(ConfigFileSnapshot {
        path: path.to_path_buf(),
        format: doc.format(),
        exists,
        values: doc.to_value()?,
        contents: doc.text().to_string(),
    })
}

/// Build a reviewable change for one file without touching the disk.
pub fn plan_file(
    path: &Path,
    default_format: ConfigFormat,
    edits: &[Edit],
    notes: Vec<String>,
) -> Result<PlannedFileChange> {
    let existed = path.exists();
    let mut doc = read_document(path, default_format)?;
    let before = existed.then(|| doc.text().to_string());
    for edit in edits {
        match edit {
            Edit::Set(path, value) => doc.set(path, value)?,
            Edit::Remove(path) => {
                doc.remove(path)?;
            }
        }
    }
    Ok(PlannedFileChange {
        path: path.to_path_buf(),
        format: doc.format(),
        before,
        after: doc.into_text(),
        notes,
    })
}

/// Check that every planned file still parses and embeds no API key.
pub fn validate_plan(plan: &ChangePlan) -> Validation {
    let mut validation = Validation::pass();
    for change in &plan.changes {
        if let Err(error) = Document::parse(change.format, change.after.clone()) {
            validation = validation.with_message(ValidationMessage::error(format!(
                "{} would not parse after the change: {error}",
                change.path.display()
            )));
            continue;
        }
        if contains_secret_token(&change.after) {
            validation = validation.with_message(ValidationMessage::error(format!(
                "{} would contain what looks like an API key; keys must stay in the credential store",
                change.path.display()
            )));
        }
        if change.before.as_deref() == Some(change.after.as_str()) {
            validation = validation.with_message(ValidationMessage::info(format!(
                "{} is already up to date",
                change.path.display()
            )));
        }
    }
    validation
}

/// Apply a plan: refuse stale plans, back everything up, write atomically,
/// verify, and roll back automatically if any step fails.
pub fn apply_plan(plan: &ChangePlan, backups: &BackupStore) -> Result<BackupId> {
    let validation = validate_plan(plan);
    if !validation.ok {
        let reasons: Vec<_> = validation.messages.iter().map(|m| m.text.clone()).collect();
        return Err(RelayError::Validation(reasons.join("; ")));
    }
    for change in &plan.changes {
        let current = change
            .path
            .exists()
            .then(|| atomic::read_file(&change.path))
            .transpose()?;
        if current != change.before {
            return Err(RelayError::Validation(format!(
                "{} changed since the plan was made; re-run plan",
                change.path.display()
            )));
        }
    }

    let paths: Vec<PathBuf> = plan.changes.iter().map(|c| c.path.clone()).collect();
    let record = backups.create(&format!("apply {}", plan.harness), &paths)?;

    let result = (|| -> Result<()> {
        for change in &plan.changes {
            atomic::write_file(&change.path, &change.after)?;
            let written = atomic::read_file(&change.path)?;
            if written != change.after {
                return Err(RelayError::Validation(format!(
                    "verification failed for {}",
                    change.path.display()
                )));
            }
            Document::parse(change.format, written)?;
        }
        Ok(())
    })();

    match result {
        Ok(()) => Ok(record.id),
        Err(error) => {
            let _ = backups.restore(&record.id);
            Err(error)
        }
    }
}

/// Heuristic scan for provider API keys inside text that is about to be
/// written to a config file.
pub fn contains_secret_token(text: &str) -> bool {
    const PREFIXES: [&str; 7] = [
        "sk-",
        "sk-ant-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "AIza",
        "ya29.",
    ];
    let bytes = text.as_bytes();
    for prefix in PREFIXES {
        let mut from = 0;
        while let Some(found) = text[from..].find(prefix) {
            let at = from + found;
            from = at + prefix.len();
            let boundary = at == 0 || !(bytes[at - 1] as char).is_ascii_alphanumeric();
            if !boundary {
                continue;
            }
            let run = text[from..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .count();
            if run >= 16 {
                return true;
            }
        }
    }
    false
}

/// Locate an executable on PATH and in common install directories that GUI
/// apps (which do not inherit a shell PATH) would otherwise miss.
pub fn find_binary(names: &[&str]) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        for sub in [
            ".local/bin",
            ".claude/local",
            ".npm-global/bin",
            ".bun/bin",
            ".cargo/bin",
            ".opencode/bin",
            "AppData/Roaming/npm",
        ] {
            dirs.push(home.join(sub));
        }
    }
    for fixed in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        dirs.push(PathBuf::from(fixed));
    }
    let suffixes: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    for dir in dirs {
        for name in names {
            for suffix in suffixes {
                let candidate = dir.join(format!("{name}{suffix}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Build a `Command` that can actually start `binary`.
///
/// On Windows an npm-installed harness is a `.cmd` shim, and `CreateProcess`
/// cannot execute one: it must go through `cmd /C "…"`. Everything else is
/// spawned directly. Quote handling follows Rust's `Command` rules, so a
/// shim path with spaces is quoted here and the argument list is appended
/// unmodified by the caller.
pub fn process(binary: &Path) -> Command {
    #[cfg(windows)]
    {
        let extension = binary
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        let is_shim = extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat");
        if is_shim {
            let mut command = Command::new(
                std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()),
            );
            command.arg("/C").arg(binary);
            return command;
        }
    }
    Command::new(binary)
}

pub fn install_source(binary: &Path) -> InstallSource {
    let text = binary.to_string_lossy().to_lowercase();
    if text.contains("/extensions/") || text.contains("\\extensions\\") {
        InstallSource::Unknown("editor-extension".to_string())
    } else if text.contains("homebrew") || text.contains("cellar") {
        InstallSource::Homebrew
    } else if text.contains("node_modules") || text.contains("npm") || text.contains(".bun") {
        InstallSource::Npm
    } else if text.starts_with("/usr/") {
        InstallSource::System
    } else {
        InstallSource::Path
    }
}

/// Executables bundled inside editor extensions (VS Code, Insiders, Cursor,
/// Windsurf), e.g. `openai.chatgpt-<version>/bin/<platform>/codex`. Users who
/// installed a harness only as an extension have no binary on PATH.
pub fn extension_binaries(extension_prefix: &str, binary: &str) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let file = if cfg!(windows) {
        format!("{binary}.exe")
    } else {
        binary.to_string()
    };
    let mut found = Vec::new();
    for editor in [".vscode", ".vscode-insiders", ".cursor", ".windsurf"] {
        let Ok(entries) = std::fs::read_dir(home.join(editor).join("extensions")) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(extension_prefix)
            })
            .map(|e| e.path())
            .collect();
        dirs.sort();
        dirs.reverse(); // newest version first
        for dir in dirs {
            let Ok(platforms) = std::fs::read_dir(dir.join("bin")) else {
                continue;
            };
            for platform in platforms.flatten() {
                let candidate = platform.path().join(&file);
                if candidate.is_file() {
                    found.push(candidate);
                }
            }
        }
    }
    found
}

/// Installations of a harness: the executable when found, otherwise a
/// config-only entry (e.g. an editor extension that shares the CLI config).
pub fn installations(
    harness: &relay_core::HarnessId,
    binary_names: &[&str],
    extra_candidates: &[PathBuf],
    config_path: &Path,
) -> Vec<relay_core::Installation> {
    let found = find_binary(binary_names)
        .or_else(|| extra_candidates.iter().find(|p| p.is_file()).cloned());
    match found {
        Some(binary) => vec![relay_core::Installation {
            harness: harness.clone(),
            source: install_source(&binary),
            version: binary_version(&binary),
            binary: Some(binary),
            config_path: config_path.to_path_buf(),
        }],
        None if config_path.exists() => vec![relay_core::Installation {
            harness: harness.clone(),
            source: InstallSource::Unknown("config-only".to_string()),
            version: None,
            binary: None,
            config_path: config_path.to_path_buf(),
        }],
        None => Vec::new(),
    }
}

/// Run `<binary> --version` with a hard timeout and parse the result.
pub fn binary_version(binary: &Path) -> Option<Version> {
    let mut child = process(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().ok()? {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
    let output = child.wait_with_output().ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Version::parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_core::{HarnessId, Route};
    use serde_json::json;

    fn route() -> Route {
        Route {
            harness: HarnessId::new("t"),
            surfaces: vec![],
            provider: "p".into(),
            model: "m".into(),
            models: vec!["m".into()],
            profile: None,
        }
    }

    #[test]
    fn detects_embedded_keys_but_not_env_references() {
        assert!(contains_secret_token("key = \"sk-abcdefghijklmnop1234\""));
        assert!(contains_secret_token("ghp_abcdefghijklmnopqrstu"));
        assert!(!contains_secret_token("apiKey: \"{env:OPENAI_API_KEY}\""));
        assert!(!contains_secret_token(
            "task-runner-configuration-value-here"
        ));
        assert!(!contains_secret_token("sk-short"));
    }

    #[test]
    fn plan_apply_rollback_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.toml");
        let original = "# mine\nmodel = \"a\"\n\n[x]\nkeep = 1\n";
        std::fs::write(&path, original).unwrap();
        let backups = BackupStore::at(dir.path().join("backups"));

        let change = plan_file(
            &path,
            ConfigFormat::Toml,
            &[
                Edit::set("model", json!("b")),
                Edit::set("model_provider", json!("relay")),
            ],
            vec![],
        )
        .unwrap();
        let plan = ChangePlan::new(HarnessId::new("t"), route(), vec![change]);
        assert!(validate_plan(&plan).ok);
        assert!(plan.unified_diff().contains("+model_provider"));
        // Planning must not touch the disk.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        let id = apply_plan(&plan, &backups).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# mine") && after.contains("keep = 1"));
        assert!(after.contains("model = \"b\""));

        backups.restore(&id).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn stale_plans_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.json");
        std::fs::write(&path, "{\"a\": 1}").unwrap();
        let backups = BackupStore::at(dir.path().join("backups"));
        let change = plan_file(
            &path,
            ConfigFormat::Json,
            &[Edit::set("a", json!(2))],
            vec![],
        )
        .unwrap();
        let plan = ChangePlan::new(HarnessId::new("t"), route(), vec![change]);
        std::fs::write(&path, "{\"a\": 99}").unwrap(); // user edits meanwhile
        assert!(matches!(
            apply_plan(&plan, &backups),
            Err(RelayError::Validation(_))
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\": 99}");
    }

    #[test]
    fn plans_for_new_files_roll_back_by_deleting_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/new.json");
        let backups = BackupStore::at(dir.path().join("backups"));
        let change = plan_file(
            &path,
            ConfigFormat::Json,
            &[Edit::set("a.b", json!(1))],
            vec![],
        )
        .unwrap();
        assert!(change.before.is_none());
        let plan = ChangePlan::new(HarnessId::new("t"), route(), vec![change]);
        let id = apply_plan(&plan, &backups).unwrap();
        assert!(path.exists());
        backups.restore(&id).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn refuses_to_embed_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.json");
        let change = plan_file(
            &path,
            ConfigFormat::Json,
            &[Edit::set("apiKey", json!("sk-abcdefghijklmnop1234"))],
            vec![],
        )
        .unwrap();
        let plan = ChangePlan::new(HarnessId::new("t"), route(), vec![change]);
        assert!(!validate_plan(&plan).ok);
        let backups = BackupStore::at(dir.path().join("backups"));
        assert!(apply_plan(&plan, &backups).is_err());
        assert!(!path.exists());
    }
}
