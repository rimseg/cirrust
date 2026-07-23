//! Unified error type for the backend. Implements `serde::Serialize` so errors
//! can cross the Tauri IPC boundary and surface as rejected promises in the UI.

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("invalid server URL: {0}")]
    Url(#[from] url::ParseError),

    #[error("credential store error: {0}")]
    Keyring(#[from] keyring::Error),

    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("not authenticated")]
    NotAuthenticated,

    #[allow(dead_code)] // reserved: poll currently returns Ok(None) while pending
    #[error("authentication is still pending")]
    LoginPending,

    #[error("server returned {status}: {body}")]
    Server { status: u16, body: String },

    #[error("{0}")]
    Message(String),
}

impl AppError {
    pub fn msg(m: impl Into<String>) -> Self {
        AppError::Message(m.into())
    }
}

/// Serialize as a tagged object so the frontend can branch on `kind`
/// (e.g. show a re-login prompt when `kind === "NotAuthenticated"`).
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let kind = match self {
            AppError::Http(_) => "Http",
            AppError::Url(_) => "Url",
            AppError::Keyring(_) => "Keyring",
            AppError::Io(_) => "Io",
            AppError::Serde(_) => "Serde",
            AppError::NotAuthenticated => "NotAuthenticated",
            AppError::LoginPending => "LoginPending",
            AppError::Server { .. } => "Server",
            AppError::Message(_) => "Message",
        };
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("kind", kind)?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
