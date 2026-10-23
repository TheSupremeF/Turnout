use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::format::ConfigFormat;
use crate::plan::{BackupId, ChangePlan, Validation};
use crate::provider::ProviderConfig;

/// Environment variable carrying the local gateway's per-session token.
/// Config files only ever reference this name, never the token.
pub const GATEWAY_TOKEN_ENV: &str = "TURNOUT_GATEWAY_TOKEN";
/// Pre-rename name, still accepted when a process is launched with it.
pub const LEGACY_GATEWAY_TOKEN_ENV: &str = "RELAY_GATEWAY_TOKEN";

/// Coding harness identifier. Open-ended so additional tools can be added
/// without touching call sites (`codex`, `claude-code`, `opencode`, …).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HarnessId(String);

impl HarnessId {
    pub const CODEX: &'static str = "codex";
    pub const CLAUDE_CODE: &'static str = "claude-code";
    pub const OPENCODE: &'static str = "opencode";

    pub fn new(id: impl Into<String>) -> Self {
        HarnessId(id.into().trim().to_lowercase().replace([' ', '/'], "-"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for HarnessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for HarnessId {
    fn from(value: &str) -> Self {
        HarnessId::new(value)
    }
}

/// Surface a route applies to. CLI and VS Code targets are configured
/// separately (they can point at different config files).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    Cli,
    VsCode,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::Cli => "cli",
            Surface::VsCode => "vscode",
        }
    }
}

/// Parsed semantic version (best effort).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub raw: String,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// Parse the first `major.minor(.patch)` run found in `raw`
    /// (handles prefixes like `codex-cli 1.2.3` or `v0.9.1`).
    pub fn parse(raw: &str) -> Option<Version> {
        let bytes = raw.as_bytes();
        for start in 0..bytes.len() {
            if !bytes[start].is_ascii_digit() {
                continue;
            }
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let major = raw[start..end].parse::<u32>().ok()?;
            let rest = &raw[end..];
            let mut parts = [0u32, 0u32];
            let mut cursor = rest;
            for slot in 0..2 {
                match cursor.strip_prefix('.') {
                    Some(next) if next.starts_with(|c: char| c.is_ascii_digit()) => {
                        let mut d = 0;
                        while d < next.len() && next.as_bytes()[d].is_ascii_digit() {
                            d += 1;
                        }
                        parts[slot] = next[..d].parse::<u32>().ok()?;
                        cursor = &next[d..];
                    }
                    _ => break,
                }
            }
            return Some(Version {
                raw: raw.trim().to_string(),
                major,
                minor: parts[0],
                patch: parts[1],
            });
        }
        None
    }
}

/// Where a harness installation came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallSource {
    Path,
    Homebrew,
    Npm,
    System,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Installation {
    pub harness: HarnessId,
    pub source: InstallSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<PathBuf>,
    pub config_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Version>,
}

/// A config file as Turnout read it — original text preserved for diffing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigFileSnapshot {
    pub path: PathBuf,
    pub format: ConfigFormat,
    pub exists: bool,
    pub contents: String,
    pub values: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigSnapshot {
    pub files: Vec<ConfigFileSnapshot>,
}

/// Everything a harness adapter needs to plan a route change. Carries the
/// provider connection and model, plus the *name* of the env var that will hold
/// the API key at launch — never the key itself.
#[derive(Clone, Debug)]
pub struct RouteContext {
    pub provider: ProviderConfig,
    /// Default model selected when the harness starts.
    pub model: String,
    /// Models exposed to the harness picker. Always contains `model`.
    pub models: Vec<String>,
    pub surfaces: Vec<Surface>,
    /// When true the harness points at the local Turnout gateway instead of the
    /// provider directly (used for protocol conversion, e.g. Claude Code →
    /// OpenAI-compatible upstream).
    pub via_gateway: bool,
    pub gateway_url: Option<String>,
    /// Standalone Turnout executable used by editor extensions to obtain the
    /// local gateway token without embedding it in their config files.
    pub gateway_token_command: Option<PathBuf>,
    /// User-only token file for clients (currently OpenCode) that support file
    /// substitution but not command-backed authentication.
    pub gateway_token_file: Option<PathBuf>,
}

/// Contract every harness adapter implements against executables + config
/// files (no vendor SDK).
pub trait HarnessAdapter: Send + Sync {
    fn id(&self) -> &HarnessId;

    /// Known config locations for the given surface.
    fn config_paths(&self, surface: Surface) -> Vec<PathBuf>;

    fn detect_installations(&self) -> Vec<Installation>;

    fn detect_version(&self) -> Option<Version>;

    fn read_config(&self) -> Result<ConfigSnapshot>;

    /// Build the change plan (files, formats, unified diffs) without touching
    /// disk.
    fn plan_changes(&self, route: &crate::harness::Route, ctx: &RouteContext)
        -> Result<ChangePlan>;

    /// Sanity-check a plan (formats parse, keys known, no secrets embedded).
    fn validate(&self, plan: &ChangePlan) -> Result<Validation>;

    /// Write the plan's files atomically after taking backups. Returns the
    /// backup id needed for rollback.
    fn apply(&self, plan: &ChangePlan) -> Result<BackupId>;

    /// Restore every file from a previous apply.
    fn rollback(&self, backup: &BackupId) -> Result<()>;
}

/// A routing decision: which provider/model serves which harness and surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub harness: HarnessId,
    #[serde(default)]
    pub surfaces: Vec<Surface>,
    pub provider: crate::provider::ProviderId,
    /// Default model selected when the harness starts.
    pub model: String,
    /// Models made available for runtime switching. Older saved plans omit
    /// this field and therefore fall back to `model`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}
