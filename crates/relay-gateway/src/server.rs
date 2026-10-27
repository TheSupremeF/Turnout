use std::collections::HashMap;
use std::convert::Infallible;
use std::hash::{BuildHasher, RandomState};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::StreamExt;
use rand::RngCore;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit};
use tokio::task::JoinHandle;

use relay_core::provider::EventStream;
use relay_core::{
    redact_secrets, ChatResponse, Model, ProviderAdapter, RelayError, Result, StreamEvent,
};

use crate::request_log::RequestMeter;
use crate::traffic::TrafficPolicy;
use crate::convert::{
    anthropic_to_chat, chat_to_anthropic, chat_to_openai, chat_to_responses, openai_to_chat,
    responses_to_chat, AnthropicEncoder, OpenAiEncoder, ResponsesEncoder, StreamEncoder,
    ToolNamespaces,
};

const HEARTBEAT: Duration = Duration::from_secs(10);
/// Largest request body the gateway buffers. axum's 2 MiB default is far below
/// what a long Codex or Claude Code session sends (full history, file contents,
/// base64 images); the gateway only listens on loopback behind a token.
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
/// How long a verified Claude credential is trusted before it is probed again.
const NATIVE_CREDENTIAL_TTL: Duration = Duration::from_secs(600);
const NATIVE_CACHE_LIMIT: usize = 256;

pub struct GatewayConfig {
    pub upstream: Arc<dyn ProviderAdapter>,
    /// Provider-aware routes keyed by the model id exposed to the harness.
    /// Empty preserves the legacy single-upstream behavior.
    pub model_routes: Vec<GatewayModelRoute>,
    /// Bearer / `x-api-key` value clients must present.
    pub token: String,
    /// When set, every request is sent upstream with this model, whatever the
    /// client asked for (Claude Code issues background calls under Claude names).
    pub model_override: Option<String>,
    /// Models this harness route is allowed to use. Empty keeps legacy
    /// behavior and exposes the complete upstream catalog.
    pub allowed_models: Vec<String>,
    /// Loopback port; `0` picks a random free one. Harness configs embed the
    /// URL, so `turnout run` uses a stable port.
    pub port: u16,
    /// Base URL of the real Anthropic API (`https://api.anthropic.com`). When
    /// set, requests for official `claude-*` models that carry Claude's own
    /// credential are forwarded there untouched, and that same credential is
    /// accepted (after a free validity probe) for the other models. This keeps
    /// Claude Code's built-in model list working while custom models are added.
    pub anthropic_passthrough: Option<String>,
    /// JSONL file this gateway appends one line per completed request to.
    /// `None` disables request logging (tests, one-shot launches).
    pub request_log: Option<std::path::PathBuf>,
    /// Per-provider concurrency and same-model retries. Share one policy
    /// between the gateways of a process so each provider has one limit.
    pub traffic: TrafficPolicy,
}

#[derive(Clone)]
pub struct GatewayModelRoute {
    pub public_model: String,
    pub upstream_model: String,
    pub upstream: Arc<dyn ProviderAdapter>,
    /// Tried in order once the primary upstream has failed (after its own
    /// retries) before producing any output. Empty means the request never
    /// leaves the model the client asked for.
    pub fallbacks: Vec<GatewayModelRoute>,
}

impl GatewayModelRoute {
    /// A route without fallbacks.
    pub fn new(
        public_model: impl Into<String>,
        upstream_model: impl Into<String>,
        upstream: Arc<dyn ProviderAdapter>,
    ) -> Self {
        Self {
            public_model: public_model.into(),
            upstream_model: upstream_model.into(),
            upstream,
            fallbacks: Vec::new(),
        }
    }
}

pub struct Gateway;

impl Gateway {
    /// 256-bit random per-session token (hex).
    pub fn generate_token() -> String {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Start on a random free loopback port.
    pub async fn start(config: GatewayConfig) -> Result<GatewayHandle> {
        let port = config.port;
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| RelayError::Http(format!("cannot bind 127.0.0.1:{port}: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| RelayError::Http(e.to_string()))?;
        let token = config.token.clone();
        let anthropic = config
            .anthropic_passthrough
            .clone()
            .map(AnthropicNative::new)
            .transpose()?;
        let state = Arc::new(Shared { config, anthropic });
        let app = Router::new()
            .route("/", get(|| async { "turnout-gateway" }))
            .route("/v1/messages", post(messages))
            .route("/v1/messages/count_tokens", post(count_tokens))
            .route("/v1/chat/completions", post(chat_completions))
            .route("/v1/responses", post(responses))
            .route("/v1/models", get(models))
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
            .with_state(state);
        let (shutdown, rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
        });
        Ok(GatewayHandle {
            addr,
            token,
            shutdown: Some(shutdown),
            task,
        })
    }
}

pub struct GatewayHandle {
    pub addr: SocketAddr,
    pub token: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl GatewayHandle {
    /// Root URL without a version prefix, e.g. `http://127.0.0.1:51234`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

struct Shared {
    config: GatewayConfig,
    anthropic: Option<AnthropicNative>,
}

/// Verbatim forwarding to Anthropic for Claude Code's own requests.
///
/// The credential never leaves this process except toward the configured
/// Anthropic base URL, is never logged, and the connection ignores proxy
/// variables so a bearer token cannot be handed to an unexpected local proxy.
struct AnthropicNative {
    base: String,
    client: reqwest::Client,
    hasher: RandomState,
    verified: Mutex<HashMap<u64, Instant>>,
}

/// Headers that describe one hop and must not be copied across the proxy.
const HOP_HEADERS: [&str; 9] = [
    "host",
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "upgrade",
    "content-length",
    "accept-encoding",
];

impl AnthropicNative {
    fn new(base: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(300))
            .no_proxy()
            .build()
            .map_err(|e| RelayError::Http(e.to_string()))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            client,
            hasher: RandomState::new(),
            verified: Mutex::new(HashMap::new()),
        })
    }

    fn forwardable(headers: &HeaderMap) -> HeaderMap {
        let mut out = HeaderMap::new();
        for (name, value) in headers {
            if !HOP_HEADERS.contains(&name.as_str()) {
                out.append(name.clone(), value.clone());
            }
        }
        out
    }

    async fn forward(
        &self,
        method: reqwest::Method,
        uri: &Uri,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Response {
        let target = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
        let response = self
            .client
            .request(method, format!("{}{target}", self.base))
            .headers(Self::forwardable(headers))
            .body(body)
            .send()
            .await;
        let upstream = match response {
            Ok(response) => response,
            Err(error) => {
                return error_response(
                    Dialect::Anthropic,
                    502,
                    &format!(
                        "cannot reach Anthropic: {}",
                        redact_secrets(&error.to_string())
                    ),
                )
            }
        };
        let mut builder = Response::builder().status(upstream.status().as_u16());
        for (name, value) in upstream.headers() {
            if !HOP_HEADERS.contains(&name.as_str()) {
                builder = builder.header(name, value);
            }
        }
        builder
            .body(Body::from_stream(upstream.bytes_stream()))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
    }

    /// Whether the credential on this request is a valid Claude credential.
    ///
    /// `count_tokens` is free, so it doubles as a validity probe. Results are
    /// cached by a keyed hash of the credential; the credential itself is not
    /// stored. Unreachable Anthropic fails closed.
    async fn verify(&self, headers: &HeaderMap) -> bool {
        let Some(credential) = native_credential(headers) else {
            return false;
        };
        let key = self.hasher.hash_one(credential);
        if let Ok(cache) = self.verified.lock() {
            if cache
                .get(&key)
                .is_some_and(|at| at.elapsed() < NATIVE_CREDENTIAL_TTL)
            {
                return true;
            }
        }
        let mut probe = Self::forwardable(headers);
        probe.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        if !probe.contains_key("anthropic-version") {
            probe.insert(
                "anthropic-version",
                header::HeaderValue::from_static("2023-06-01"),
            );
        }
        let result = self
            .client
            .post(format!("{}/v1/messages/count_tokens", self.base))
            .headers(probe)
            .timeout(Duration::from_secs(15))
            .body(
                json!({"model": "claude-sonnet-5-5", "messages": [{"role": "user", "content": "."}]})
                    .to_string(),
            )
            .send()
            .await;
        let valid = result.is_ok_and(|response| {
            let status = response.status().as_u16();
            // 400/429 still prove the credential authenticated.
            response.status().is_success() || status == 400 || status == 429
        });
        if valid {
            if let Ok(mut cache) = self.verified.lock() {
                if cache.len() >= NATIVE_CACHE_LIMIT {
                    cache.retain(|_, at| at.elapsed() < NATIVE_CREDENTIAL_TTL);
                }
                cache.insert(key, Instant::now());
            }
        }
        valid
    }
}

/// Claude's own credential: an OAuth bearer or an Anthropic API key. The
/// gateway's own token is 64 hex characters and never matches.
fn native_credential(headers: &HeaderMap) -> Option<&str> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let key = headers.get("x-api-key").and_then(|v| v.to_str().ok());
    bearer
        .into_iter()
        .chain(key)
        .find(|value| value.starts_with("sk-ant-"))
}

fn is_official_model(model: &str) -> bool {
    model.starts_with("claude-") && !model.contains('/')
}

fn requested_model(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Gateway token, or a Claude credential Anthropic accepts.
async fn allowed(state: &Shared, headers: &HeaderMap) -> bool {
    if authorized(headers, &state.config.token) {
        return true;
    }
    match &state.anthropic {
        Some(native) => native.verify(headers).await,
        None => false,
    }
}

type AppState = State<Arc<Shared>>;

#[derive(Clone, Copy, PartialEq)]
enum Dialect {
    Anthropic,
    OpenAi,
    Responses,
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let presented = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.strip_prefix("Bearer ").unwrap_or(v))
        })
        .unwrap_or("");
    // Constant-time comparison; the token is the only thing guarding the key.
    presented.len() == token.len()
        && presented
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn error_type(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        503 | 529 => "overloaded_error",
        _ => "api_error",
    }
}

fn error_response(dialect: Dialect, status: u16, message: &str) -> Response {
    let message = redact_secrets(message);
    let body = match dialect {
        Dialect::Anthropic => {
            json!({"type": "error", "error": {"type": error_type(status), "message": message}})
        }
        Dialect::OpenAi | Dialect::Responses => {
            json!({"error": {"message": message, "type": error_type(status), "code": null}})
        }
    };
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        axum::Json(body),
    )
        .into_response()
}

fn upstream_error(dialect: Dialect, error: RelayError) -> Response {
    match error {
        // The provider's status code is preserved so clients can retry correctly.
        RelayError::ProviderStatus {
            status, message, ..
        } => error_response(dialect, status, &message),
        RelayError::Validation(message) => error_response(dialect, 400, &message),
        other => error_response(dialect, 502, &other.to_string()),
    }
}

async fn messages(State(state): AppState, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(native) = &state.anthropic {
        if native_credential(&headers).is_some() && is_official_model(&requested_model(&body)) {
            return native
                .forward(reqwest::Method::POST, &uri, &headers, body)
                .await;
        }
    }
    handle_chat(state, headers, body, Dialect::Anthropic).await
}

async fn chat_completions(State(state): AppState, headers: HeaderMap, body: Bytes) -> Response {
    handle_chat(state, headers, body, Dialect::OpenAi).await
}

async fn responses(State(state): AppState, headers: HeaderMap, body: Bytes) -> Response {
    handle_chat(state, headers, body, Dialect::Responses).await
}

async fn handle_chat(
    state: Arc<Shared>,
    headers: HeaderMap,
    body: Bytes,
    dialect: Dialect,
) -> Response {
    let config = &state.config;
    if !allowed(&state, &headers).await {
        return error_response(dialect, 401, "invalid gateway token");
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return error_response(dialect, 400, &format!("invalid JSON body: {e}")),
    };
    let requested = value.get("model").and_then(Value::as_str).unwrap_or("");
    if config.model_override.is_none() && !config.allowed_models.is_empty() {
        if !config.allowed_models.iter().any(|model| model == requested) {
            return error_response(
                dialect,
                400,
                &format!("model `{requested}` is not enabled for this Turnout route"),
            );
        }
    }
    let model_route = if config.model_override.is_none() {
        config
            .model_routes
            .iter()
            .find(|route| route.public_model == requested)
    } else {
        None
    };
    if config.model_override.is_none() && !config.model_routes.is_empty() && model_route.is_none() {
        return error_response(
            dialect,
            400,
            &format!("model `{requested}` has no provider route"),
        );
    }
    // The chosen route, then its fallbacks: an upstream that is overloaded,
    // rate-limited or unreachable hands the same request to the next model of
    // this route. Only failures *before the first frame* can be retried; once
    // the client has a partial stream the response is committed.
    let chain: Vec<&GatewayModelRoute> = model_route
        .map(|route| {
            std::iter::once(route)
                .chain(route.fallbacks.iter())
                .collect()
        })
        .unwrap_or_default();
    // The client sees the model id it asked for, not the upstream one, when a
    // provider-aware route served it.
    let response_model = model_route.is_some().then(|| requested.to_string());
    // Attempts in order: the requested model, then its configured fallbacks.
    // Without model routes there is one attempt on the default upstream.
    let attempts: Vec<(&Arc<dyn ProviderAdapter>, Option<&str>)> = if chain.is_empty() {
        vec![(&config.upstream, None)]
    } else {
        chain
            .iter()
            .map(|route| (&route.upstream, Some(route.upstream_model.as_str())))
            .collect()
    };
    let build = |upstream: &Arc<dyn ProviderAdapter>, upstream_model: Option<&str>| {
        let dialect_upstream = upstream.dialect();
        let model = upstream_model.or(config.model_override.as_deref());
        match dialect {
            Dialect::Anthropic => anthropic_to_chat(&value, dialect_upstream, model),
            Dialect::OpenAi => openai_to_chat(&value, dialect_upstream, model),
            Dialect::Responses => responses_to_chat(&value, dialect_upstream, model),
        }
    };

    let mut meter = RequestMeter::new(
        config.request_log.clone(),
        requested,
        value.get("stream").and_then(Value::as_bool).unwrap_or(false),
    );
    let traffic = &config.traffic;
    // Namespaced Codex tools travel under flat names; replies map them back.
    let namespaces = if dialect == Dialect::Responses {
        ToolNamespaces::from_responses_request(&value)
    } else {
        ToolNamespaces::default()
    };
    let mut last_error = None;

    for (index, (upstream, upstream_model)) in attempts.into_iter().enumerate() {
        let request = match build(upstream, upstream_model) {
            Ok(request) => request,
            Err(error) if index == 0 => {
                if let Some(meter) = meter.take() {
                    meter.finish(Some(&error.to_string()));
                }
                return upstream_error(dialect, error);
            }
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        let provider = upstream.id().as_str().to_string();
        if let Some(meter) = meter.as_mut() {
            meter.served_by(&provider, &request.model, index > 0);
        }
        // Same model first: a rate limit or an overloaded provider is usually
        // over in seconds, and switching models changes the answer.
        for attempt in 0..=traffic.retries {
            if attempt > 0 {
                tokio::time::sleep(traffic.backoff(attempt)).await;
                if let Some(meter) = meter.as_mut() {
                    meter.retried();
                }
            }
            let waiting = Instant::now();
            let permit = match traffic.acquire(&provider).await {
                Ok(permit) => permit,
                Err(error) => {
                    // Already waited the full queue timeout; retrying the same
                    // provider would only wait again.
                    last_error = Some(error);
                    break;
                }
            };
            if let Some(meter) = meter.as_mut() {
                meter.queued(waiting.elapsed());
            }
            let outcome = if request.stream {
                upstream.stream(request.clone()).await.map(Served::Stream)
            } else {
                upstream.chat(request.clone()).await.map(Served::Whole)
            };
            match outcome {
                Ok(served) => {
                    if index > 0 {
                        note_fallback(requested, &provider, &request.model);
                    }
                    return deliver(
                        served,
                        dialect,
                        response_model,
                        &request.model,
                        &namespaces,
                        meter,
                        permit,
                    );
                }
                Err(error) if retryable(&error) => {
                    note_retry(requested, &provider, &request.model, &error);
                    last_error = Some(error);
                }
                Err(error) => {
                    if let Some(meter) = meter.take() {
                        meter.finish(Some(&error.to_string()));
                    }
                    return upstream_error(dialect, error);
                }
            }
        }
    }
    let last = last_error
        .as_ref()
        .map(|error| error.to_string())
        .unwrap_or_else(|| "no provider could serve this route".to_string());
    if let Some(meter) = meter.take() {
        meter.finish(Some(&last));
    }
    match last_error {
        Some(error) => upstream_error(dialect, error),
        None => error_response(dialect, 502, "no provider could serve this route"),
    }
}

/// What an upstream returned for one attempt.
enum Served {
    Whole(ChatResponse),
    Stream(EventStream),
}

/// Turn a successful upstream reply into the client's dialect. The provider
/// slot (`permit`) stays taken until the reply, including a whole stream, is
/// delivered.
fn deliver(
    served: Served,
    dialect: Dialect,
    response_model: Option<String>,
    upstream_model: &str,
    namespaces: &ToolNamespaces,
    mut meter: Option<RequestMeter>,
    permit: Option<OwnedSemaphorePermit>,
) -> Response {
    match served {
        Served::Whole(mut response) => {
            if let Some(model) = response_model {
                response.model = model;
            }
            if let Some(mut meter) = meter.take() {
                meter.usage(response.usage.input_tokens, response.usage.output_tokens);
                meter.finish(None);
            }
            drop(permit);
            axum::Json(match dialect {
                Dialect::Anthropic => chat_to_anthropic(&response),
                Dialect::OpenAi => chat_to_openai(&response),
                Dialect::Responses => chat_to_responses(&response, namespaces),
            })
            .into_response()
        }
        Served::Stream(events) => {
            let model = response_model.unwrap_or_else(|| upstream_model.to_string());
            sse_response(events, encoder_for(dialect, model, namespaces), meter, permit)
        }
    }
}

/// One line in the Agent log, so a reroute is still visible afterwards.
fn note_fallback(requested: &str, provider: &str, upstream_model: &str) {
    eprintln!("turnout-agent: `{requested}` served by fallback {provider}:{upstream_model}");
}

fn note_retry(requested: &str, provider: &str, upstream_model: &str, error: &RelayError) {
    eprintln!(
        "turnout-agent: `{requested}` failed on {provider}:{upstream_model} ({})",
        redact_secrets(&error.to_string())
    );
}
/// Only a failure that waiting or another provider could plausibly fix is
/// retried; a bad request (400) or an auth mistake would fail the same way.
fn retryable(error: &RelayError) -> bool {
    match error {
        RelayError::Http(_) => true,
        RelayError::ProviderStatus { status, .. } => {
            matches!(status, 408 | 429 | 500 | 502 | 503 | 504 | 529)
        }
        _ => false,
    }
}

fn encoder_for(
    dialect: Dialect,
    response_model: String,
    namespaces: &ToolNamespaces,
) -> Box<dyn StreamEncoder> {
    match dialect {
        Dialect::Anthropic => Box::new(AnthropicEncoder::new(response_model)),
        Dialect::OpenAi => Box::new(OpenAiEncoder::new(response_model)),
        Dialect::Responses => Box::new(
            ResponsesEncoder::new(response_model).with_namespaces(namespaces.clone()),
        ),
    }
}

fn sse_response(
    mut events: EventStream,
    mut encoder: Box<dyn StreamEncoder>,
    mut meter: Option<RequestMeter>,
    permit: Option<OwnedSemaphorePermit>,
) -> Response {
    let (tx, rx) = mpsc::channel::<Bytes>(64);
    tokio::spawn(async move {
        // Held until this task ends: the provider slot is busy for as long as
        // the stream is, and is freed even when the client disconnects.
        let _permit = permit;
        macro_rules! send_all {
            ($frames:expr) => {
                for frame in $frames {
                    if tx.send(Bytes::from(frame)).await.is_err() {
                        return; // client went away: dropping `events` cancels the upstream call
                    }
                }
            };
        }
        send_all!(encoder.begin());
        let mut failure: Option<String> = None;
        let mut tick = tokio::time::interval(HEARTBEAT);
        tick.tick().await;
        loop {
            tokio::select! {
                next = events.next() => match next {
                    Some(Ok(event)) => {
                        match (&event, meter.as_mut()) {
                            // Usage arrives on its own frame; it is recorded, not counted as content.
                            (StreamEvent::Usage { usage }, Some(meter)) => {
                                meter.usage(usage.input_tokens, usage.output_tokens);
                            }
                            (StreamEvent::Ping, _) => {}
                            (_, Some(meter)) => meter.first_frame(),
                            (_, None) => {}
                        }
                        send_all!(encoder.push(event))
                    }
                    Some(Err(error)) => {
                        failure = Some(error.to_string());
                        send_all!(encoder.push(StreamEvent::Error {
                            message: redact_secrets(&error.to_string()), code: None, retryable: false }));
                        break;
                    }
                    None => break,
                },
                _ = tick.tick() => send_all!([encoder.heartbeat()]),
                _ = tx.closed() => {
                    // The harness disconnected mid-stream: still worth a line.
                    if let Some(meter) = meter.take() {
                        meter.finish(Some("client disconnected"));
                    }
                    return;
                }
            }
        }
        send_all!(encoder.finish());
        if let Some(meter) = meter.take() {
            meter.finish(failure.as_deref());
        }
    });

    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|bytes| (Ok::<_, Infallible>(bytes), rx))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn count_tokens(
    State(state): AppState,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(native) = &state.anthropic {
        if native_credential(&headers).is_some() && is_official_model(&requested_model(&body)) {
            return native
                .forward(reqwest::Method::POST, &uri, &headers, body)
                .await;
        }
    }
    if !allowed(&state, &headers).await {
        return error_response(Dialect::Anthropic, 401, "invalid gateway token");
    }
    // Providers expose no token counter; ~4 characters per token is the usual estimate.
    axum::Json(json!({"input_tokens": body.len().div_ceil(4)})).into_response()
}

async fn models(State(state): AppState, headers: HeaderMap) -> Response {
    if !allowed(&state, &headers).await {
        return error_response(Dialect::OpenAi, 401, "invalid gateway token");
    }
    if !state.config.model_routes.is_empty() {
        let mut models = Vec::new();
        for route in &state.config.model_routes {
            if models
                .iter()
                .any(|model: &Model| model.id == route.public_model)
            {
                continue;
            }
            let mut model = Model::new(route.upstream.id().clone(), route.public_model.clone());
            model.display_name = route.public_model.clone();
            models.push(model);
        }
        return models_response(&models);
    }
    match state.config.upstream.discover_models().await {
        Ok(models) => {
            let models = models
                .into_iter()
                .filter(|model| {
                    state.config.allowed_models.is_empty()
                        || state.config.allowed_models.iter().any(|id| id == &model.id)
                })
                .collect::<Vec<_>>();
            models_response(&models)
        }
        Err(e) => upstream_error(Dialect::OpenAi, e),
    }
}

fn models_response(models: &[Model]) -> Response {
    axum::Json(json!({
        "object": "list", "has_more": false,
        "data": models.iter().map(|m| json!({
            "id": m.id, "object": "model", "type": "model", "display_name": m.display_name,
            "owned_by": m.provider.as_str()})).collect::<Vec<_>>(),
    }))
    .into_response()
}
