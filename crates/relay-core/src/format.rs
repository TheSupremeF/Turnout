use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{RelayError, Result};

/// Configuration formats Turnout can patch without losing comments, ordering or
/// unknown fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFormat {
    Json,
    Jsonc,
    Toml,
    Yaml,
    Env,
}

impl ConfigFormat {
    /// Detect by file extension.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_string_lossy().to_lowercase();
        match ext.as_str() {
            "json" => Some(ConfigFormat::Json),
            "jsonc" | "code-workspace" => Some(ConfigFormat::Jsonc),
            "toml" => Some(ConfigFormat::Toml),
            "yaml" | "yml" => Some(ConfigFormat::Yaml),
            "env" => Some(ConfigFormat::Env),
            _ => None,
        }
    }

    /// Detect from contents when the extension is missing or misleading
    /// (e.g. `.env` files without extension, or JSONC inside a `.json` file).
    pub fn sniff(contents: &str) -> Option<Self> {
        let trimmed = contents.trim_start();
        if trimmed.starts_with('#') {
            // `.env` style files are comments + KEY=VALUE pairs.
            if contents.lines().any(|line| {
                let line = line.trim();
                line.starts_with('#') || line.is_empty() || env_assignment(line).is_some()
            }) && contents
                .lines()
                .filter_map(|line| env_assignment(line.trim()))
                .count()
                > 0
            {
                return Some(ConfigFormat::Env);
            }
            return None;
        }
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
                // Could be TOML (`[table]`) or a JSON array. TOML wins only when
                // a bare `key = value` line exists outside strings.
                if looks_like_toml(contents) {
                    Some(ConfigFormat::Toml)
                } else {
                    Some(ConfigFormat::Json)
                }
            } else {
                Some(ConfigFormat::Json)
            };
        }
        if looks_like_toml(contents) {
            return Some(ConfigFormat::Toml);
        }
        if looks_like_yaml(contents) {
            return Some(ConfigFormat::Yaml);
        }
        None
    }

    /// Extension-aware detection with content sniffing as a fallback.
    pub fn detect(path: &Path, contents: &str) -> Result<Self> {
        let by_path = Self::from_path(path);
        if let Some(format) = by_path {
            // Trust the extension except for `.json` files that clearly hold
            // JSONC (comments / trailing commas) or `.env` files with no ext.
            if format == ConfigFormat::Json && has_jsonc_markers(contents) {
                return Ok(ConfigFormat::Jsonc);
            }
            return Ok(format);
        }
        if let Some(format) = Self::sniff(contents) {
            return Ok(format);
        }
        if contents.trim().is_empty() {
            return Ok(ConfigFormat::Json);
        }
        Err(RelayError::UnsupportedFormat {
            path: path.to_path_buf(),
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ConfigFormat::Json => "json",
            ConfigFormat::Jsonc => "jsonc",
            ConfigFormat::Toml => "toml",
            ConfigFormat::Yaml => "yaml",
            ConfigFormat::Env => "env",
        }
    }
}

impl std::fmt::Display for ConfigFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// KEY=VALUE recognition used by format sniffing and the `.env` editor.
/// Accepts an optional `export ` prefix.
pub(crate) fn env_assignment(line: &str) -> Option<(String, String)> {
    let line = line.strip_prefix("export ").unwrap_or(line);
    env_kv(line)
}

/// Parse a single `KEY=VALUE` line (without `export`), returning key and the
/// raw value span text.
pub(crate) fn env_kv(line: &str) -> Option<(String, String)> {
    if line.starts_with('#') || line.trim().is_empty() {
        return None;
    }
    let eq = line.find('=')?;
    let key = line[..eq].trim().to_string();
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    Some((key, line[eq + 1..].to_string()))
}

fn has_jsonc_markers(contents: &str) -> bool {
    // Cheap heuristics: `//` or `/*` comments outside strings, or trailing
    // commas before `}`/`]`.
    let mut in_string = false;
    let mut escaped = false;
    let bytes = contents.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => in_string = true,
            '/' if i + 1 < bytes.len() && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*') => {
                return true
            }
            ',' => {
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn looks_like_toml(contents: &str) -> bool {
    let mut in_string: Option<char> = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(quote) = in_string {
            if line.contains(quote) {
                in_string = None;
            }
            continue;
        }
        if line.starts_with('[') {
            return true;
        }
        if let Some(eq) = line.find('=') {
            let key = line[..eq].trim();
            if !key.is_empty()
                && key.chars().all(|c| {
                    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' || c == '"'
                })
            {
                return true;
            }
        }
        if line.starts_with('"') || line.starts_with('\'') {
            in_string = line.chars().next();
        }
    }
    false
}

fn looks_like_yaml(contents: &str) -> bool {
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "---" || trimmed.starts_with("- ") {
            return true;
        }
        if let Some(eq) = trimmed.find(':') {
            let key = trimmed[..eq].trim();
            if !key.is_empty() && !key.contains('=') && !trimmed[..eq].contains('[') {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_by_extension() {
        assert_eq!(
            ConfigFormat::from_path(&PathBuf::from("/a/config.toml")),
            Some(ConfigFormat::Toml)
        );
        assert_eq!(
            ConfigFormat::from_path(&PathBuf::from("/a/settings.json")),
            Some(ConfigFormat::Json)
        );
        assert_eq!(
            ConfigFormat::from_path(&PathBuf::from("/a/opencode.yaml")),
            Some(ConfigFormat::Yaml)
        );
    }

    #[test]
    fn json_with_comments_upgrades_to_jsonc() {
        let format = ConfigFormat::detect(
            &PathBuf::from("/a/settings.json"),
            "{\n  // keep me\n  \"a\": 1,\n}\n",
        )
        .unwrap();
        assert_eq!(format, ConfigFormat::Jsonc);
    }

    #[test]
    fn sniffs_env_files() {
        let format =
            ConfigFormat::detect(&PathBuf::from("/a/.env"), "# keys\nOPENAI_API_KEY=x\n").unwrap();
        assert_eq!(format, ConfigFormat::Env);
    }

    #[test]
    fn sniffs_toml_and_yaml() {
        assert_eq!(
            ConfigFormat::sniff("[model_providers.turnout]\nbase_url = \"x\"\n"),
            Some(ConfigFormat::Toml)
        );
        assert_eq!(
            ConfigFormat::sniff("provider:\n  openai:\n    models: {}\n"),
            Some(ConfigFormat::Yaml)
        );
    }
}
