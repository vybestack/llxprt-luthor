use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CliError {
    #[error("invalid command arguments")]
    Arguments,
    #[error("state database unavailable")]
    Database,
    #[error("task not found")]
    TaskNotFound,
    #[error("attempt not found")]
    AttemptNotFound,
    #[error("recorded log path is unsafe or unavailable")]
    UnsafeLog,
    #[error("output serialization failed")]
    Serialization,
}
