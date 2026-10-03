use crate::{config::ConfigError, state::StateError, worktree::WorktreeError};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("launch plan does not match verified task or worktree")]
    Conflict,
    #[error(
        "supervisor READY handshake timed out; reservation held for identity-safe reconciliation"
    )]
    ReadyTimeout,
    #[error("supervisor execution is unavailable; reservation held for reconciliation")]
    ExecutionUnavailable,
    #[error("process gate closed without release")]
    GateClosed,
    #[error("stop cannot prove ownership; reservation held")]
    StopUnavailable,
    #[error("cannot establish process identity")]
    IdentityUnavailable,
    #[cfg(unix)]
    #[error("stop socket path exceeds Unix socket path capacity")]
    StopSocketPathTooLong,
}
