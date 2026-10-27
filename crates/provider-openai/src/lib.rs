//! Generic OpenAI-compatible provider over raw HTTP/JSON/SSE (no SDK).
//!
//! Works for any service exposing `GET /models` and `POST /chat/completions`:
//! OpenAI, OpenCode Go, OpenRouter, Ollama, EVREN, custom gateways, …

pub mod wire;

use std::time::Instant;
use std::{collections::BTreeMap, mem};

use futures::future::BoxFuture;
use futures::{stream, StreamExt};
use serde_json::{json, Value};

use relay_core::provider::EventStream;
use relay_core::transport::{join_url, HeaderMap, HeaderName, HeaderValue, SseStream};
use relay_core::{
    ChatRequest, ChatResponse, ContentBlock, HttpClient, Model, ModelCapabilities, ProtocolDialect,
    ProviderAdapter, ProviderConfig, ProviderId, RelayError, Result, StopReason, StreamEvent,
    TestResult,
};

use wire::{
    build_chat_body, build_responses_body, parse_chat_response, parse_models,
    parse_responses_response, ChunkParser, ResponsesChunkParser,
};

pub struct OpenAiCompatible {
    config: ProviderConfig,
    http: HttpClient,
    api_key: Option<String>,
}

impl OpenAiCompatible {
    /// `api_key` comes from the credential store at call time; it is never
    /// persisted by this crate.
    pub fn new(config: ProviderConfig, api_key: Option<String>) -> Result<Self> {
        let chatgpt_oauth =
            config.extra.get("auth_mode").map(String::as_str) == Some("chatgpt-oauth");
        Ok(OpenAiCompatible {
            config,
            http: if chatgpt_oauth {
                HttpClient::direct()?
            } else {
                HttpClient::new()?
            },
            api_key,
        })
    }

    pub fn http(&self) -> &HttpClient {
        &self.http
    }

    /// Remember the rate-limit headers this provider sends on its responses.
    pub fn with_header_sink(
        mut self,
        sink: Option<std::sync::Arc<dyn relay_core::HeaderSink>>,
    ) -> Self {
        self.http = self.http.with_header_sink(sink);
        self
    }

    pub fn headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        if let Some(key) = &self.api_key {
            let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| RelayError::Http("API key contains invalid characters".into()))?;
            value.set_sensitive(true);
            headers.insert(reqwest_auth(), value);
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

fn reqwest_auth() -> HeaderName {
    HeaderName::from_static("authorization")
}

/// Convert an SSE frame stream into neutral events.
pub fn events_from_frames(frames: SseStream) -> EventStream {
    let events = frames
        .scan(ChunkParser::default(), |parser, item| {
            let out: Option<Vec<Result<StreamEvent>>> = match item {
                Err(e) => Some(vec![Err(e)]),
                Ok(frame) => {
                    let data = frame.data.trim();
                    if data == "[DONE]" {
                        None
                    } else if data.is_empty() {
                        Some(Vec::new())
                    } else {
                        match serde_json::from_str::<Value>(data) {
                            Ok(v) => Some(parser.parse(&v).into_iter().map(Ok).collect()),
                            Err(e) => Some(vec![Err(RelayError::parse("<sse>", e.to_string()))]),
                        }
                    }
                }
            };
            futures::future::ready(out)
        })
        .flat_map(stream::iter);
    Box::pin(events)
}

pub fn response_events_from_frames(frames: SseStream) -> EventStream {
    let events = frames
        .scan(ResponsesChunkParser::default(), |parser, item| {
            let out = match item {
                Err(error) => vec![Err(error)],
                Ok(frame) if frame.data.trim().is_empty() || frame.data.trim() == "[DONE]" => {
                    Vec::new()
                }
                Ok(frame) => match serde_json::from_str::<Value>(frame.data.trim()) {
                    Ok(value) => parser.parse(&value).into_iter().map(Ok).collect(),
                    Err(error) => {
                        vec![Err(RelayError::parse("<responses-sse>", error.to_string()))]
                    }
                },
            };
            futures::future::ready(Some(out))
        })
        .flat_map(stream::iter);
    Box::pin(events)
}

async fn collect_response_stream(mut events: EventStream) -> Result<ChatResponse> {
    #[derive(Default)]
    struct PendingTool {
        id: String,
        name: String,
        arguments: String,
    }

    let mut response = ChatResponse::default();
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tools: BTreeMap<usize, PendingTool> = BTreeMap::new();
    while let Some(event) = events.next().await {
        match event? {
            StreamEvent::Start { id, model } => {
                response.id = id;
                response.model = model;
            }
            StreamEvent::TextDelta { text: delta } => text.push_str(&delta),
            StreamEvent::ReasoningDelta { text: delta } => reasoning.push_str(&delta),
            StreamEvent::ToolCallDelta { delta } => {
                let tool = tools.entry(delta.index).or_default();
                if let Some(id) = delta.id {
                    tool.id = id;
                }
                if let Some(name) = delta.name {
                    tool.name = name;
                }
                if let Some(arguments) = delta.arguments_delta {
                    tool.arguments.push_str(&arguments);
                }
            }
            StreamEvent::Stop { reason } => response.stop_reason = reason,
            StreamEvent::Usage { usage } => response.usage = usage,
            StreamEvent::Ping => {}
            StreamEvent::Error { message, code, .. } => {
                return Err(RelayError::other(match code {
                    Some(code) => format!("{code}: {message}"),
                    None => message,
                }));
            }
        }
    }
    if !reasoning.is_empty() {
        response.content.push(ContentBlock::Reasoning {
            text: mem::take(&mut reasoning),
        });
    }
    if !text.is_empty() {
        response.content.push(ContentBlock::Text {
            text: mem::take(&mut text),
        });
    }
    for (_, tool) in tools {
        response.content.push(ContentBlock::ToolUse {
            id: tool.id,
            name: tool.name,
            input: serde_json::from_str(&tool.arguments).unwrap_or_else(|_| json!({})),
        });
    }
    if response.stop_reason == StopReason::EndTurn
        && response
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
    {
        response.stop_reason = StopReason::ToolUse;
    }
    Ok(response)
}

impl ProviderAdapter for OpenAiCompatible {
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
            let responses = self.config.dialect == ProtocolDialect::OpenAiResponses;
            let chatgpt_oauth =
                self.config.extra.get("auth_mode").map(String::as_str) == Some("chatgpt-oauth");
            let url = self.url(if responses {
                "responses"
            } else {
                "chat/completions"
            });
            let headers = self.headers()?;
            let started = Instant::now();
            if responses && chatgpt_oauth {
                let body = json!({
                    "model": model,
                    "input": [{
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": "Reply with exactly: pong"
                        }]
                    }],
                    "max_output_tokens": 16,
                    "store": false,
                    "stream": true
                });
                let result: Result<()> = async {
                    let mut frames = self.http.post_sse(&url, &headers, &body).await?;
                    while let Some(frame) = frames.next().await {
                        let frame = frame?;
                        let data = frame.data.trim();
                        if data.is_empty() || data == "[DONE]" {
                            continue;
                        }
                        let value: Value = serde_json::from_str(data).map_err(|error| {
                            RelayError::parse("<responses-sse>", error.to_string())
                        })?;
                        match value["type"].as_str() {
                            Some("response.completed") => return Ok(()),
                            Some("response.failed") | Some("response.incomplete") => {
                                let error = &value["response"]["error"];
                                return Err(RelayError::other(
                                    error["message"]
                                        .as_str()
                                        .unwrap_or("OpenAI response did not complete"),
                                ));
                            }
                            _ => {}
                        }
                    }
                    Err(RelayError::other(
                        "OpenAI stream ended before response.completed",
                    ))
                }
                .await;
                let latency_ms = started.elapsed().as_millis() as u64;
                return match result {
                    Ok(()) => Ok(TestResult {
                        ok: true,
                        latency_ms,
                        status: 200,
                        detail: "OK".to_string(),
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
                };
            }
            let mut body = if responses {
                json!({"model": model, "input": "ping", "max_output_tokens": 16, "store": false})
            } else {
                json!({"model": model,
                    "messages": [{"role": "user", "content": "ping"}], "max_tokens": 1})
            };
            let mut result = self.http.post_json::<Value>(&url, &headers, &body).await;
            // Newer OpenAI models reject `max_tokens`.
            if let Err(RelayError::ProviderStatus {
                status: 400,
                message,
                ..
            }) = &result
            {
                if message.contains("max_completion_tokens") {
                    body = json!({"model": model,
                        "messages": [{"role": "user", "content": "ping"}],
                        "max_completion_tokens": 16});
                    result = self.http.post_json::<Value>(&url, &headers, &body).await;
                }
            }
            let latency_ms = started.elapsed().as_millis() as u64;
            match result {
                Ok(_) => Ok(TestResult {
                    ok: true,
                    latency_ms,
                    status: 200,
                    detail: "OK".to_string(),
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

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        wire::infer_capabilities(model)
    }

    fn stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<EventStream>> {
        Box::pin(async move {
            if self.config.dialect == ProtocolDialect::OpenAiResponses {
                let frames = self
                    .http
                    .post_sse(
                        &self.url("responses"),
                        &self.headers()?,
                        &build_responses_body(&request, true),
                    )
                    .await?;
                return Ok(response_events_from_frames(frames));
            }
            let url = self.url("chat/completions");
            let headers = self.headers()?;
            let body = build_chat_body(&request, true);
            match self.http.post_sse(&url, &headers, &body).await {
                Ok(frames) => Ok(events_from_frames(frames)),
                // Some OpenAI-compatible servers reject `stream_options`.
                // Usage is then simply unavailable, which the encoders tolerate.
                Err(RelayError::ProviderStatus {
                    status: 400,
                    ref message,
                    ..
                }) if message.contains("stream_options") => {
                    let mut retry = body.clone();
                    if let Some(obj) = retry.as_object_mut() {
                        obj.remove("stream_options");
                    }
                    let frames = self.http.post_sse(&url, &headers, &retry).await?;
                    Ok(events_from_frames(frames))
                }
                Err(other) => Err(other),
            }
        })
    }

    fn chat(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatResponse>> {
        Box::pin(async move {
            if self.config.dialect == ProtocolDialect::OpenAiResponses {
                if self.config.extra.get("auth_mode").map(String::as_str) == Some("chatgpt-oauth") {
                    return collect_response_stream(self.stream(request).await?).await;
                }
                let value = self
                    .http
                    .post_json(
                        &self.url("responses"),
                        &self.headers()?,
                        &build_responses_body(&request, false),
                    )
                    .await?;
                return parse_responses_response(&value);
            }
            let url = self.url("chat/completions");
            let headers = self.headers()?;
            let body = build_chat_body(&request, false);
            let value: Value = match self.http.post_json(&url, &headers, &body).await {
                Ok(value) => value,
                // Newer OpenAI models renamed `max_tokens`; older servers
                // reject `max_completion_tokens`.
                Err(RelayError::ProviderStatus {
                    status: 400,
                    ref message,
                    ..
                }) if message.contains("max_tokens")
                    || message.contains("max_completion_tokens") =>
                {
                    let mut retry = body.clone();
                    if let Some(obj) = retry.as_object_mut() {
                        if obj.contains_key("max_tokens") {
                            let value = obj.remove("max_tokens");
                            obj.insert("max_completion_tokens".to_string(), value.unwrap());
                        } else {
                            let value = obj.remove("max_completion_tokens");
                            obj.insert("max_tokens".to_string(), value.unwrap());
                        }
                    }
                    self.http.post_json(&url, &headers, &retry).await?
                }
                Err(other) => return Err(other),
            };
            parse_chat_response(&value)
        })
    }
}
