//! Turnout core: the platform-independent domain model shared by the desktop app,
//! the `turnout` CLI and the local gateway.
//!
//! Design rules:
//! - Providers are generic. Anything exposing an OpenAI-compatible or
//!   Anthropic-compatible HTTP/JSON/SSE API can be registered (EVREN is only a
//!   preset with extra endpoints, never a hard requirement).
//! - Harnesses are wired through executables and config contracts, never SDKs.
//! - Secrets never appear in config files, logs or plans; only credential
//!   references travel through this crate.

pub mod brand;
pub mod error;
pub mod format;
pub mod harness;
pub mod model;
pub mod plan;
pub mod provider;
pub mod quota;
pub mod session;
pub mod transport;

pub use brand::{APP_NAME, BIN_NAME, DATA_DIR_NAME};
pub use error::{RelayError, Result};
pub use format::ConfigFormat;
pub use harness::{
    ConfigFileSnapshot, ConfigSnapshot, HarnessAdapter, HarnessId, InstallSource, Installation,
    Route, RouteContext, Surface, Version, GATEWAY_TOKEN_ENV,
};
pub use model::{EndpointKind, Model, ModelCapabilities, Usage};
pub use plan::{
    short_hash, BackupId, ChangePlan, PlannedFileChange, Validation, ValidationLevel,
    ValidationMessage,
};
pub use provider::{
    CredentialRef, ProtocolDialect, ProviderAdapter, ProviderConfig, ProviderId, TestResult,
    CREDENTIAL_SERVICE_ID,
};
pub use session::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, MessageContent, Role, StopReason,
    StreamEvent, ToolCallDelta, ToolDef,
};
pub use quota::{HeaderSink, QuotaSink};
pub use transport::{redact_secrets, HttpClient, SseFrame};
