use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("session busy: {0}")]
    SessionBusy(String),

    #[error("session occupied: {session_id} held by {holder}")]
    SessionOccupied { session_id: String, holder: String },

    #[error("runtime is shutting down")]
    ShuttingDown,

    #[error("invalid argument: {0}")]
    Invalid(String),

    #[error("not configured: missing {0}")]
    NotConfigured(String),

    #[error("settings: {0}")]
    Settings(String),

    #[error("provider: {0}")]
    Provider(String),

    #[error("{0}")]
    Sidecar(String),

    #[error("{0}")]
    Corpus(String),

    #[error("store: {0}")]
    Store(String),

    #[error("cancelled")]
    Cancelled,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Self::Store(err.to_string())
    }
}

impl Error {
    pub fn is_not_configured(&self) -> bool {
        matches!(self, Self::NotConfigured(_))
    }

    pub fn is_context_overflow(&self) -> bool {
        match self {
            Self::Provider(s) => looks_like_context_overflow(s),
            _ => false,
        }
    }
}

pub fn looks_like_context_overflow(body: &str) -> bool {
    let l = body.to_ascii_lowercase();
    l.contains("context overflow")
        || l.contains("prompt too long")
        || l.contains("context_length")
        || l.contains("maximum context")
}
