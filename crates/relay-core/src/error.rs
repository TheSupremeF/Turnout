use std::path::PathBuf;

/// Result alias used across every core crate.
pub type Result<T> = std::result::Result<T, RelayError>;

/// Errors surfaced by the tool. Messages are safe to show in the UI and CLI:
/// [`crate::transport::redact_secrets`] is applied at transport boundaries so
/// provider payloads never leak API keys into reports.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("http transport error: {0}")]
    Http(String),

    #[error("{provider} returned HTTP {status}: {message}")]
    ProviderStatus {
        provider: String,
        status: u16,
        message: String,
    },

    #[error("failed to parse {path}: {message}")]
    Parse { path: PathBuf, message: String },

    #[error("unsupported config format for {path}")]
    UnsupportedFormat { path: PathBuf },

    #[error("config key not found: {0}")]
    KeyNotFound(String),

    #[error("credential store error: {0}")]
    Credential(String),

    #[error("vault is locked — master password required")]
    VaultLocked,

    #[error("backup not found: {0}")]
    BackupNotFound(String),

    #[error("validation failed: {0}")]
    Validation(String),

    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    #[error("harness not found: {0}")]
    HarnessNotFound(String),

    #[error("model not found: {0}")]
    ModelNotFound(String),

    #[error("operation cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

impl RelayError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        RelayError::Io {
            path: path.into(),
            source,
        }
    }

    pub fn parse(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        RelayError::Parse {
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        RelayError::Other(message.into())
    }
}

impl From<std::io::Error> for RelayError {
    fn from(value: std::io::Error) -> Self {
        RelayError::Io {
            path: PathBuf::from("<unknown>"),
            source: value,
        }
    }
}
