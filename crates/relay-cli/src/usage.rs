//! Subscription usage of the official logins: ChatGPT/Codex and Claude.
//!
//! Only the two read-only usage endpoints are called. Tokens are never
//! refreshed or written — refreshing would rotate the refresh token and sign
//! the user out of Codex or Claude Code.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Result};
use base64::Engine;
use relay_core::transport::{HeaderMap, HeaderName, HeaderValue, HttpClient};
use serde::Serialize;
use serde_json::Value;

use crate::home::Home;

const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

const CHATGPT_EXPIRED: &str = "ChatGPT login expired — run Codex once to refresh it";
const CLAUDE_EXPIRED: &str = "Claude login expired — open Claude Code once to refresh it";

/// One window of a subscription limit (5-hour or weekly).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsageWindow {
    pub label: String,
    pub window_minutes: u64,
    pub used_percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

/// What one provider reports, or why it could not.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsageReport {
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<UsageWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl UsageReport {
    fn failed(provider: &str, error: String) -> Self {
        UsageReport {
            provider: provider.to_string(),
            plan: None,
            windows: Vec::new(),
            error: Some(relay_core::redact_secrets(&error)),
        }
    }
}

/// `5h` / `weekly` / `45m` / `2h` for an arbitrary window length.
fn window_label(minutes: u64) -> String {
    match minutes {
        300 => "5h".to_string(),
        10_080 => "weekly".to_string(),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

/// Claude sends an ISO timestamp with fraction and offset. Keep the seconds
/// and mark it UTC when the offset is UTC; otherwise leave it untouched.
fn iso_from_claude(value: &str) -> String {
    let utc = value.ends_with('Z') || value.ends_with("+00:00");
    if utc && value.len() >= 19 {
        return format!("{}Z", &value[..19]);
    }
    value.to_string()
}

/// ChatGPT's `wham/usage` payload → normalized windows.
pub fn parse_codex_usage(value: &Value) -> UsageReport {
    let mut windows = Vec::new();
    for key in ["primary_window", "secondary_window"] {
        let window = &value["rate_limit"][key];
        if !window.is_object() {
            continue;
        }
        let Some(percent) = window["used_percent"].as_f64() else {
            continue;
        };
        let minutes = window["limit_window_seconds"]
            .as_u64()
            .map(|seconds| seconds / 60)
            .unwrap_or(0);
        windows.push(UsageWindow {
            label: window_label(minutes),
            window_minutes: minutes,
            used_percent: percent,
            resets_at: window["reset_at"]
                .as_u64()
                .map(provider_evren::iso_utc),
        });
    }
    UsageReport {
        provider: "openai".to_string(),
        plan: value["plan_type"].as_str().map(str::to_string),
        windows,
        error: None,
    }
}

/// Anthropic's `/api/oauth/usage` payload → normalized windows.
pub fn parse_claude_usage(value: &Value) -> UsageReport {
    let mut windows = Vec::new();
    let mut push = |key: &str, label: &str, minutes: u64| {
        let window = &value[key];
        let Some(percent) = window["utilization"].as_f64() else {
            return;
        };
        windows.push(UsageWindow {
            label: label.to_string(),
            window_minutes: minutes,
            used_percent: percent,
            resets_at: window["resets_at"].as_str().map(iso_from_claude),
        });
    };
    push("five_hour", "5h", 300);
    push("seven_day", "weekly", 10_080);
    push("seven_day_opus", "weekly opus", 10_080);
    push("seven_day_sonnet", "weekly sonnet", 10_080);
    UsageReport {
        provider: "anthropic".to_string(),
        plan: None,
        windows,
        error: None,
    }
}

/// `openai` / `anthropic`, both when no provider was named.
fn selected(provider: Option<&str>) -> Result<Vec<&'static str>> {
    match provider.map(|name| name.trim().to_lowercase()) {
        None => Ok(vec!["openai", "anthropic"]),
        Some(name) if matches!(name.as_str(), "openai" | "chatgpt" | "codex") => {
            Ok(vec!["openai"])
        }
        Some(name) if matches!(name.as_str(), "anthropic" | "claude") => Ok(vec!["anthropic"]),
        Some(name) => bail!("usage is supported for `openai` and `anthropic`, not `{name}`"),
    }
}

pub async fn run(home: &Home, provider: Option<&str>, json: bool) -> Result<()> {
    let providers = selected(provider)?;
    let mut reports = Vec::new();
    for provider in providers {
        reports.push(match provider {
            "openai" => fetch_codex(home).await,
            _ => fetch_claude().await,
        });
    }
    if json {
        println!("{}", serde_json::to_string(&reports)?);
        return Ok(());
    }
    for report in &reports {
        print_human(report);
    }
    Ok(())
}

fn print_human(report: &UsageReport) {
    let title = match report.provider.as_str() {
        "openai" => "ChatGPT",
        "anthropic" => "Claude",
        other => other,
    };
    match &report.plan {
        Some(plan) if !plan.is_empty() => println!("{title} ({plan})"),
        _ => println!("{title}"),
    }
    if let Some(error) = &report.error {
        println!("  {error}");
        return;
    }
    for window in &report.windows {
        let reset = window
            .resets_at
            .as_deref()
            .map(|at| {
                let day = at.get(0..10).unwrap_or_default();
                let time = at.get(11..16).unwrap_or_default();
                format!("   resets {day} {time} UTC")
            })
            .unwrap_or_default();
        println!(
            "  {:<10} {:>4} used{reset}",
            window.label,
            percent(window.used_percent)
        );
    }
}

fn percent(value: f64) -> String {
    if (value - value.round()).abs() < 0.05 {
        format!("{:.0}%", value)
    } else {
        format!("{value:.1}%")
    }
}

// ---- ChatGPT ----------------------------------------------------------------

/// Codex's own login first, then Turnout's stored ChatGPT session.
fn codex_tokens(home: &Home) -> Option<(String, Option<String>)> {
    if let Some(tokens) = codex_login_tokens() {
        return Some(tokens);
    }
    let tokens = crate::read_openai_oauth_tokens(home).ok()?;
    let account = account_id_from_id_token(&tokens.id_token);
    Some((tokens.access_token, account))
}

fn codex_auth_path() -> Option<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))?;
    Some(home.join("auth.json"))
}

fn codex_login_tokens() -> Option<(String, Option<String>)> {
    let text = std::fs::read_to_string(codex_auth_path()?).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let tokens = &value["tokens"];
    let access = tokens["access_token"].as_str()?.to_string();
    let account = tokens["account_id"].as_str().map(str::to_string);
    Some((access, account))
}

/// The account id lives in the ID token's `https://api.openai.com/auth` claim.
fn account_id_from_id_token(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .map(str::to_string)
}

async fn fetch_codex(home: &Home) -> UsageReport {
    let Some((token, account)) = codex_tokens(home) else {
        return UsageReport::failed(
            "openai",
            "ChatGPT login not found — run Codex once to sign in".to_string(),
        );
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("authorization"),
        match HeaderValue::from_str(&format!("Bearer {token}")) {
            Ok(value) => value,
            Err(error) => return UsageReport::failed("openai", error.to_string()),
        },
    );
    if let Some(account) = account.and_then(|account| HeaderValue::from_str(&account).ok()) {
        headers.insert(HeaderName::from_static("chatgpt-account-id"), account);
    }
    headers.insert(
        HeaderName::from_static("originator"),
        HeaderValue::from_static("codex_cli_rs"),
    );
    headers.insert(
        HeaderName::from_static("user-agent"),
        HeaderValue::from_static("codex_cli_rs"),
    );

    let client = match HttpClient::new() {
        Ok(client) => client,
        Err(error) => return UsageReport::failed("openai", error.to_string()),
    };
    match client
        .get_json::<Value>(CODEX_USAGE_URL, &headers)
        .await
    {
        Ok(value) => parse_codex_usage(&value),
        Err(error) => UsageReport::failed("openai", codex_error(&error)),
    }
}

fn codex_error(error: &relay_core::RelayError) -> String {
    match error {
        relay_core::RelayError::ProviderStatus { status: 401 | 403, .. } => {
            CHATGPT_EXPIRED.to_string()
        }
        error => error.to_string(),
    }
}

// ---- Claude -----------------------------------------------------------------

fn claude_credentials_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join(".credentials.json"));
    }
    dirs::home_dir().map(|home| home.join(".claude").join(".credentials.json"))
}

/// Claude Code's OAuth session, read only.
fn claude_credentials() -> Option<Value> {
    let text = claude_credentials_text()?;
    serde_json::from_str(&text).ok()
}

#[cfg(target_os = "macos")]
fn claude_credentials_text() -> Option<String> {
    // Reading the password can show a Keychain prompt; bound the wait so the
    // CLI never hangs on it.
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let output = std::process::Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .output();
        let _ = sender.send(output);
    });
    if let Ok(Ok(output)) = receiver.recv_timeout(Duration::from_secs(60)) {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    std::fs::read_to_string(claude_credentials_path()?).ok()
}

#[cfg(not(target_os = "macos"))]
fn claude_credentials_text() -> Option<String> {
    std::fs::read_to_string(claude_credentials_path()?).ok()
}

async fn fetch_claude() -> UsageReport {
    let Some(credentials) = claude_credentials() else {
        return UsageReport::failed(
            "anthropic",
            "Claude login not found — open Claude Code once to sign in".to_string(),
        );
    };
    let oauth = &credentials["claudeAiOauth"];
    let Some(token) = oauth["accessToken"].as_str() else {
        return UsageReport::failed(
            "anthropic",
            "Claude login not found — open Claude Code once to sign in".to_string(),
        );
    };
    let plan = oauth["subscriptionType"].as_str().map(str::to_string);

    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("authorization"),
        match HeaderValue::from_str(&format!("Bearer {token}")) {
            Ok(value) => value,
            Err(error) => return UsageReport::failed("anthropic", error.to_string()),
        },
    );
    headers.insert(
        HeaderName::from_static("anthropic-beta"),
        HeaderValue::from_static("oauth-2025-04-20"),
    );
    headers.insert(
        HeaderName::from_static("user-agent"),
        HeaderValue::from_static("claude-code/2"),
    );

    let client = match HttpClient::new() {
        Ok(client) => client,
        Err(error) => return UsageReport::failed("anthropic", error.to_string()),
    };
    match client
        .get_json::<Value>(CLAUDE_USAGE_URL, &headers)
        .await
    {
        Ok(value) => {
            let mut report = parse_claude_usage(&value);
            report.plan = plan;
            report
        }
        Err(error) => UsageReport::failed("anthropic", claude_error(&error)),
    }
}

fn claude_error(error: &relay_core::RelayError) -> String {
    match error {
        relay_core::RelayError::ProviderStatus { status: 401 | 403, .. } => {
            CLAUDE_EXPIRED.to_string()
        }
        error => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODEX: &str = r#"{"plan_type":"plus","email":"user@example.com",
        "rate_limit":{"allowed":true,"limit_reached":false,
         "primary_window":{"used_percent":0,"limit_window_seconds":18000,
           "reset_after_seconds":16596,"reset_at":1793150938},
         "secondary_window":{"used_percent":3,"limit_window_seconds":604800,
           "reset_after_seconds":467830,"reset_at":1793602171}},
        "credits":{"has_credits":false,"unlimited":false,"balance":"0"}}"#;

    const CLAUDE: &str = r#"{"five_hour":{"utilization":23.0,
            "resets_at":"2026-10-28T00:10:00.199007+00:00"},
        "seven_day":{"utilization":13.0,
            "resets_at":"2026-10-30T19:00:00.199027+00:00"},
        "seven_day_opus":null,"seven_day_sonnet":null}"#;

    #[test]
    fn codex_usage_normalizes_both_windows() {
        let value: Value = serde_json::from_str(CODEX).unwrap();
        let report = parse_codex_usage(&value);
        assert_eq!(report.provider, "openai");
        assert_eq!(report.plan.as_deref(), Some("plus"));
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[0].label, "5h");
        assert_eq!(report.windows[0].window_minutes, 300);
        assert_eq!(report.windows[0].used_percent, 0.0);
        assert_eq!(
            report.windows[0].resets_at.as_deref(),
            Some("2026-10-28T01:28:58Z")
        );
        assert_eq!(report.windows[1].label, "weekly");
        assert_eq!(report.windows[1].window_minutes, 10_080);
        assert_eq!(report.windows[1].used_percent, 3.0);
        assert_eq!(
            report.windows[1].resets_at.as_deref(),
            Some("2026-11-02T06:49:31Z")
        );
    }

    #[test]
    fn codex_usage_tolerates_a_missing_window() {
        let value: Value = serde_json::from_str(
            r#"{"plan_type":"pro","rate_limit":{"primary_window":null,
                "secondary_window":{"used_percent":7,"limit_window_seconds":604800,
                "reset_at":0}}}"#,
        )
        .unwrap();
        let report = parse_codex_usage(&value);
        assert_eq!(report.plan.as_deref(), Some("pro"));
        assert_eq!(report.windows.len(), 1);
        assert_eq!(report.windows[0].label, "weekly");
        assert_eq!(report.windows[0].resets_at.as_deref(), Some("1970-01-01T00:00:00Z"));
    }

    #[test]
    fn claude_usage_normalizes_the_subscription_windows() {
        let value: Value = serde_json::from_str(CLAUDE).unwrap();
        let report = parse_claude_usage(&value);
        assert_eq!(report.provider, "anthropic");
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[0].label, "5h");
        assert_eq!(report.windows[0].window_minutes, 300);
        assert_eq!(report.windows[0].used_percent, 23.0);
        assert_eq!(
            report.windows[0].resets_at.as_deref(),
            Some("2026-10-28T00:10:00Z")
        );
        assert_eq!(report.windows[1].label, "weekly");
        assert_eq!(report.windows[1].window_minutes, 10_080);
        assert_eq!(report.windows[1].used_percent, 13.0);
    }

    #[test]
    fn claude_usage_adds_model_windows_when_present() {
        let value: Value = serde_json::from_str(
            r#"{"five_hour":null,
                "seven_day":{"utilization":5.5,"resets_at":"2026-10-30T19:00:00+00:00"},
                "seven_day_opus":{"utilization":40.0,
                    "resets_at":"2026-10-30T19:00:00+00:00"},
                "seven_day_sonnet":null}"#,
        )
        .unwrap();
        let report = parse_claude_usage(&value);
        let labels: Vec<_> = report.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["weekly", "weekly opus"]);
        assert_eq!(report.windows[0].used_percent, 5.5);
        assert_eq!(report.windows[1].used_percent, 40.0);
    }

    #[test]
    fn window_labels_cover_odd_lengths() {
        assert_eq!(window_label(300), "5h");
        assert_eq!(window_label(10_080), "weekly");
        assert_eq!(window_label(45), "45m");
        assert_eq!(window_label(120), "2h");
    }
}
