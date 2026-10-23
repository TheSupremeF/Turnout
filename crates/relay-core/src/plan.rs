use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::format::ConfigFormat;
use crate::harness::{HarnessId, Route};

/// Identifier of a backup taken before applying a change plan.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackupId(pub String);

impl BackupId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when this is a plain directory name: no separators, no `..`, no
    /// drive or leading dot. Ids are joined onto the backups directory, so a
    /// value like `../..` would escape it.
    pub fn is_safe(&self) -> bool {
        !self.0.is_empty()
            && self.0.len() <= 128
            && !self.0.starts_with('.')
            && self
                .0
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    }
}

impl std::fmt::Display for BackupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One file change inside a plan. `before` is the on-disk content at plan time
/// (`None` when the file does not exist yet); `after` is the full new content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlannedFileChange {
    pub path: PathBuf,
    pub format: ConfigFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    pub after: String,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl PlannedFileChange {
    /// Unified diff of this file for review in the UI / CLI.
    pub fn unified_diff(&self) -> String {
        let before = self.before.as_deref().unwrap_or("");
        crate::transport::unified_diff(before, &self.after, &self.path)
    }
}

/// A reviewable set of file changes for one route.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChangePlan {
    pub id: String,
    pub harness: HarnessId,
    pub route: Route,
    /// Unix timestamp (seconds) when the plan was created.
    pub created_at_unix: u64,
    pub changes: Vec<PlannedFileChange>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl ChangePlan {
    pub fn new(harness: HarnessId, route: Route, changes: Vec<PlannedFileChange>) -> Self {
        let created_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let id = format!(
            "plan-{}-{}",
            created_at_unix,
            short_hash(&format!("{:?}", changes))
        );
        ChangePlan {
            id,
            harness,
            route,
            created_at_unix,
            changes,
            notes: Vec::new(),
        }
    }

    /// Combined unified diff of every file in the plan.
    pub fn unified_diff(&self) -> String {
        self.changes
            .iter()
            .map(PlannedFileChange::unified_diff)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationMessage {
    pub level: ValidationLevel,
    pub text: String,
}

impl ValidationMessage {
    pub fn info(text: impl Into<String>) -> Self {
        ValidationMessage {
            level: ValidationLevel::Info,
            text: text.into(),
        }
    }

    pub fn warning(text: impl Into<String>) -> Self {
        ValidationMessage {
            level: ValidationLevel::Warning,
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        ValidationMessage {
            level: ValidationLevel::Error,
            text: text.into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Validation {
    pub ok: bool,
    pub messages: Vec<ValidationMessage>,
}

impl Validation {
    pub fn pass() -> Self {
        Validation {
            ok: true,
            messages: Vec::new(),
        }
    }

    pub fn with_message(mut self, message: ValidationMessage) -> Self {
        if message.level == ValidationLevel::Error {
            self.ok = false;
        }
        self.messages.push(message);
        self
    }
}

/// Tiny non-cryptographic short hash used for plan/backup ids.
pub fn short_hash(input: &str) -> String {
    // FNV-1a 64-bit, hex-encoded; ids only need uniqueness within a machine.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::Version;

    #[test]
    fn version_parses_common_shapes() {
        let v = Version::parse("codex-cli 1.2.3 (abc)").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
        let v = Version::parse("v0.9.1").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (0, 9, 1));
        let v = Version::parse("2.0").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (2, 0, 0));
        assert!(Version::parse("unknown").is_none());
    }
}
