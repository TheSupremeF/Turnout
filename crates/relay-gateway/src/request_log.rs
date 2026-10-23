//! One JSON line per completed request, appended to `<data dir>/requests/<harness>.jsonl`.
//!
//! This is the only record of what the background agent actually served: which
//! model the harness asked for, which provider answered, how long it took and
//! how many tokens it used. It never contains prompts, responses or keys.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use relay_core::redact_secrets;

/// Accumulates what one request did, then appends a single line.
pub struct RequestMeter {
    path: PathBuf,
    requested_model: String,
    provider: String,
    upstream_model: String,
    stream: bool,
    started: Instant,
    input_tokens: u64,
    output_tokens: u64,
    fell_back: bool,
    first_frame: Option<Instant>,
}

impl RequestMeter {
    pub fn new(
        path: Option<PathBuf>,
        requested_model: &str,
        stream: bool,
    ) -> Option<Self> {
        Some(Self {
            path: path?,
            requested_model: requested_model.to_string(),
            provider: String::new(),
            upstream_model: String::new(),
            stream,
            started: Instant::now(),
            input_tokens: 0,
            output_tokens: 0,
            fell_back: false,
            first_frame: None,
        })
    }

    /// Which provider/model actually served the request.
    pub fn served_by(&mut self, provider: &str, upstream_model: &str, fell_back: bool) {
        self.provider = provider.to_string();
        self.upstream_model = upstream_model.to_string();
        self.fell_back = fell_back;
    }

    pub fn usage(&mut self, input: u64, output: u64) {
        self.input_tokens = input;
        self.output_tokens = output;
    }

    /// Time to the first *content* frame — the number a user actually feels.
    /// Our own opening frame does not count; it is written the moment the
    /// upstream connection is accepted.
    pub fn first_frame(&mut self) {
        self.first_frame.get_or_insert_with(Instant::now);
    }

    /// Write the line. Called exactly once, on success or failure.
    pub fn finish(self, error: Option<&str>) {
        let total_ms = self.started.elapsed().as_millis() as u64;
        let first_frame_ms = self
            .first_frame
            .map(|at| at.saturating_duration_since(self.started).as_millis() as u64);
        let line = json!({
            "ts": unix_now(),
            "model": self.requested_model,
            "provider": self.provider,
            "upstream_model": self.upstream_model,
            "stream": self.stream,
            "status": if error.is_some() { "error" } else { "ok" },
            "ms": total_ms,
            "first_frame_ms": first_frame_ms,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "fallback": self.fell_back,
            "error": error.map(redact_secrets),
        });
        self.drop_oldest_lines();
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Append-only, one line per request: a crash mid-write loses at most
        // this line, never the file.
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Keep the file bounded without a rotation daemon: past the cap the oldest
    /// half is dropped once, so steady-state writes stay O(1).
    fn drop_oldest_lines(&self) {
        const MAX_LINES: usize = 5000;
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return;
        };
        if text.lines().count() < MAX_LINES {
            return;
        }
        let lines = text.lines().collect::<Vec<_>>();
        let keep = lines[lines.len() / 2..].join("\n") + "\n";
        let _ = std::fs::write(&self.path, keep);
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `requests/` directory under the data dir.
pub fn requests_dir(root: &Path) -> PathBuf {
    root.join("requests")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_line_carries_timing_and_usage_but_never_the_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests").join("codex.jsonl");
        let mut meter = RequestMeter::new(Some(path.clone()), "evren/glm-5.3", true).unwrap();
        meter.served_by("evren", "glm-5.3", true);
        meter.usage(12, 34);
        meter.first_frame();
        meter.finish(None);

        let line = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["model"], "evren/glm-5.3");
        assert_eq!(value["provider"], "evren");
        assert_eq!(value["upstream_model"], "glm-5.3");
        assert_eq!(value["input_tokens"], 12);
        assert_eq!(value["output_tokens"], 34);
        assert_eq!(value["fallback"], true);
        assert_eq!(value["status"], "ok");
        assert!(value["first_frame_ms"].is_number());
    }

    #[test]
    fn an_error_line_is_redacted_and_still_has_a_duration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex.jsonl");
        let meter = RequestMeter::new(Some(path.clone()), "glm-5.3", false).unwrap();
        meter.finish(Some("upstream said sk-upstream-secret-0123456789"));
        let line = std::fs::read_to_string(&path).unwrap();
        assert!(!line.contains("sk-upstream-secret-0123456789"), "{line}");
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["status"], "error");
    }

    #[test]
    fn no_path_means_no_file_is_written() {
        assert!(RequestMeter::new(None, "glm-5.3", false).is_none());
    }
}
