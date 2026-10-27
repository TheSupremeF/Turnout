//! Remembering the rate-limit headers a provider sends on ordinary responses.
//!
//! Not every provider serves a `/quota` endpoint, but most send their limits
//! as response headers. [`QuotaSink`] keeps the last set it saw in a small
//! per-provider JSON file so `turnout providers quota <id>` can fall back to
//! them. Header values are the only thing written: never request credentials.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::transport::HeaderMap;

/// Receives the response headers of upstream calls.
pub trait HeaderSink: Send + Sync {
    fn record(&self, headers: &HeaderMap);
}

/// Writes `{"at": <unix secs>, "headers": {name: value}}` to
/// `<dir>/<provider>.json`, replacing the previous capture.
pub struct QuotaSink {
    dir: PathBuf,
    provider: String,
}

impl QuotaSink {
    /// `provider` becomes a file name; anything but `[a-z0-9_-]` is folded to
    /// `_` so a provider id can never escape `dir`.
    pub fn new(dir: impl Into<PathBuf>, provider: &str) -> Self {
        let provider = provider
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        QuotaSink {
            dir: dir.into(),
            provider,
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.provider))
    }
}

impl HeaderSink for QuotaSink {
    fn record(&self, headers: &HeaderMap) {
        let mut captured = BTreeMap::new();
        for (name, value) in headers {
            let name = name.as_str();
            if !interesting(name) {
                continue;
            }
            let Ok(value) = value.to_str() else { continue };
            let value = value.trim();
            // A capture is only ever read back by us; keep it small and plain.
            if !value.is_empty() && value.len() <= 200 {
                captured.insert(name.to_string(), value.to_string());
            }
        }
        if captured.is_empty() {
            return;
        }
        let body = serde_json::json!({ "at": unix_time(), "headers": captured });
        // Never let a failed capture break the request it was reading.
        let _ = write_atomic(&self.path(), body.to_string().as_bytes());
    }
}

fn interesting(name: &str) -> bool {
    name == "retry-after"
        || name.starts_with("x-ratelimit-")
        || name.starts_with("anthropic-ratelimit-")
        || name.starts_with("x-evren-")
}

pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Temp file in the same directory, then rename: readers never see half a file.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{HeaderName, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn writes_only_limit_headers() {
        let dir = tempfile::tempdir().unwrap();
        let sink = QuotaSink::new(dir.path(), "openai");
        sink.record(&headers(&[
            ("x-ratelimit-remaining-tokens", "900"),
            ("anthropic-ratelimit-requests-limit", "50"),
            ("retry-after", "3"),
            ("authorization", "Bearer sk-secret"),
            ("x-evren-credits-remaining", "1060.0"),
            ("content-type", "application/json"),
        ]));
        let text = std::fs::read_to_string(sink.path()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let captured = value["headers"].as_object().unwrap();
        assert_eq!(captured["x-ratelimit-remaining-tokens"], "900");
        assert_eq!(captured["anthropic-ratelimit-requests-limit"], "50");
        assert_eq!(captured["retry-after"], "3");
        assert_eq!(captured["x-evren-credits-remaining"], "1060.0");
        assert!(!captured.contains_key("authorization"));
        assert!(!captured.contains_key("content-type"));
        assert!(value["at"].as_u64().unwrap() > 0);
    }

    #[test]
    fn leaves_no_file_when_nothing_matched() {
        let dir = tempfile::tempdir().unwrap();
        let sink = QuotaSink::new(dir.path(), "groq");
        sink.record(&headers(&[("content-type", "text/plain")]));
        assert!(!sink.path().exists());
    }

    #[test]
    fn provider_id_cannot_escape_the_directory() {
        let sink = QuotaSink::new("/tmp/quota", "../etc/passwd");
        assert_eq!(sink.path(), PathBuf::from("/tmp/quota/___etc_passwd.json"));
    }
}
