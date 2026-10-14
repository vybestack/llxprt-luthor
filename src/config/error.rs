use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid JSON configuration: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

impl ConfigError {
    /// Bounded message for operator entry points. Validation reasons are fixed
    /// strings that never carry configuration values, whereas serde errors can
    /// echo input bytes, so those are reduced to the subject alone.
    pub fn operator_message(&self, subject: &str) -> String {
        match self {
            Self::Invalid(reason) => format!("{subject} configuration invalid: {reason}"),
            Self::Json(_) => format!("{subject} configuration invalid"),
        }
    }
}
