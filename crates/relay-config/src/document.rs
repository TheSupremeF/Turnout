use std::path::{Path, PathBuf};

use serde_json::Value;

use relay_core::format::ConfigFormat;
use relay_core::{RelayError, Result};

use crate::env_edit;
use crate::json_edit;
use crate::path::ConfigPath;
use crate::toml_edit;
use crate::yaml_edit;

/// A config file Relay can read and patch losslessly.
#[derive(Clone, Debug)]
pub struct Document {
    format: ConfigFormat,
    text: String,
    source: Option<PathBuf>,
}

impl Document {
    pub fn parse(format: ConfigFormat, text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        let mut format = format;
        if format == ConfigFormat::Json && serde_json::from_str::<Value>(&text).is_err() {
            // Tolerate JSONC (comments / trailing commas) inside `.json` files.
            if json_edit::to_value(&text).is_ok() {
                format = ConfigFormat::Jsonc;
            }
        }
        let doc = Document {
            format,
            text,
            source: None,
        };
        doc.check_parses()?;
        Ok(doc)
    }

    /// Empty document in the given format (used when creating new files).
    pub fn empty(format: ConfigFormat) -> Self {
        Document {
            format,
            text: String::new(),
            source: None,
        }
    }

    /// Read an existing file, detecting its format.
    pub fn open(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| RelayError::io(path, e))?;
        let format = ConfigFormat::detect(path, &text)?;
        Ok(Document {
            format,
            text,
            source: Some(path.to_path_buf()),
        })
    }

    pub fn format(&self) -> ConfigFormat {
        self.format
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Whole document as a JSON value.
    pub fn to_value(&self) -> Result<Value> {
        if self.text.trim().is_empty() {
            return Ok(Value::Object(Default::default()));
        }
        match self.format {
            ConfigFormat::Json | ConfigFormat::Jsonc => json_edit::to_value(&self.text),
            ConfigFormat::Toml => toml_edit::to_value(&self.text),
            ConfigFormat::Yaml => yaml_edit::to_value(&self.text),
            ConfigFormat::Env => Ok(env_edit::to_value(&self.text)),
        }
    }

    /// Read a key path (`None` when it does not exist).
    pub fn get(&self, path: &ConfigPath) -> Result<Option<Value>> {
        let mut current = self.to_value()?;
        for segment in path.segments() {
            match current {
                Value::Object(map) => match map.get(segment) {
                    Some(next) => current = next.clone(),
                    None => return Ok(None),
                },
                Value::Array(items) => match segment.parse::<usize>() {
                    Ok(index) if index < items.len() => current = items[index].clone(),
                    _ => return Ok(None),
                },
                _ => return Ok(None),
            }
        }
        Ok(Some(current))
    }

    /// Set a key path, preserving comments, ordering and unknown fields.
    pub fn set(&mut self, path: &ConfigPath, value: &Value) -> Result<()> {
        self.text = match self.format {
            ConfigFormat::Json | ConfigFormat::Jsonc => {
                json_edit::set(&self.text, path.segments(), value)?
            }
            ConfigFormat::Toml => toml_edit::set(&self.text, path.segments(), value)?,
            ConfigFormat::Yaml => yaml_edit::set(&self.text, path.segments(), value)?,
            ConfigFormat::Env => {
                let key = path
                    .segments()
                    .first()
                    .ok_or_else(|| RelayError::KeyNotFound("(empty)".to_string()))?;
                if path.segments().len() > 1 {
                    return Err(RelayError::KeyNotFound(path.join()));
                }
                let value = match value {
                    Value::String(s) => s.clone(),
                    other => serde_json::to_string(other)
                        .map_err(|e| RelayError::other(e.to_string()))?
                        .trim_matches('"')
                        .to_string(),
                };
                env_edit::set(&self.text, key, &value)
            }
        };
        self.check_parses()?;
        Ok(())
    }

    /// Remove a key path. Returns whether anything changed.
    pub fn remove(&mut self, path: &ConfigPath) -> Result<bool> {
        let (text, changed) = match self.format {
            ConfigFormat::Json | ConfigFormat::Jsonc => {
                json_edit::remove(&self.text, path.segments())?
            }
            ConfigFormat::Toml => toml_edit::remove(&self.text, path.segments())?,
            ConfigFormat::Yaml => yaml_edit::remove(&self.text, path.segments())?,
            ConfigFormat::Env => {
                let Some(key) = path.segments().first() else {
                    return Ok(false);
                };
                env_edit::remove(&self.text, key)
            }
        };
        if changed {
            self.text = text;
            self.check_parses()?;
        }
        Ok(changed)
    }

    /// Ensure the edited text still parses in its own format.
    pub fn check_parses(&self) -> Result<()> {
        if self.text.trim().is_empty() {
            return Ok(());
        }
        match self.format {
            ConfigFormat::Json => {
                serde_json::from_str::<Value>(&self.text)
                    .map_err(|e| RelayError::parse(self.path_label(), e.to_string()))?;
            }
            ConfigFormat::Jsonc => {
                json_edit::to_value(&self.text)
                    .map_err(|e| RelayError::parse(self.path_label(), e.to_string()))?;
            }
            ConfigFormat::Toml => {
                toml_edit::to_value(&self.text)
                    .map_err(|e| RelayError::parse(self.path_label(), e.to_string()))?;
            }
            ConfigFormat::Yaml => {
                yaml_edit::to_value(&self.text)
                    .map_err(|e| RelayError::parse(self.path_label(), e.to_string()))?;
            }
            ConfigFormat::Env => {}
        }
        Ok(())
    }

    fn path_label(&self) -> PathBuf {
        self.source
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("<{}>", self.format)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patches_each_format_through_the_same_api() {
        for (format, text, path, value, expected) in [
            (
                ConfigFormat::Json,
                "{\n  // keep\n  \"model\": \"a\"\n}",
                "model",
                json!("b"),
                "{\n  // keep\n  \"model\": \"b\"\n}",
            ),
            (
                ConfigFormat::Toml,
                "# keep\nmodel = \"a\"\n",
                "model",
                json!("b"),
                "# keep\nmodel = \"b\"\n",
            ),
            (
                ConfigFormat::Yaml,
                "# keep\nmodel: a\n",
                "model",
                json!("b"),
                "# keep\nmodel: b\n",
            ),
            (
                ConfigFormat::Env,
                "# keep\nMODEL=a\n",
                "MODEL",
                json!("b"),
                "# keep\nMODEL=b\n",
            ),
        ] {
            let mut doc = Document::parse(format, text).unwrap();
            doc.set(&ConfigPath::from(path), &value).unwrap();
            assert_eq!(doc.text(), expected, "format {format}");
            assert_eq!(doc.get(&ConfigPath::from(path)).unwrap(), Some(value));
        }
    }

    #[test]
    fn rejects_edits_that_would_break_parsing() {
        let mut doc = Document::parse(ConfigFormat::Json, "{\"a\": 1}").unwrap();
        // Removing the only key is fine — the document still parses.
        assert!(doc.remove(&ConfigPath::from("a")).unwrap());
        assert_eq!(doc.text(), "{}");
    }

    #[test]
    fn creates_new_documents() {
        let mut doc = Document::empty(ConfigFormat::Toml);
        doc.set(
            &ConfigPath::dotted("model_providers.relay.base_url"),
            &json!("https://x"),
        )
        .unwrap();
        assert!(doc.text().contains("[model_providers.relay]"));
        assert!(doc.text().contains("base_url = \"https://x\""));
    }
}
