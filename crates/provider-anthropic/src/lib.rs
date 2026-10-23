//! Anthropic Messages API (and compatible gateways) over raw HTTP/SSE.

pub mod wire;

use std::time::Instant;

use futures::future::BoxFuture;
use futures::{stream, StreamExt};
use serde_json::{json, Value};

use relay_core::provider::EventStream;
use relay_core::transport::{join_url, HeaderMap, HeaderName, HeaderValue, SseStream};
use relay_core::{
    ChatRequest, ChatResponse, HttpClient, Model, ModelCapabilities, ProviderAdapter,
    ProviderConfig, ProviderId, RelayError, Result, StreamEvent, TestResult,
};

use wire::{build_messages_body, parse_message_response, parse_models, EventParser};

pub const API_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    config: ProviderConfig,
    http: HttpClient,
    api_key: Option<String>,
}

impl AnthropicProvider {
    pub fn new(config: ProviderConfig, api_key: Option<String>) -> Result<Self> {
        Ok(AnthropicProvider {
            config,
            http: HttpClient::new()?,
            api_key,
        })
    }

    /// `x-api-key` by default; gateways that expect a bearer token set
    /// `extra.auth = "bearer"` on the provider config.
    pub fn headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static(API_VERSION),
        );
        if let Some(key) = &self.api_key {
            let bearer = self.config.extra.get("auth").map(String::as_str) == Some("bearer");
            let (name, text) = if bearer {
                ("authorization", format!("Bearer {key}"))
            } else {
                ("x-api-key", key.clone())
            };
            let mut value = HeaderValue::from_str(&text)
                .map_err(|_| RelayError::Http("API key contains invalid characters".into()))?;
            value.set_sensitive(true);
            headers.insert(HeaderName::from_static(name), value);
        }
        for (name, value) in &self.config.default_headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| RelayError::Http(e.to_string()))?;
            let value =
                HeaderValue::from_str(value).map_err(|e| RelayError::Http(e.to_string()))?;
            headers.insert(name, value);
        }
        Ok(headers)
    }

    fn url(&self, path: &str) -> String {
        join_url(&self.config.base_url, path)
    }
}

pub fn events_from_frames(frames: SseStream) -> EventStream {
    let events = frames
        .scan(EventParser::default(), |parser, item| {
            let out: Vec<Result<StreamEvent>> = match item {
                Err(e) => vec![Err(e)],
                Ok(frame) if frame.data.trim().is_empty() => Vec::new(),
                Ok(frame) => match serde_json::from_str::<Value>(&frame.data) {
                    Ok(v) => parser.parse(&v).into_iter().map(Ok).collect(),
                    Err(e) => vec![Err(RelayError::parse("<sse>", e.to_string()))],
                },
            };
            futures::future::ready(Some(out))
        })
        .flat_map(stream::iter);
    Box::pin(events)
}

impl ProviderAdapter for AnthropicProvider {
    fn id(&self) -> &ProviderId {
        &self.config.id
    }

    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn discover_models(&self) -> BoxFuture<'_, Result<Vec<Model>>> {
        Box::pin(async move {
            let path = self.config.models_path.as_deref().unwrap_or("models");
            let value: Value = self
                .http
                .get_json(&self.url(path), &self.headers()?)
                .await?;
            Ok(parse_models(&self.config.id, &value))
        })
    }

    fn test_connection(&self, model: &str) -> BoxFuture<'_, Result<TestResult>> {
        let model = model.to_string();
        Box::pin(async move {
            let body = json!({"model": model, "max_tokens": 1,
                "messages": [{"role": "user", "content": "ping"}]});
            let started = Instant::now();
            let result = self
                .http
                .post_json::<Value>(&self.url("messages"), &self.headers()?, &body)
                .await;
            let latency_ms = started.elapsed().as_millis() as u64;
            match result {
                Ok(_) => Ok(TestResult {
                    ok: true,
                    latency_ms,
                    status: 200,
                    detail: "OK".into(),
                    model: Some(model),
                }),
                Err(RelayError::ProviderStatus {
                    status, message, ..
                }) => Ok(TestResult {
                    ok: false,
                    latency_ms,
                    status,
                    detail: message,
                    model: Some(model),
                }),
                Err(other) => Err(other),
            }
        })
    }

    fn capabilities(&self, _model: &str) -> ModelCapabilities {
        ModelCapabilities {
            vision: true,
            reasoning: true,
            ..ModelCapabilities::coding_defaults()
        }
    }

    fn stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<EventStream>> {
        Box::pin(async move {
            let body = build_messages_body(&request, true);
            let frames = self
                .http
                .post_sse(&self.url("messages"), &self.headers()?, &body)
                .await?;
            Ok(events_from_frames(frames))
        })
    }

    fn chat(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatResponse>> {
        Box::pin(async move {
            let body = build_messages_body(&request, false);
            let value: Value = self
                .http
                .post_json(&self.url("messages"), &self.headers()?, &body)
                .await?;
            parse_message_response(&value)
        })
    }
}
