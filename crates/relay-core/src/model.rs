use serde::{Deserialize, Serialize};

use crate::provider::ProviderId;

/// A model exposed by a provider. The catalog is refreshable from the
/// provider's `/models` endpoint; local metadata only adds the capabilities
/// the endpoint does not describe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub display_name: String,
    pub provider: ProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    #[serde(default)]
    pub endpoint: EndpointKind,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Model {
    pub fn new(provider: ProviderId, id: impl Into<String>) -> Self {
        let id = id.into();
        Model {
            display_name: id.clone(),
            id,
            provider,
            context_window: None,
            max_output_tokens: None,
            capabilities: ModelCapabilities::default(),
            endpoint: EndpointKind::default(),
            deprecated: false,
            notes: None,
        }
    }

    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = name.into();
        self
    }

    pub fn with_context(mut self, context_window: u64, max_output: u64) -> Self {
        self.context_window = Some(context_window);
        self.max_output_tokens = Some(max_output);
        self
    }

    pub fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

/// What a model can do. Unknown fields default to `false` so catalogs stay
/// explicit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub chat: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub tool_calls: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub audio_input: bool,
    #[serde(default)]
    pub audio_output: bool,
    #[serde(default)]
    pub embeddings: bool,
    #[serde(default)]
    pub json_mode: bool,
}

impl ModelCapabilities {
    pub const fn coding_defaults() -> Self {
        ModelCapabilities {
            chat: true,
            streaming: true,
            tool_calls: true,
            reasoning: false,
            vision: false,
            audio_input: false,
            audio_output: false,
            embeddings: false,
            json_mode: true,
        }
    }

    pub const fn full() -> Self {
        ModelCapabilities {
            chat: true,
            streaming: true,
            tool_calls: true,
            reasoning: true,
            vision: true,
            audio_input: true,
            audio_output: true,
            embeddings: true,
            json_mode: true,
        }
    }
}

/// Which endpoint family serves the model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EndpointKind {
    #[default]
    ChatCompletions,
    Responses,
    Messages,
    Completions,
}

/// Token accounting shared by chat responses and stream usage events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}
