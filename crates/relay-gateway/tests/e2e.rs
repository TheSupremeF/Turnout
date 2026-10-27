//! Real HTTP, no mocks of our own code: client -> gateway -> OpenAI-compatible
//! upstream (a scripted local server).

use std::sync::{Arc, Mutex};

use provider_openai::OpenAiCompatible;
use relay_core::{ProviderAdapter, ProviderConfig};
use relay_gateway::{Gateway, GatewayConfig, GatewayHandle, GatewayModelRoute};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TOKEN: &str = "gw-test-token";

/// Scripted upstream. Returns the port and a log of request bodies it received.
async fn upstream() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                // Grows with the request: the gateway accepts up to 64 MiB and
                // long-session tests send several MiB.
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = vec![0u8; 65536];
                loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let req = String::from_utf8_lossy(&buf).to_string();
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(body.clone());

                let (status, ctype, out) = if body.contains("ratelimit") {
                    (
                        "429 Too Many Requests",
                        "application/json",
                        r#"{"error":{"message":"slow down sk-upstream-secret-0123456789"}}"#
                            .to_string(),
                    )
                } else if body.contains("\"stream\":true") && body.contains("usetool") {
                    ("200 OK", "text/event-stream", [
                        r#"data: {"id":"1","model":"glm","choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"ls","arguments":""}}]}}]}"#,
                        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
                        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\".\"}"}}]},"finish_reason":"tool_calls"}]}"#,
                        r#"data: {"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":5}}"#,
                        "data: [DONE]",
                    ].map(|l| format!("{l}\n\n")).concat())
                } else if body.contains("\"stream\":true") {
                    ("200 OK", "text/event-stream", [
                        r#"data: {"id":"1","model":"glm","choices":[{"delta":{"reasoning_content":"hmm"}}]}"#,
                        r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#,
                        r#"data: {"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}"#,
                        r#"data: {"choices":[],"usage":{"prompt_tokens":4,"completion_tokens":2}}"#,
                        "data: [DONE]",
                    ].map(|l| format!("{l}\n\n")).concat())
                } else {
                    ("200 OK", "application/json", r#"{"id":"1","model":"glm","choices":[{"finish_reason":"stop","message":{"content":"pong"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.to_string())
                };
                let resp = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len());
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, seen)
}

async fn gateway() -> (GatewayHandle, Arc<Mutex<Vec<String>>>) {
    gateway_with_route(Some("glm-5.3".to_string()), vec!["glm-5.3".to_string()]).await
}

async fn gateway_with_route(
    model_override: Option<String>,
    allowed_models: Vec<String>,
) -> (GatewayHandle, Arc<Mutex<Vec<String>>>) {
    let (port, seen) = upstream().await;
    let config = ProviderConfig::openai_compatible("mock", format!("http://127.0.0.1:{port}/v1"));
    let provider = OpenAiCompatible::new(config, Some("upstream-key".into())).unwrap();
    let handle = Gateway::start(GatewayConfig {
        upstream: Arc::new(provider),
        model_routes: Vec::new(),
        token: TOKEN.to_string(),
        model_override,
        allowed_models,
        port: 0,
        anthropic_passthrough: None,
        request_log: None,
    })
    .await
    .unwrap();
    (handle, seen)
}

#[tokio::test]
async fn multi_model_routes_forward_selected_models_and_reject_others() {
    let (gw, seen) = gateway_with_route(
        None,
        vec!["deepseek-v4.1-flash".into(), "mimo-v2.6-pro".into()],
    )
    .await;
    let ok = post(
        &gw,
        "/v1/messages",
        json!({"model": "mimo-v2.6-pro", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(ok.status(), 200);
    assert!(seen.lock().unwrap()[0].contains("\"model\":\"mimo-v2.6-pro\""));

    let rejected = post(
        &gw,
        "/v1/messages",
        json!({"model": "not-enabled", "messages": []}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(rejected.status(), 400);
    assert_eq!(seen.lock().unwrap().len(), 1);
    gw.shutdown().await;
}

#[tokio::test]
async fn provider_aware_routes_select_the_right_upstream_and_model() {
    let (port_a, seen_a) = upstream().await;
    let (port_b, seen_b) = upstream().await;
    let config_a =
        ProviderConfig::openai_compatible("provider-a", format!("http://127.0.0.1:{port_a}/v1"));
    let config_b =
        ProviderConfig::openai_compatible("provider-b", format!("http://127.0.0.1:{port_b}/v1"));
    let provider_a: Arc<dyn ProviderAdapter> =
        Arc::new(OpenAiCompatible::new(config_a, Some("key-a".into())).unwrap());
    let provider_b: Arc<dyn ProviderAdapter> =
        Arc::new(OpenAiCompatible::new(config_b, Some("key-b".into())).unwrap());
    let handle = Gateway::start(GatewayConfig {
        upstream: provider_a.clone(),
        model_routes: vec![
            GatewayModelRoute::new("provider-a/shared", "real-a", provider_a),
            GatewayModelRoute::new("provider-b/shared", "real-b", provider_b),
        ],
        token: TOKEN.into(),
        model_override: None,
        allowed_models: vec!["provider-a/shared".into(), "provider-b/shared".into()],
        port: 0,
        anthropic_passthrough: None,
        request_log: None,
    })
    .await
    .unwrap();

    for public_model in ["provider-a/shared", "provider-b/shared"] {
        let response = post(
            &handle,
            "/v1/messages",
            json!({"model": public_model, "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), 200);
    }
    assert!(seen_a.lock().unwrap()[0].contains("\"model\":\"real-a\""));
    assert!(seen_b.lock().unwrap()[0].contains("\"model\":\"real-b\""));
    handle.shutdown().await;
}

fn post(gw: &GatewayHandle, path: &str, body: Value) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(format!("{}{}", gw.url(), path))
        .header("x-api-key", TOKEN)
        .json(&body)
}

#[tokio::test]
async fn binds_loopback_only() {
    let (gw, _) = gateway().await;
    assert!(gw.addr.ip().is_loopback());
    gw.shutdown().await;
}

#[tokio::test]
async fn claude_code_stream_text_with_reasoning_and_model_override() {
    let (gw, seen) = gateway().await;
    let body = json!({"model": "claude-sonnet-5-5", "max_tokens": 64, "stream": true,
        "messages": [{"role": "user", "content": "hi"}]});
    let resp = post(&gw, "/v1/messages", body).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let text = resp.text().await.unwrap();

    let pos = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("missing {needle} in:\n{text}"))
    };
    assert!(pos("event: message_start") < pos("\"thinking_delta\""));
    assert!(pos("\"thinking_delta\"") < pos("\"text\":\"Hel\""));
    assert!(pos("\"text\":\"Hel\"") < pos("\"text\":\"lo\""));
    assert!(pos("\"text\":\"lo\"") < pos("\"stop_reason\":\"end_turn\""));
    assert!(pos("\"stop_reason\":\"end_turn\"") < pos("event: message_stop"));
    assert!(text.contains("\"output_tokens\":2"));

    let upstream_body = seen.lock().unwrap()[0].clone();
    assert!(
        upstream_body.contains("\"model\":\"glm-5.3\""),
        "override not applied: {upstream_body}"
    );
    assert!(!upstream_body.contains("claude-sonnet"));
    gw.shutdown().await;
}

#[tokio::test]
async fn claude_code_tool_call_stream() {
    let (gw, _) = gateway().await;
    let body = json!({"model": "x", "max_tokens": 64, "stream": true, "messages": [{"role": "user", "content": "usetool"}],
        "tools": [{"name": "ls", "input_schema": {"type": "object"}}]});
    let text = post(&gw, "/v1/messages", body)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        text.contains(
            r#""content_block":{"id":"call_1","input":{},"name":"ls","type":"tool_use"}"#
        ),
        "{text}"
    );
    let partial: String = text
        .lines()
        .filter(|l| l.contains("input_json_delta"))
        .map(|l| {
            serde_json::from_str::<Value>(l.trim_start_matches("data: ")).unwrap()["delta"]
                ["partial_json"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(partial, r#"{"path":"."}"#);
    assert!(text.contains("\"stop_reason\":\"tool_use\""));
    assert!(text.contains("\"input_tokens\":11"));
    gw.shutdown().await;
}

#[tokio::test]
async fn non_streaming_message() {
    let (gw, _) = gateway().await;
    let resp = post(
        &gw,
        "/v1/messages",
        json!({"model": "x", "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .send()
    .await
    .unwrap();
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["type"], "message");
    assert_eq!(v["content"][0]["text"], "pong");
    assert_eq!(v["stop_reason"], "end_turn");
    gw.shutdown().await;
}

#[tokio::test]
async fn rejects_missing_and_wrong_tokens() {
    let (gw, seen) = gateway().await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/messages", gw.url());
    let body = json!({"model": "x", "messages": []});
    let none = client.post(&url).json(&body).send().await.unwrap();
    assert_eq!(none.status(), 401);
    let wrong = client
        .post(&url)
        .header("authorization", "Bearer nope")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    let v: Value = wrong.json().await.unwrap();
    assert_eq!(v["error"]["type"], "authentication_error");
    let bearer = client
        .post(&url)
        .header("authorization", format!("Bearer {TOKEN}"))
        .json(&json!({"model": "x", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        bearer.status(),
        200,
        "Bearer form (ANTHROPIC_AUTH_TOKEN) must work"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "unauthenticated calls never reach the provider"
    );
    gw.shutdown().await;
}

#[tokio::test]
async fn provider_status_is_preserved_and_secrets_are_redacted() {
    let (gw, _) = gateway().await;
    let resp = post(&gw, "/v1/messages", json!({"model": "x", "max_tokens": 8, "messages": [{"role": "user", "content": "ratelimit"}]}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 429);
    let text = resp.text().await.unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["error"]["type"], "rate_limit_error");
    assert!(!text.contains("sk-upstream-secret-0123456789"), "{text}");
    gw.shutdown().await;
}

#[tokio::test]
async fn openai_clients_get_openai_shaped_streams() {
    let (gw, _) = gateway().await;
    let body =
        json!({"model": "gpt", "stream": true, "messages": [{"role": "user", "content": "hi"}]});
    let text = post(&gw, "/v1/chat/completions", body)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        text.contains("\"content\":\"Hel\"") && text.contains("\"content\":\"lo\""),
        "{text}"
    );
    assert!(text.contains("\"finish_reason\":\"stop\""));
    assert!(text.contains("\"total_tokens\":6"));
    assert!(text.trim_end().ends_with("data: [DONE]"));
    gw.shutdown().await;
}

#[tokio::test]
async fn count_tokens_endpoint_answers() {
    let (gw, _) = gateway().await;
    let v: Value = post(
        &gw,
        "/v1/messages/count_tokens",
        json!({"model": "x", "messages": [{"role": "user", "content": "hello world"}]}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(v["input_tokens"].as_u64().unwrap() > 0);
    gw.shutdown().await;
}
#[tokio::test]
async fn codex_responses_stream_over_a_chat_completions_upstream() {
    let (gw, seen) = gateway().await;
    let body = json!({"model": "gpt-5", "stream": true, "instructions": "be brief",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}]});
    let text = post(&gw, "/v1/responses", body)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let pos = |n: &str| {
        text.find(n)
            .unwrap_or_else(|| panic!("missing {n}:\n{text}"))
    };
    assert!(pos("event: response.created") < pos("event: response.output_item.added"));
    assert!(pos("\"delta\":\"Hel\"") < pos("\"delta\":\"lo\""));
    assert!(pos("event: response.output_item.done") < pos("event: response.completed"));
    assert!(text.contains("\"total_tokens\":6"));
    let upstream = seen.lock().unwrap()[0].clone();
    assert!(
        upstream.contains("\"role\":\"system\"") && upstream.contains("be brief"),
        "{upstream}"
    );
    gw.shutdown().await;
}

/// Scripted Anthropic API. Records every raw request (headers and body) and
/// answers 401 to any credential containing `bad`.
async fn anthropic_upstream() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut total = 0;
                loop {
                    let n = sock.read(&mut buf[total..]).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    total += n;
                    let text = String::from_utf8_lossy(&buf[..total]).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if total >= end + 4 + len {
                            break;
                        }
                    }
                }
                let req = String::from_utf8_lossy(&buf[..total]).to_string();
                log.lock().unwrap().push(req.clone());
                let (status, out) = if req.to_lowercase().contains("bad") {
                    (
                        "401 Unauthorized",
                        r#"{"type":"error","error":{"type":"authentication_error","message":"nope"}}"#,
                    )
                } else {
                    (
                        "200 OK",
                        r#"{"id":"msg_native","type":"message","role":"assistant","model":"claude-sonnet-5-5","content":[{"type":"text","text":"native"}],"usage":{"input_tokens":1,"output_tokens":1}}"#,
                    )
                };
                let resp = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len());
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, seen)
}

/// Hybrid gateway: custom model `glm-5.3` plus passthrough to a fake Anthropic.
async fn hybrid_gateway() -> (
    GatewayHandle,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<String>>>,
) {
    let (custom_port, custom_seen) = upstream().await;
    let (native_port, native_seen) = anthropic_upstream().await;
    let config =
        ProviderConfig::openai_compatible("mock", format!("http://127.0.0.1:{custom_port}/v1"));
    let provider: Arc<dyn ProviderAdapter> =
        Arc::new(OpenAiCompatible::new(config, Some("upstream-key".into())).unwrap());
    let handle = Gateway::start(GatewayConfig {
        upstream: provider.clone(),
        model_routes: vec![GatewayModelRoute::new("glm-5.3", "glm-5.3", provider)],
        token: TOKEN.into(),
        model_override: Some("glm-5.3".into()),
        allowed_models: vec!["glm-5.3".into()],
        port: 0,
        anthropic_passthrough: Some(format!("http://127.0.0.1:{native_port}")),
        request_log: None,
    })
    .await
    .unwrap();
    (handle, custom_seen, native_seen)
}

fn claude_post(gw: &GatewayHandle, token: &str, model: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(format!("{}/v1/messages?beta=true", gw.url()))
        .header("authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .json(&json!({"model": model, "messages": [{"role": "user", "content": "hi"}]}))
}

#[tokio::test]
async fn official_claude_models_are_forwarded_untouched() {
    let (gw, custom_seen, native_seen) = hybrid_gateway().await;
    let response = claude_post(&gw, "sk-ant-oat-good", "claude-sonnet-5-5")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("msg_native"));

    let seen = native_seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "no validity probe for passthrough traffic");
    let request = seen[0].to_lowercase();
    assert!(request.starts_with("post /v1/messages?beta=true "));
    assert!(request.contains("authorization: bearer sk-ant-oat-good"));
    assert!(request.contains("anthropic-beta: oauth-2025-04-20"));
    assert!(request.contains("\"model\":\"claude-sonnet-5-5\""));
    assert!(custom_seen.lock().unwrap().is_empty());
    gw.shutdown().await;
}

#[tokio::test]
async fn custom_models_accept_a_verified_claude_credential_and_cache_it() {
    let (gw, custom_seen, native_seen) = hybrid_gateway().await;
    for _ in 0..2 {
        let response = claude_post(&gw, "sk-ant-oat-good", "glm-5.3")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
    assert_eq!(custom_seen.lock().unwrap().len(), 2);
    let probes = native_seen.lock().unwrap();
    assert_eq!(
        probes.len(),
        1,
        "second request must reuse the cached probe"
    );
    assert!(probes[0]
        .to_lowercase()
        .starts_with("post /v1/messages/count_tokens "));
    gw.shutdown().await;
}

#[tokio::test]
async fn custom_models_reject_unverified_or_missing_credentials() {
    let (gw, custom_seen, _native_seen) = hybrid_gateway().await;
    let rejected = claude_post(&gw, "sk-ant-oat-bad", "glm-5.3")
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 401);

    let anonymous = reqwest::Client::new()
        .post(format!("{}/v1/messages", gw.url()))
        .json(&json!({"model": "glm-5.3", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);

    // A claude-* request with a credential Anthropic rejects is Anthropic's
    // 401 to return; it must never fall through to the custom provider.
    let forwarded = claude_post(&gw, "sk-ant-oat-bad", "claude-sonnet-5-5")
        .send()
        .await
        .unwrap();
    assert_eq!(forwarded.status(), 401);
    assert!(custom_seen.lock().unwrap().is_empty());
    gw.shutdown().await;
}

#[tokio::test]
async fn gateway_token_still_works_beside_passthrough() {
    let (gw, custom_seen, _native_seen) = hybrid_gateway().await;
    let response = post(
        &gw,
        "/v1/messages",
        json!({"model": "glm-5.3", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(custom_seen.lock().unwrap().len(), 1);
    gw.shutdown().await;
}

/// An upstream that always answers 429, so the primary path of a route fails.
async fn rate_limited_upstream() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_string());
                let out = r#"{"error":{"message":"slow down"}}"#;
                let resp = format!(
                    "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                    out.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, seen)
}

#[tokio::test]
async fn a_rate_limited_route_falls_back_to_its_next_model() {
    let (limited_port, limited_seen) = rate_limited_upstream().await;
    let (healthy_port, healthy_seen) = upstream().await;
    let limited: Arc<dyn ProviderAdapter> = Arc::new(
        OpenAiCompatible::new(
            ProviderConfig::openai_compatible(
                "limited",
                format!("http://127.0.0.1:{limited_port}/v1"),
            ),
            Some("key-limited".into()),
        )
        .unwrap(),
    );
    let healthy: Arc<dyn ProviderAdapter> = Arc::new(
        OpenAiCompatible::new(
            ProviderConfig::openai_compatible(
                "healthy",
                format!("http://127.0.0.1:{healthy_port}/v1"),
            ),
            Some("key-healthy".into()),
        )
        .unwrap(),
    );
    let handle = Gateway::start(GatewayConfig {
        upstream: limited.clone(),
        model_routes: vec![GatewayModelRoute {
            public_model: "glm-5.3".into(),
            upstream_model: "glm-5.3".into(),
            upstream: limited.clone(),
            fallbacks: vec![GatewayModelRoute::new("mimo", "mimo-v2.6-pro", healthy)],
        }],
        token: TOKEN.into(),
        model_override: None,
        allowed_models: vec!["glm-5.3".into(), "mimo".into()],
        port: 0,
        anthropic_passthrough: None,
        request_log: None,
    })
    .await
    .unwrap();

    let response = post(
        &handle,
        "/v1/chat/completions",
        json!({"model": "glm-5.3", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .send()
    .await
    .unwrap();
    // The client sees a 200 under its own model id; the reroute stays internal.
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["model"], "glm-5.3");
    assert_eq!(limited_seen.lock().unwrap().len(), 1);
    assert_eq!(healthy_seen.lock().unwrap().len(), 1);
    assert!(healthy_seen.lock().unwrap()[0].contains("mimo-v2.6-pro"));
    handle.shutdown().await;
}

#[tokio::test]
async fn a_streaming_route_falls_back_before_the_first_frame() {
    let (limited_port, _) = rate_limited_upstream().await;
    let (healthy_port, healthy_seen) = upstream().await;
    let limited: Arc<dyn ProviderAdapter> = Arc::new(
        OpenAiCompatible::new(
            ProviderConfig::openai_compatible(
                "limited",
                format!("http://127.0.0.1:{limited_port}/v1"),
            ),
            Some("key-limited".into()),
        )
        .unwrap(),
    );
    let healthy: Arc<dyn ProviderAdapter> = Arc::new(
        OpenAiCompatible::new(
            ProviderConfig::openai_compatible(
                "healthy",
                format!("http://127.0.0.1:{healthy_port}/v1"),
            ),
            Some("key-healthy".into()),
        )
        .unwrap(),
    );
    let handle = Gateway::start(GatewayConfig {
        upstream: limited.clone(),
        model_routes: vec![GatewayModelRoute {
            public_model: "glm-5.3".into(),
            upstream_model: "glm-5.3".into(),
            upstream: limited,
            fallbacks: vec![GatewayModelRoute::new("mimo", "mimo-v2.6-pro", healthy)],
        }],
        token: TOKEN.into(),
        model_override: None,
        allowed_models: vec!["glm-5.3".into(), "mimo".into()],
        port: 0,
        anthropic_passthrough: None,
        request_log: None,
    })
    .await
    .unwrap();
    let response = post(
        &handle,
        "/v1/chat/completions",
        json!({"model": "glm-5.3", "stream": true, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(body.contains("Hel"), "{body}");
    assert_eq!(healthy_seen.lock().unwrap().len(), 1);
    handle.shutdown().await;
}

#[tokio::test]
async fn a_bad_request_is_not_retried_and_keeps_its_status() {
    let (limited_port, limited_seen) = rate_limited_upstream().await;
    let (healthy_port, healthy_seen) = upstream().await;
    let limited: Arc<dyn ProviderAdapter> = Arc::new(
        OpenAiCompatible::new(
            ProviderConfig::openai_compatible(
                "limited",
                format!("http://127.0.0.1:{limited_port}/v1"),
            ),
            Some("key-limited".into()),
        )
        .unwrap(),
    );
    let healthy: Arc<dyn ProviderAdapter> = Arc::new(
        OpenAiCompatible::new(
            ProviderConfig::openai_compatible(
                "healthy",
                format!("http://127.0.0.1:{healthy_port}/v1"),
            ),
            Some("key-healthy".into()),
        )
        .unwrap(),
    );
    let handle = Gateway::start(GatewayConfig {
        upstream: limited.clone(),
        model_routes: vec![GatewayModelRoute {
            public_model: "glm-5.3".into(),
            upstream_model: "glm-5.3".into(),
            upstream: limited,
            fallbacks: vec![GatewayModelRoute::new("mimo", "mimo-v2.6-pro", healthy)],
        }],
        token: TOKEN.into(),
        model_override: None,
        allowed_models: vec!["glm-5.3".into(), "mimo".into()],
        port: 0,
        anthropic_passthrough: None,
        request_log: None,
    })
    .await
    .unwrap();
    // A model outside the route is a client mistake, never a fallback trigger.
    let rejected = post(
        &handle,
        "/v1/chat/completions",
        json!({"model": "nope", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(rejected.status(), 400);
    assert!(limited_seen.lock().unwrap().is_empty());
    assert!(healthy_seen.lock().unwrap().is_empty());
    handle.shutdown().await;
}

#[tokio::test]
async fn every_served_request_leaves_one_usage_line() {
    let (port, _) = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("requests").join("codex.jsonl");
    let config = ProviderConfig::openai_compatible("mock", format!("http://127.0.0.1:{port}/v1"));
    let provider: Arc<dyn ProviderAdapter> =
        Arc::new(OpenAiCompatible::new(config, Some("upstream-key".into())).unwrap());
    let handle = Gateway::start(GatewayConfig {
        upstream: provider.clone(),
        model_routes: vec![GatewayModelRoute::new("glm-5.3", "glm-5.3", provider)],
        token: TOKEN.into(),
        model_override: None,
        allowed_models: vec!["glm-5.3".into()],
        port: 0,
        anthropic_passthrough: None,
        request_log: Some(log.clone()),
    })
    .await
    .unwrap();

    // Streaming: the usage frame arrives on its own event.
    let response = post(
        &handle,
        "/v1/chat/completions",
        json!({"model": "glm-5.3", "stream": true, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    handle.shutdown().await;

    let text = std::fs::read_to_string(&log).unwrap();
    let line: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(line["model"], "glm-5.3");
    assert_eq!(line["provider"], "mock");
    assert_eq!(line["stream"], true);
    assert_eq!(line["status"], "ok");
    assert_eq!(line["input_tokens"], 4);
    assert_eq!(line["output_tokens"], 2);
    assert!(line["first_frame_ms"].is_number());
    // No prompt or response text is ever recorded.
    assert!(!text.contains("hi"), "{text}");
}

#[tokio::test]
async fn a_request_body_cannot_choose_where_the_gateway_sends_it() {
    // The LiteLLM class of bug (GHSA-3cv6-jpf6-8222 / GHSA-hx8v-g79f-8w5f):
    // request-body routing fields override the configured upstream. Here the
    // only thing a body may pick is a model *id* from the configured routes;
    // every URL field is dropped during conversion, so the request still lands
    // on the provider Turnout was configured with.
    let (port, seen) = upstream().await;
    let config = ProviderConfig::openai_compatible("mock", format!("http://127.0.0.1:{port}/v1"));
    let provider = Arc::new(OpenAiCompatible::new(config, Some("upstream-key".into())).unwrap());
    let handle = Gateway::start(GatewayConfig {
        upstream: provider.clone(),
        model_routes: vec![GatewayModelRoute::new("glm-5.3", "glm-5.3", provider)],
        token: TOKEN.into(),
        model_override: None,
        allowed_models: vec!["glm-5.3".into()],
        port: 0,
        anthropic_passthrough: None,
        request_log: None,
    })
    .await
    .unwrap();

    let response = post(
        &handle,
        "/v1/chat/completions",
        json!({
            "model": "glm-5.3",
            "base_url": "http://169.254.169.254/latest/meta-data/",
            "api_base": "http://attacker.example/",
            "provider": "aws_bedrock",
            "user_config": {"credentials": "/etc/passwd"},
            "messages": [{"role": "user", "content": "hi"}],
        }),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    handle.shutdown().await;

    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1, "one upstream call");
    let forwarded = &bodies[0];
    for forbidden in ["169.254.169.254", "attacker.example", "aws_bedrock", "/etc/passwd"] {
        assert!(!forwarded.contains(forbidden), "`{forbidden}` reached upstream: {forwarded}");
    }
}

#[tokio::test]
async fn a_model_outside_the_route_never_reaches_an_upstream() {
    // Model ids are the one body field that does route. An unknown id must be
    // refused here, not forwarded in the hope some provider understands it.
    let (port, seen) = upstream().await;
    let config = ProviderConfig::openai_compatible("mock", format!("http://127.0.0.1:{port}/v1"));
    let provider = Arc::new(OpenAiCompatible::new(config, Some("upstream-key".into())).unwrap());
    let gw = Gateway::start(GatewayConfig {
        upstream: provider.clone(),
        model_routes: vec![GatewayModelRoute::new("glm-5.3", "glm-5.3", provider)],
        token: TOKEN.into(),
        model_override: None,
        allowed_models: vec!["glm-5.3".into()],
        port: 0,
        anthropic_passthrough: None,
        request_log: None,
    })
    .await
    .unwrap();
    let response = post(
        &gw,
        "/v1/chat/completions",
        json!({"model": "http://169.254.169.254/v1", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 400);
    assert!(seen.lock().unwrap().is_empty());
    gw.shutdown().await;
}

#[tokio::test]
async fn a_long_session_body_reaches_the_upstream() {
    let (gw, seen) = gateway().await;
    // 8 MiB is past axum's 2 MiB default but ordinary for a long Codex session.
    let long = "x".repeat(8 * 1024 * 1024);
    let response = post(
        &gw,
        "/v1/responses",
        json!({"model": "glm-5.3", "stream": true, "input": long}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await;
    assert_eq!(seen.lock().unwrap().len(), 1);
    gw.shutdown().await;
}

#[tokio::test]
async fn an_oversized_body_is_rejected_before_any_upstream_call() {
    let (gw, seen) = gateway().await;
    let huge = "x".repeat(relay_gateway::MAX_BODY_BYTES + 1);
    let response = post(
        &gw,
        "/v1/chat/completions",
        json!({"model": "glm-5.3", "messages": [{"role": "user", "content": huge}]}),
    )
    .send()
    .await;
    // Refused with 413, or the connection is cut while the body is still
    // uploading; never a panic, a hang or a forwarded request.
    match response {
        Ok(response) => assert_eq!(response.status(), 413),
        Err(error) => assert!(error.is_timeout() || error.is_request(), "{error}"),
    }
    assert!(seen.lock().unwrap().is_empty());
    gw.shutdown().await;
}
