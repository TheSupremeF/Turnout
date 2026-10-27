//! Raw HTTP + SSE transport shared by provider adapters and the gateway.
//!
//! Rules enforced here:
//! - No vendor SDKs; plain HTTPS, JSON and SSE only.
//! - Provider error bodies are redacted before they can reach logs or reports.

use std::collections::VecDeque;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{RelayError, Result};

pub use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// Wrapper over `reqwest` with product-wide defaults.
#[derive(Clone)]
pub struct HttpClient {
    client: reqwest::Client,
    timeout: Duration,
    /// Receives the headers of every response, when set.
    sink: Option<Arc<dyn crate::quota::HeaderSink>>,
}

impl Default for HttpClient {
    fn default() -> Self {
        HttpClient::new().expect("http client")
    }
}

impl HttpClient {
    pub fn new() -> Result<Self> {
        Self::build(Duration::from_secs(60), false)
    }

    /// Build a client that connects directly instead of inheriting process proxy
    /// variables. Used for protected OAuth inference so bearer credentials are
    /// not handed to an unexpected local proxy and SSE cannot be buffered by it.
    pub fn direct() -> Result<Self> {
        Self::build(Duration::from_secs(60), true)
    }

    pub fn with_timeout(timeout: Duration) -> Result<Self> {
        Self::build(timeout, false)
    }

    fn build(timeout: Duration, direct: bool) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(format!("{}/{}", crate::BIN_NAME, env!("CARGO_PKG_VERSION")));
        if direct {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|e| RelayError::Http(e.to_string()))?;
        Ok(HttpClient {
            client,
            timeout,
            sink: None,
        })
    }

    /// Record response headers (rate limits, retry hints) through `sink`.
    /// Chained after the constructor so no call site has to thread it through.
    pub fn with_header_sink(
        mut self,
        sink: Option<Arc<dyn crate::quota::HeaderSink>>,
    ) -> Self {
        self.sink = sink;
        self
    }

    /// Hand a response's headers to the sink, if one is set. Reading the
    /// headers cannot fail the request, and the sink itself never errors.
    fn observe(&self, response: &reqwest::Response) {
        if let Some(sink) = &self.sink {
            sink.record(response.headers());
        }
    }

    pub async fn get_json<T: DeserializeOwned>(&self, url: &str, headers: &HeaderMap) -> Result<T> {
        let response = self
            .client
            .get(url)
            .headers(headers.clone())
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| RelayError::Http(redact_secrets(&e.to_string())))?;
        self.observe(&response);
        decode_json(response, url).await
    }

    pub async fn post_json<T: DeserializeOwned>(
        &self,
        url: &str,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<T> {
        let response = self
            .client
            .post(url)
            .headers(headers.clone())
            .timeout(self.timeout)
            .json(body)
            .send()
            .await
            .map_err(|e| RelayError::Http(redact_secrets(&e.to_string())))?;
        self.observe(&response);
        decode_json(response, url).await
    }

    /// POST that expects an SSE body back; returns parsed SSE frames.
    pub async fn post_sse(
        &self,
        url: &str,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<SseStream> {
        let response = self
            .client
            .post(url)
            .headers(headers.clone())
            .json(body)
            .send()
            .await
            .map_err(|e| RelayError::Http(redact_secrets(&e.to_string())))?;
        // A 429 carries the same limits as a successful call, so record first.
        self.observe(&response);
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(RelayError::ProviderStatus {
                provider: host_of(url),
                status: status.as_u16(),
                message: redact_secrets(&excerpt(&text)),
            });
        }
        Ok(sse_frames(response.bytes_stream()))
    }
}

async fn decode_json<T: DeserializeOwned>(response: reqwest::Response, url: &str) -> Result<T> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| RelayError::Http(e.to_string()))?;
    if !status.is_success() {
        return Err(RelayError::ProviderStatus {
            provider: host_of(url),
            status: status.as_u16(),
            message: redact_secrets(&excerpt(&text)),
        });
    }
    serde_json::from_str(&text).map_err(|e| RelayError::Parse {
        path: std::path::PathBuf::from(url),
        message: redact_secrets(&e.to_string()),
    })
}

fn excerpt(text: &str) -> String {
    let text = text.trim();
    if text.len() > 600 {
        format!("{}…", &text[..600])
    } else {
        text.to_string()
    }
}

fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "provider".to_string())
}

/// Join a base URL and a path without duplicating slashes.
pub fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    format!("{base}/{path}")
}

// ---------------------------------------------------------------------------
// SSE
// ---------------------------------------------------------------------------

/// One parsed Server-Sent Event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

pub type SseStream = Pin<Box<dyn Stream<Item = Result<SseFrame>> + Send>>;

/// Parse a single SSE block (fields separated by newlines).
pub fn parse_sse_block(block: &str) -> SseFrame {
    let mut frame = SseFrame::default();
    for line in block.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => frame.event = Some(value.to_string()),
            "data" => {
                if !frame.data.is_empty() {
                    frame.data.push('\n');
                }
                frame.data.push_str(value);
            }
            "id" => frame.id = Some(value.to_string()),
            "retry" => frame.retry = value.trim().parse().ok(),
            _ => {}
        }
    }
    frame
}

/// Convert a byte stream into SSE frames.
pub fn fn_sse_frames<S>(source: S) -> SseStream
where
    S: Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
{
    Box::pin(SseByteStream {
        source: Box::pin(source),
        buffer: VecDeque::new(),
        pending: None,
        done: false,
    })
}

/// Named wrapper kept for call-site readability.
pub fn sse_frames<S>(source: S) -> SseStream
where
    S: Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
{
    fn_sse_frames(source)
}

struct SseByteStream<S> {
    source: Pin<Box<S>>,
    buffer: VecDeque<u8>,
    pending: Option<Bytes>,
    done: bool,
}

impl<S> SseByteStream<S>
where
    S: Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
{
    fn take_frame(&mut self) -> Option<SseFrame> {
        // Find a blank-line separator in the buffer.
        let bytes: Vec<u8> = self.buffer.iter().copied().collect();
        let mut start = 0;
        let mut sep = None;
        while start + 1 < bytes.len() {
            if bytes[start] == b'\n' {
                if bytes[start + 1] == b'\n' {
                    sep = Some((start, 2));
                    break;
                }
                if bytes[start + 1] == b'\r' && start + 2 < bytes.len() && bytes[start + 2] == b'\n'
                {
                    sep = Some((start, 3));
                    break;
                }
            }
            if bytes[start] == b'\r'
                && start + 1 < bytes.len()
                && bytes[start + 1] == b'\n'
                && start + 2 < bytes.len()
                && bytes[start + 2] == b'\r'
            {
                sep = Some((start, 4));
                break;
            }
            start += 1;
        }
        if let Some((end, skip)) = sep {
            let block: Vec<u8> = self.buffer.drain(..end).collect();
            for _ in 0..skip {
                self.buffer.pop_front();
            }
            let block = String::from_utf8_lossy(&block).to_string();
            return Some(parse_sse_block(&block));
        }
        if self.done && !self.buffer.is_empty() {
            let block: Vec<u8> = self.buffer.drain(..).collect();
            let block = String::from_utf8_lossy(&block).to_string();
            if block.trim().is_empty() {
                return None;
            }
            return Some(parse_sse_block(&block));
        }
        None
    }
}

impl<S> Stream for SseByteStream<S>
where
    S: Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
{
    type Item = Result<SseFrame>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(frame) = this.take_frame() {
                return Poll::Ready(Some(Ok(frame)));
            }
            if this.done {
                return Poll::Ready(None);
            }
            if let Some(pending) = this.pending.take() {
                this.buffer.extend(pending.iter());
                continue;
            }
            match this.source.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.extend(chunk.iter());
                }
                Poll::Ready(Some(Err(err))) => {
                    return Poll::Ready(Some(Err(RelayError::Http(err.to_string()))));
                }
                Poll::Ready(None) => {
                    this.done = true;
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

const SENSITIVE_KEYS: [&str; 8] = [
    "key",
    "token",
    "secret",
    "password",
    "authorization",
    "credential",
    "apikey",
    "bearer",
];

const TOKEN_PREFIXES: [&str; 12] = [
    "sk-ant-",
    "sk-",
    "rk-",
    "ghp_",
    "gho_",
    "github_pat_",
    "xoxb-",
    "AIza",
    "ya29.",
    // Groq, Hugging Face and Together keys: long, high-entropy, and printed
    // verbatim by upstream error bodies.
    "gsk_",
    "hf_",
    "together_",
];

/// Mask secrets in free-form text (error bodies, logs, CLI output).
///
/// Conservative on purpose: over-redaction is always safe.
pub fn redact_secrets(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Known token prefixes: mask until a delimiter.
        let rest: String = chars[i..].iter().take(12).collect();
        if TOKEN_PREFIXES.iter().any(|p| rest.starts_with(p)) {
            let start = i;
            while i < chars.len()
                && !chars[i].is_whitespace()
                && chars[i] != '"'
                && chars[i] != '\''
                && chars[i] != ','
                && chars[i] != ')'
            {
                i += 1;
            }
            out.push_str(&mask(&chars[start..i].iter().collect::<String>()));
            continue;
        }

        // Sensitive key names followed by `:` or `=` — mask the value.
        if let Some((key_end, _key)) = match_sensitive_key(&chars, i) {
            let mut j = key_end;
            while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
                j += 1;
            }
            // Only a real `key:` / `key=` pair is masked; otherwise the text
            // falls through so token-prefix scanning still sees it.
            if j < chars.len() && (chars[j] == ':' || chars[j] == '=') {
                out.push_str(&chars[i..key_end].iter().collect::<String>());
                out.push(chars[j]);
                j += 1;
                while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
                    out.push(chars[j]);
                    j += 1;
                }
                let quote = if j < chars.len() && (chars[j] == '"' || chars[j] == '\'') {
                    let q = chars[j];
                    out.push(q);
                    j += 1;
                    Some(q)
                } else {
                    None
                };
                let value_start = j;
                while j < chars.len() {
                    match quote {
                        Some(q) if chars[j] == q => break,
                        None if chars[j].is_whitespace()
                            || chars[j] == ','
                            || chars[j] == '}'
                            || chars[j] == ']' =>
                        {
                            break
                        }
                        _ => j += 1,
                    }
                }
                let value: String = chars[value_start..j].iter().collect();
                // `Authorization: Bearer <jwt>` — the secret is the *second*
                // word. Masking only "Bearer" leaves the credential in the log.
                if quote.is_none() && is_auth_scheme(&value) {
                    out.push_str(&value);
                    while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
                        out.push(chars[j]);
                        j += 1;
                    }
                    let token_start = j;
                    while j < chars.len()
                        && !chars[j].is_whitespace()
                        && chars[j] != ','
                        && chars[j] != '}'
                        && chars[j] != ']'
                    {
                        j += 1;
                    }
                    out.push_str(&mask(&chars[token_start..j].iter().collect::<String>()));
                    i = j;
                    continue;
                }
                out.push_str(&mask(&value));
                if let Some(q) = quote {
                    if j < chars.len() && chars[j] == q {
                        out.push(q);
                        j += 1;
                    }
                }
                i = j;
                continue;
            }
        }

        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Auth schemes whose value is `Scheme <credential>`, not a bare secret.
fn is_auth_scheme(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "bearer" | "basic" | "token" | "digest" | "negotiate"
    )
}

fn match_sensitive_key(chars: &[char], start: usize) -> Option<(usize, String)> {
    // Read a quoted or bare identifier starting at `start`.
    let quote = match chars.get(start) {
        Some('"') | Some('\'') => Some(chars[start]),
        Some(c) if c.is_ascii_alphabetic() || *c == '_' => None,
        _ => return None,
    };
    let mut i = start + quote.map(|_| 1).unwrap_or(0);
    let mut key = String::new();
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == q {
                i += 1;
                break;
            }
        } else if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            // keep going
        } else {
            break;
        }
        key.push(c);
        i += 1;
    }
    let lowered = key.to_lowercase();
    if SENSITIVE_KEYS.iter().any(|k| lowered.contains(k)) {
        Some((i, key))
    } else {
        None
    }
}

/// Mask a secret, keeping a short suffix for correlation.
pub fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 4 {
        return "*".repeat(chars.len().max(1));
    }
    format!(
        "{}…{}",
        "*".repeat((chars.len() - 4).min(12)),
        chars[chars.len() - 4..].iter().collect::<String>()
    )
}

// ---------------------------------------------------------------------------
// Unified diff
// ---------------------------------------------------------------------------

/// Unified diff between two texts (used for config change review).
pub fn unified_diff(before: &str, after: &str, path: &Path) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    if before == after {
        return String::new();
    }
    let display = path.display().to_string();
    let mut out = format!("--- a/{display}\n+++ b/{display}\n");

    // LCS-based edit script; config files are small, the cap guards outliers.
    let ops = if a.len() * b.len() > 4_000_000 {
        vec![Op::DeleteAll, Op::InsertAll]
    } else {
        edit_script(&a, &b)
    };

    // Group into hunks with three lines of context.
    #[derive(Clone, Copy, PartialEq)]
    enum Tag {
        Equal,
        Delete,
        Insert,
    }
    let tagged: Vec<(Tag, &str)> = ops
        .iter()
        .flat_map(|op| match op {
            Op::Equal(s) => vec![(Tag::Equal, *s)],
            Op::Delete(s) => vec![(Tag::Delete, *s)],
            Op::Insert(s) => vec![(Tag::Insert, *s)],
            Op::DeleteAll => a.iter().map(|s| (Tag::Delete, *s)).collect(),
            Op::InsertAll => b.iter().map(|s| (Tag::Insert, *s)).collect(),
        })
        .collect();

    let mut idx = 0;
    let mut old_line = 1usize;
    let mut new_line = 1usize;
    while idx < tagged.len() {
        if tagged[idx].0 == Tag::Equal {
            old_line += 1;
            new_line += 1;
            idx += 1;
            continue;
        }
        // Hunk start: back up three context lines.
        let mut start = idx;
        let mut back = 0;
        while start > 0 && back < 3 && tagged[start - 1].0 == Tag::Equal {
            start -= 1;
            back += 1;
        }
        // Hunk end: include the changes and up to three trailing context lines.
        let mut end = idx;
        let mut forward = 0;
        while end < tagged.len() {
            if tagged[end].0 != Tag::Equal {
                forward = 0;
            } else {
                forward += 1;
                if forward > 3 {
                    end -= 2;
                    break;
                }
            }
            end += 1;
        }
        end = end.min(tagged.len());

        let old_start = old_line - back;
        let new_start = new_line - back;
        let mut old_count = 0usize;
        let mut new_count = 0usize;
        let mut body = String::new();
        for (tag, line) in &tagged[start..end] {
            match tag {
                Tag::Equal => {
                    old_count += 1;
                    new_count += 1;
                    body.push_str(&format!(" {line}\n"));
                }
                Tag::Delete => {
                    old_count += 1;
                    body.push_str(&format!("-{line}\n"));
                }
                Tag::Insert => {
                    new_count += 1;
                    body.push_str(&format!("+{line}\n"));
                }
            }
        }
        out.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
        out.push_str(&body);

        for (tag, _) in &tagged[idx..end] {
            match tag {
                Tag::Equal => {
                    old_line += 1;
                    new_line += 1;
                }
                Tag::Delete => old_line += 1,
                Tag::Insert => new_line += 1,
            }
        }
        idx = end;
        // Skip the context lines we already emitted bookkeeping for.
        while idx < tagged.len() && tagged[idx].0 == Tag::Equal {
            old_line += 1;
            new_line += 1;
            idx += 1;
        }
    }

    out
}

enum Op<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
    DeleteAll,
    InsertAll,
}

fn edit_script<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<Op<'a>> {
    let n = a.len();
    let m = b.len();
    // LCS length table.
    let mut table = vec![0usize; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[idx(i, j)] = if a[i] == b[j] {
                table[idx(i + 1, j + 1)] + 1
            } else {
                table[idx(i + 1, j)].max(table[idx(i, j + 1)])
            };
        }
    }
    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Equal(a[i]));
            i += 1;
            j += 1;
        } else if table[idx(i + 1, j)] >= table[idx(i, j + 1)] {
            ops.push(Op::Delete(a[i]));
            i += 1;
        } else {
            ops.push(Op::Insert(b[j]));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete(a[i]));
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert(b[j]));
        j += 1;
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse_block() {
        let frame = parse_sse_block("event: message_start\ndata: {\"a\":1}\nid: 7\n: comment\n");
        assert_eq!(frame.event.as_deref(), Some("message_start"));
        assert_eq!(frame.data, "{\"a\":1}");
        assert_eq!(frame.id.as_deref(), Some("7"));
    }

    #[test]
    fn joins_multiline_data() {
        let frame = parse_sse_block("data: one\ndata: two\n");
        assert_eq!(frame.data, "one\ntwo");
    }

    #[test]
    fn redacts_known_token_shapes() {
        let masked = redact_secrets("Authorization: Bearer sk-abc123def456ghi789");
        assert!(!masked.contains("sk-abc123def456ghi789"));
        assert!(masked.contains('…') || masked.contains('*'));
    }

    #[test]
    fn redacts_json_key_values() {
        let masked = redact_secrets(r#"{"api_key": "super-secret-value", "model": "gpt-4o"}"#);
        assert!(!masked.contains("super-secret-value"));
        assert!(masked.contains("gpt-4o"));
    }

    #[test]
    fn redacts_tokens_inside_quoted_prose_mentioning_key() {
        let masked = redact_secrets(r#"{"message":"bad key sk-test-leak"}"#);
        assert!(!masked.contains("sk-test-leak"), "{masked}");
    }

    #[test]
    fn redacts_env_style_values() {
        let masked = redact_secrets("OPENAI_API_KEY=abcd1234efgh5678\nMODEL=x\n");
        assert!(!masked.contains("abcd1234efgh5678"));
    }

    #[test]
    fn a_bearer_scheme_does_not_hide_the_token_after_it() {
        // Regression: `Authorization: Bearer <token>` masked only the word
        // "Bearer", so the credential itself reached the request log verbatim.
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl";
        let masked = redact_secrets(&format!("Authorization: Bearer {jwt}\nnext line"));
        assert!(!masked.contains(jwt), "{masked}");
        assert!(masked.contains("Authorization"));
        // A bare, non-scheme value still masks as before.
        let bare = redact_secrets("/api key: abcd1234efgh5678");
        assert!(!bare.contains("abcd1234efgh5678"), "{bare}");
    }

    #[test]
    fn redacts_groq_and_hugging_face_keys() {
        for secret in ["gsk_live1234567890abcdef", "hf_abcdefghijklmnopqrst"] {
            let masked = redact_secrets(&format!("upstream rejected {secret}"));
            assert!(!masked.contains(secret), "{masked}");
        }
    }

    #[test]
    fn builds_unified_diff() {
        let diff = unified_diff("a\nb\nc\n", "a\nB\nc\n", Path::new("cfg.toml"));
        assert!(diff.contains("--- a/cfg.toml"));
        assert!(diff.contains("-b"));
        assert!(diff.contains("+B"));
    }

    #[test]
    fn empty_diff_for_identical_text() {
        assert_eq!(unified_diff("x\n", "x\n", Path::new("f")), "");
    }

    #[test]
    fn join_url_handles_slashes() {
        assert_eq!(
            join_url("https://x.com/v1/", "/models"),
            "https://x.com/v1/models"
        );
    }
}
