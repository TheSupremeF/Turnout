//! EVREN: an OpenAI-compatible provider plus its extra endpoints
//! (`/quota`, `/terms/*`). Chat, models and streaming are delegated to the
//! generic adapter — EVREN is a preset, not a requirement.
//!
//! `base_url` must include the API version prefix (e.g. `https://…/v1`).

use futures::future::BoxFuture;
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
}
