//! EVREN: an OpenAI-compatible provider plus its extra endpoints
//! (`/quota`, `/terms/*`). Chat, models and streaming are delegated to the
//! generic adapter — EVREN is a preset, not a requirement.
//!
//! `base_url` must include the API version prefix (e.g. `https://…/v1`).

use std::collections::BTreeMap;

use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::{json, Value};

use provider_openai::OpenAiCompatible;
use relay_core::provider::EventStream;
use relay_core::transport::join_url;
use relay_core::{
    ChatRequest, ChatResponse, Model, ModelCapabilities, ProviderAdapter, ProviderConfig,
    ProviderId, Result, TestResult,
};

/// EVREN's OpenAI-compatible base URL (includes the version prefix).
pub const DEFAULT_BASE_URL: &str = "https://evren-llmapi.ssyz.org.tr/v1";

pub struct EvrenProvider {
    inner: OpenAiCompatible,
}

impl EvrenProvider {
    pub fn config(base_url: impl Into<String>) -> ProviderConfig {
        let mut config = ProviderConfig::openai_compatible(ProviderId::EVREN, base_url);
        config.label = "EVREN".to_string();
        config.api_key_env = "EVREN_API_KEY".to_string();
        config
    }

    pub fn with_default_url(api_key: Option<String>) -> Result<Self> {
        Self::new(DEFAULT_BASE_URL, api_key)
    }

    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Result<Self> {
        Ok(EvrenProvider {
            inner: OpenAiCompatible::new(Self::config(base_url), api_key)?,
        })
    }

    fn url(&self, path: &str) -> String {
        join_url(&self.inner.config().base_url, path)
    }

    /// `GET /quota`
    pub async fn quota(&self) -> Result<Value> {
        self.inner
            .http()
            .get_json(&self.url("quota"), &self.inner.headers()?)
            .await
    }

    /// `GET /quota`, read into [`Quota`].
    pub async fn quota_summary(&self) -> Result<Quota> {
        Ok(Quota::from_value(&self.quota().await?))
    }

    /// `GET /terms/status`
    pub async fn terms_status(&self) -> Result<Value> {
        self.inner
            .http()
            .get_json(&self.url("terms/status"), &self.inner.headers()?)
            .await
    }

    /// `GET /terms/text`
    pub async fn terms_text(&self) -> Result<Value> {
        self.inner
            .http()
            .get_json(&self.url("terms/text"), &self.inner.headers()?)
            .await
    }

    /// `POST /terms/accept`. Only call after the user has seen the terms text.
    pub async fn accept_terms(&self) -> Result<Value> {
        self.inner
            .http()
            .post_json(
                &self.url("terms/accept"),
                &self.inner.headers()?,
                &json!({}),
            )
            .await
    }
}

/// What `GET /quota` reports, under provider-neutral names. Every field is
/// optional: a field the server leaves out (or renames) is simply `None`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Quota {
    /// The server's own verdict, e.g. `ok`.
    pub level: Option<String>,
    pub credits_remaining: Option<f64>,
    /// Credits reserved by requests still running.
    pub credits_held: Option<f64>,
    pub daily_limit_tokens: Option<u64>,
    pub daily_remaining_tokens: Option<u64>,
    pub daily_used_tokens: Option<u64>,
    pub daily_reset_at: Option<String>,
    /// Length of the rolling token window, e.g. 5 minutes.
    pub window_minutes: Option<u64>,
    pub window_limit_tokens: Option<u64>,
    pub window_used_tokens: Option<u64>,
    pub window_reset_at: Option<String>,
    pub concurrency_limit: Option<u64>,
    pub concurrent_requests: Option<u64>,
    /// Per-minute token budget, as `x-ratelimit-*` headers report it.
    pub minute_limit_tokens: Option<u64>,
    pub minute_remaining_tokens: Option<u64>,
    /// Reset as the header gave it (`"12ms"`, `"30s"`, a timestamp…).
    pub minute_reset: Option<String>,
    pub minute_limit_requests: Option<u64>,
    pub minute_remaining_requests: Option<u64>,
    /// Unix seconds the figures describe.
    pub observed_at: Option<u64>,
    /// `"live"` from `/quota`, `"headers"` from a captured response.
    pub source: Option<String>,
}

impl Quota {
    pub fn from_value(value: &Value) -> Self {
        // Credits arrive as decimal strings ("1060.0000"); accept numbers too.
        let float = |key: &str| match &value[key] {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        };
        let int = |key: &str| match &value[key] {
            Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        };
        let text = |key: &str| value[key].as_str().map(str::to_string);
        Quota {
            level: text("level"),
            credits_remaining: float("remaining_cr"),
            credits_held: float("held_cr"),
            daily_limit_tokens: int("daily_limit_tokens"),
            daily_remaining_tokens: int("daily_remaining_tokens"),
            daily_used_tokens: int("daily_used_tokens"),
            daily_reset_at: text("daily_reset_at"),
            window_minutes: int("window_minutes"),
            window_limit_tokens: int("cap"),
            window_used_tokens: int("used_tokens"),
            window_reset_at: text("reset_at"),
            concurrency_limit: int("concurrency_limit"),
            concurrent_requests: int("concurrent_requests"),
            observed_at: Some(relay_core::quota::unix_time()),
            source: Some("live".to_string()),
            ..Quota::default()
        }
    }

    /// Read whichever rate-limit headers a provider sent, at `observed_at`
    /// (unix seconds). Every vendor names them differently; see the table in
    /// [`Self::from_headers`].
    pub fn from_headers(headers: &BTreeMap<String, String>, observed_at: u64) -> Self {
        // Header names arrive lowercased from `HeaderMap`, but a captured file
        // can be edited by hand, so look up case-insensitively anyway.
        let get = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.trim())
                .filter(|value| !value.is_empty())
        };
        let int = |name: &str| get(name).and_then(|value| value.parse::<u64>().ok());
        let float = |name: &str| get(name).and_then(|value| value.parse::<f64>().ok());
        let text = |name: &str| get(name).map(str::to_string);
        // OpenAI/Groq/DeepSeek and Anthropic agree on the units but not names.
        let pick = |openai: &str, anthropic: &str| int(openai).or_else(|| int(anthropic));
        Quota {
            credits_remaining: float("x-evren-credits-remaining"),
            credits_held: float("x-evren-credits-held"),
            daily_remaining_tokens: int("x-evren-daily-remaining-tokens"),
            daily_limit_tokens: int("x-evren-daily-limit-tokens"),
            daily_reset_at: text("x-evren-daily-reset").and_then(|v| reset_at(&v, observed_at)),
            minute_limit_tokens: pick("x-ratelimit-limit-tokens", "anthropic-ratelimit-tokens-limit"),
            minute_remaining_tokens: pick(
                "x-ratelimit-remaining-tokens",
                "anthropic-ratelimit-tokens-remaining",
            ),
            minute_reset: text("x-ratelimit-reset-tokens")
                .or_else(|| text("anthropic-ratelimit-tokens-reset"))
                .or_else(|| text("x-ratelimit-reset"))
                .and_then(|v| reset_at(&v, observed_at)),
            minute_limit_requests: pick(
                "x-ratelimit-limit-requests",
                "anthropic-ratelimit-requests-limit",
            ),
            minute_remaining_requests: pick(
                "x-ratelimit-remaining-requests",
                "anthropic-ratelimit-requests-remaining",
            ),
            observed_at: Some(observed_at),
            source: Some("headers".to_string()),
            ..Quota::default()
        }
    }
}

/// Providers state a reset as unix seconds (EVREN), a duration from now
/// (OpenAI: `12ms`, `6m0s`; some send bare seconds) or an ISO timestamp
/// (Anthropic). All become an ISO UTC timestamp; anything else is dropped.
fn reset_at(value: &str, observed_at: u64) -> Option<String> {
    let value = value.trim();
    if value.contains('T') {
        return Some(value.to_string());
    }
    if let Ok(number) = value.parse::<f64>() {
        let at = if number > 1e12 {
            (number / 1000.0) as u64
        } else if number > 1e9 {
            number as u64
        } else {
            observed_at + number.ceil() as u64
        };
        return Some(iso_utc(at));
    }
    // Go-style duration: `1h2m3.5s`, `12ms`.
    let mut seconds = 0.0;
    let mut rest = value;
    while !rest.is_empty() {
        let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
        let amount: f64 = rest[..end].parse().ok()?;
        rest = &rest[end..];
        let unit_end = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
        seconds += amount
            * match &rest[..unit_end] {
                "h" => 3600.0,
                "m" => 60.0,
                "s" => 1.0,
                "ms" => 0.001,
                _ => return None,
            };
        rest = &rest[unit_end..];
    }
    Some(iso_utc(observed_at + seconds.ceil() as u64))
}

/// Unix seconds → `2026-10-28T00:00:00Z` (civil-from-days, no date crate).
pub fn iso_utc(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let time = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

impl ProviderAdapter for EvrenProvider {
    fn id(&self) -> &ProviderId {
        self.inner.id()
    }

    fn config(&self) -> &ProviderConfig {
        self.inner.config()
    }

    fn discover_models(&self) -> BoxFuture<'_, Result<Vec<Model>>> {
        self.inner.discover_models()
    }

    fn test_connection(&self, model: &str) -> BoxFuture<'_, Result<TestResult>> {
        self.inner.test_connection(model)
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        self.inner.capabilities(model)
    }

    fn stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<EventStream>> {
        self.inner.stream(request)
    }

    fn chat(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatResponse>> {
        self.inner.chat(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_is_a_plain_openai_compatible_config() {
        let config = EvrenProvider::config(DEFAULT_BASE_URL);
        assert_eq!(config.id.as_str(), "evren");
        assert_eq!(config.base_url, "https://evren-llmapi.ssyz.org.tr/v1");
        assert_eq!(config.api_key_env, "EVREN_API_KEY");
        assert_eq!(config.dialect, relay_core::ProtocolDialect::OpenAiChat);
    }

    #[test]
    fn resets_become_iso_utc() {
        let at = 1_793_131_575; // 2026-10-27T20:06:15Z
        assert_eq!(iso_utc(at), "2026-10-27T20:06:15Z");
        assert_eq!(reset_at("1793145600", at).as_deref(), Some("2026-10-28T00:00:00Z"));
        assert_eq!(reset_at("6m0s", at).as_deref(), Some("2026-10-27T20:12:15Z"));
        assert_eq!(reset_at("12ms", at).as_deref(), Some("2026-10-27T20:06:16Z"));
        assert_eq!(reset_at("30", at).as_deref(), Some("2026-10-27T20:06:45Z"));
        assert_eq!(
            reset_at("2026-10-27T20:07:00Z", at).as_deref(),
            Some("2026-10-27T20:07:00Z")
        );
        assert_eq!(reset_at("soon", at), None);
    }

    #[test]
    fn quota_reads_the_evren_answer() {
        let quota = Quota::from_value(&json!({
            "cap": 2500000,
            "concurrency_limit": 32,
            "concurrent_requests": 2,
            "daily_limit_tokens": 15000000,
            "daily_remaining_tokens": 13407136,
            "daily_reset_at": "2026-10-28T00:00:00Z",
            "daily_used_tokens": 1592864,
            "held_cr": "0.0000",
            "level": "ok",
            "remaining_cr": "1060.0000",
            "reset_at": "2026-10-27T09:55:00+00:00",
            "used_tokens": 118069,
            "window_minutes": 5
        }));
        assert_eq!(quota.level.as_deref(), Some("ok"));
        assert_eq!(quota.credits_remaining, Some(1060.0));
        assert_eq!(quota.credits_held, Some(0.0));
        assert_eq!(quota.daily_remaining_tokens, Some(13_407_136));
        assert_eq!(quota.daily_limit_tokens, Some(15_000_000));
        assert_eq!(quota.window_limit_tokens, Some(2_500_000));
        assert_eq!(quota.window_used_tokens, Some(118_069));
        assert_eq!(quota.window_minutes, Some(5));
        assert_eq!(quota.concurrency_limit, Some(32));
        assert_eq!(quota.window_reset_at.as_deref(), Some("2026-10-27T09:55:00+00:00"));
    }

    #[test]
    fn quota_tolerates_missing_and_odd_fields() {
        let quota = Quota::from_value(&json!({"remaining_cr": 12.5, "cap": "100", "level": null}));
        assert_eq!(quota.credits_remaining, Some(12.5));
        assert_eq!(quota.window_limit_tokens, Some(100));
        assert_eq!(quota.level, None);
        assert_eq!(quota.daily_limit_tokens, None);
    }

    fn header_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn quota_reads_evren_rate_limit_headers() {
        let quota = Quota::from_headers(
            &header_map(&[
                ("x-evren-credits-remaining", "1060.5000"),
                ("x-evren-credits-held", "2.2500"),
                ("x-evren-daily-remaining-tokens", "13407136"),
                ("x-evren-daily-limit-tokens", "15000000"),
                ("x-evren-daily-reset", "2026-10-28T00:00:00Z"),
            ]),
            1_772_000_000,
        );
        assert_eq!(quota.credits_remaining, Some(1060.5));
        assert_eq!(quota.credits_held, Some(2.25));
        assert_eq!(quota.daily_remaining_tokens, Some(13_407_136));
        assert_eq!(quota.daily_limit_tokens, Some(15_000_000));
        assert_eq!(quota.daily_reset_at.as_deref(), Some("2026-10-28T00:00:00Z"));
        assert_eq!(quota.observed_at, Some(1_772_000_000));
        assert_eq!(quota.source.as_deref(), Some("headers"));
    }

    #[test]
    fn quota_reads_openai_rate_limit_headers() {
        let quota = Quota::from_headers(
            &header_map(&[
                ("x-ratelimit-limit-tokens", "150000"),
                ("x-ratelimit-remaining-tokens", "149780"),
                ("x-ratelimit-reset-tokens", "12ms"),
                ("x-ratelimit-limit-requests", "10000"),
                ("x-ratelimit-remaining-requests", "9998"),
            ]),
            1_772_000_000,
        );
        assert_eq!(quota.minute_limit_tokens, Some(150_000));
        assert_eq!(quota.minute_remaining_tokens, Some(149_780));
        assert_eq!(quota.minute_reset.as_deref(), Some("2026-02-25T06:13:21Z"));
        assert_eq!(quota.minute_limit_requests, Some(10_000));
        assert_eq!(quota.minute_remaining_requests, Some(9_998));
        // The per-minute budget never lands in the rolling-window fields.
        assert_eq!(quota.window_limit_tokens, None);
        assert_eq!(quota.window_used_tokens, None);
    }

    #[test]
    fn quota_reads_anthropic_rate_limit_headers() {
        let quota = Quota::from_headers(
            &header_map(&[
                ("anthropic-ratelimit-tokens-limit", "40000"),
                ("anthropic-ratelimit-tokens-remaining", "38500"),
                ("anthropic-ratelimit-tokens-reset", "2026-10-27T09:55:00Z"),
                ("anthropic-ratelimit-requests-limit", "50"),
                ("anthropic-ratelimit-requests-remaining", "49"),
            ]),
            1_772_000_000,
        );
        assert_eq!(quota.minute_limit_tokens, Some(40_000));
        assert_eq!(quota.minute_remaining_tokens, Some(38_500));
        assert_eq!(
            quota.minute_reset.as_deref(),
            Some("2026-10-27T09:55:00Z")
        );
        assert_eq!(quota.minute_limit_requests, Some(50));
        assert_eq!(quota.minute_remaining_requests, Some(49));
    }

    #[test]
    fn quota_ignores_junk_header_values() {
        let quota = Quota::from_headers(
            &header_map(&[
                ("x-ratelimit-remaining-tokens", "lots"),
                ("x-ratelimit-limit-tokens", ""),
                ("x-evren-credits-remaining", "n/a"),
                ("retry-after", "3"),
            ]),
            7,
        );
        assert_eq!(quota.minute_remaining_tokens, None);
        assert_eq!(quota.minute_limit_tokens, None);
        assert_eq!(quota.credits_remaining, None);
        // Nothing readable at all: still a headers capture, but empty figures.
        assert_eq!(quota.source.as_deref(), Some("headers"));
        assert_eq!(quota.observed_at, Some(7));
    }

    #[test]
    fn quota_from_headers_is_case_insensitive_and_prefers_openai_names() {
        let quota = Quota::from_headers(
            &header_map(&[
                ("X-RateLimit-Limit-Tokens", "900"),
                ("anthropic-ratelimit-tokens-limit", "40000"),
            ]),
            1,
        );
        assert_eq!(quota.minute_limit_tokens, Some(900));
    }

    #[test]
    fn live_quota_is_marked_as_such() {
        let quota = Quota::from_value(&json!({"level": "ok"}));
        assert_eq!(quota.source.as_deref(), Some("live"));
        assert!(quota.observed_at.is_some());
        assert_eq!(quota.minute_limit_tokens, None);
    }
}
