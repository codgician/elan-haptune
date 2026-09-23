use serde::Serialize;
use std::{fmt, io};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Serialize)]
pub struct Error {
    pub kind: &'static str,
    pub message: String,
    pub exit_code: u8,
}

impl Error {
    pub fn new(code: u8, kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            exit_code: code,
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(2, "invalid_request", message)
    }
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(1, "protocol", message)
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(5, "unsupported", message)
    }
    pub fn context(mut self, context: impl fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }
    pub fn io(context: impl fmt::Display, error: io::Error) -> Self {
        let kind = if error.kind() == io::ErrorKind::PermissionDenied {
            "permission"
        } else {
            "io"
        };
        Self::new(1, kind, format!("{context}: {error}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
