use serde::{Deserialize, Serialize};

/// Stable, machine-readable error codes. CLI maps these to exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// 0-ish: not an error, but search produced nothing.
    NoResults,
    /// 3: caller lacks the permission (scoped token).
    PermissionDenied,
    /// 4: bad arguments.
    InvalidParams,
    /// 5: daemon unreachable / internal failure.
    DaemonUnavailable,
    Internal,
    NotFound,
    Unimplemented,
}

impl ErrorCode {
    /// CLI exit code contract (docs/技术方案-v0.1.md §5.2).
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::NoResults => 2,
            ErrorCode::PermissionDenied => 3,
            ErrorCode::InvalidParams => 4,
            ErrorCode::DaemonUnavailable => 5,
            ErrorCode::NotFound => 2,
            ErrorCode::Unimplemented => 5,
            ErrorCode::Internal => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct CoreError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl CoreError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), hint: None }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Structured JSON error envelope — never prose for agents.
    pub fn to_envelope(&self) -> serde_json::Value {
        serde_json::json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "hint": self.hint,
            }
        })
    }
}

impl From<rusqlite::Error> for CoreError {
    fn from(e: rusqlite::Error) -> Self {
        CoreError::new(ErrorCode::Internal, format!("storage: {e}"))
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(e: serde_json::Error) -> Self {
        CoreError::new(ErrorCode::InvalidParams, format!("json: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;
