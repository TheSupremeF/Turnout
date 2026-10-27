use std::collections::BTreeMap;
use std::fmt;

use futures::future::BoxFuture;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

use crate::error::Result;
use crate::model::{Model, ModelCapabilities};
use crate::session::{ChatRequest, ChatResponse, StreamEvent};

/// Provider identifier. Open-ended on purpose: any service exposing an
/// OpenAI-compatible or Anthropic-compatible HTTP API can be registered
/// (`openai`, `anthropic`, `evren`, `opencode-go`, `local-llama`, …).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    pub const OPENAI: &'static str = "openai";
    pub const ANTHROPIC: &'static str = "anthropic";
    pub const EVREN: &'static str = "evren";

    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into().trim().to_lowercase().replace([' ', '/'], "-");
        ProviderId(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ProviderId {
    fn from(value: &str) -> Self {
        ProviderId::new(value)
    }
}

impl From<String> for ProviderId {
    fn from(value: String) -> Self {
        ProviderId::new(value)
    }
}

/// Wire contract spoken to the provider. The generic `openai` dialect covers
/// every OpenAI-compatible service (OpenCode Go, OpenRouter, Ollama, Azure-style
/// gateways, EVREN, …); `anthropic` covers Messages API services.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProtocolDialect {
    /// `POST /v1/chat/completions` with OpenAI-style SSE chunks.
    OpenAiChat,
    /// `POST /v1/responses` with OpenAI Responses events.
    OpenAiResponses,
    /// `POST /v1/messages` with Anthropic Messages SSE events.
    AnthropicMessages,
}

impl ProtocolDialect {
    pub fn as_str(self) -> &'static str {
        match self {
            ProtocolDialect::OpenAiChat => "openai-chat",
            ProtocolDialect::OpenAiResponses => "openai-responses",
            ProtocolDialect::AnthropicMessages => "anthropic-messages",
        }
    }

    pub fn from_str_value(value: &str) -> Option<Self> {
        match value {
            "openai-chat" | "openai" | "chat" => Some(ProtocolDialect::OpenAiChat),
            "openai-responses" | "responses" => Some(ProtocolDialect::OpenAiResponses),
            "anthropic-messages" | "anthropic" | "messages" => {
                Some(ProtocolDialect::AnthropicMessages)
            }
            _ => None,
        }
    }
}

/// Where a provider's API key lives. Never carries the secret itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRef {
    /// Credential store service name. `relay` is an internal storage id kept
    /// for compatibility with existing credentials files; it is not a product
    /// name and must not be renamed.
    pub service: String,
    /// Credential store account name, e.g. `provider:openai`.
    pub account: String,
}

/// Internal credential-store service id. Existing `credentials.json` files are
/// filed under this key, so it must never change with the product name.
pub const CREDENTIAL_SERVICE_ID: &str = "relay";

impl CredentialRef {
    pub fn for_provider(provider: &ProviderId) -> Self {
        CredentialRef {
            service: CREDENTIAL_SERVICE_ID.to_string(),
            account: format!("provider:{}", provider.as_str()),
        }
    }
}

/// A registered provider connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: ProviderId,
    pub label: String,
    pub dialect: ProtocolDialect,
    /// Base URL without a trailing slash, e.g. `https://api.example.com/v1`.
    pub base_url: String,
    /// Secret reference; the key itself stays in the OS credential store.
    pub credential: CredentialRef,
    /// Environment variable name injected at harness launch (`turnout run`).
    /// Config files only ever receive this *name*, never the value.
    pub api_key_env: String,
    #[serde(default)]
    pub default_headers: BTreeMap<String, String>,
    /// Optional override for the model listing path (default `/models`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_path: Option<String>,
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
    /// Most requests the gateway sends to this provider at once; more wait in
    /// line. `None` is unlimited. Matters when several agents share one key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
}

impl ProviderConfig {
    /// A generic OpenAI-compatible provider. This is the default shape for any
    /// service that exposes `POST /v1/chat/completions` + `GET /v1/models`.
    pub fn openai_compatible(id: impl Into<ProviderId>, base_url: impl Into<String>) -> Self {
        let id = id.into();
        ProviderConfig {
            label: id.to_string(),
            credential: CredentialRef::for_provider(&id),
            api_key_env: format!("{}_API_KEY", id.as_str().to_uppercase().replace('-', "_")),
            id,
            dialect: ProtocolDialect::OpenAiChat,
            base_url: base_url.into(),
            default_headers: BTreeMap::new(),
            models_path: None,
            extra: BTreeMap::new(),
            max_concurrent: None,
        }
    }

    /// A provider speaking the Anthropic Messages API.
    pub fn anthropic(id: impl Into<ProviderId>, base_url: impl Into<String>) -> Self {
        let mut config = ProviderConfig::openai_compatible(id, base_url);
        config.dialect = ProtocolDialect::AnthropicMessages;
        config.api_key_env = "ANTHROPIC_AUTH_TOKEN".to_string();
        config
    }
}

/// Result of a connection test.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TestResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub status: u16,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Streaming events returned by [`ProviderAdapter::stream`].
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

/// Contract every provider adapter implements over raw HTTP/JSON/SSE —
/// no vendor SDK involved.
pub trait ProviderAdapter: Send + Sync {
    fn id(&self) -> &ProviderId;

    fn config(&self) -> &ProviderConfig;

    fn dialect(&self) -> ProtocolDialect {
        self.config().dialect
    }

    /// `GET /models` (path overridable per provider).
    fn discover_models(&self) -> BoxFuture<'_, Result<Vec<Model>>>;

    /// Cheap round-trip against the provider for the given model.
    fn test_connection(&self, model: &str) -> BoxFuture<'_, Result<TestResult>>;

    /// Local capability metadata for a model (from catalog or heuristics).
    fn capabilities(&self, model: &str) -> ModelCapabilities;

    /// Streaming chat completion.
    fn stream(&self, request: ChatRequest) -> BoxFuture<'_, Result<EventStream>>;

    /// Non-streaming chat completion.
    fn chat(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatResponse>>;
}
