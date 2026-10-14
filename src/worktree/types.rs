use crate::state::{StateError, WorktreeIdentity};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("invalid task id")]
    InvalidTaskId,
    #[error("task has no verified claim")]
    NotClaimed,
    #[error("worktree conflict: {0}")]
    Conflict(&'static str),
    #[error("git operation failed: {0}")]
    Git(&'static str),
    #[error("remote base fetch failed; dispatch held (no stale local fallback)")]
    BaseFetch,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    State(#[from] StateError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeResult {
    Created(WorktreeIdentity),
    Existing(WorktreeIdentity),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeInspection {
    UnverifiedPathPresent,
    UnverifiedPathAbsent,
    IdentityMatches,
    IdentityMismatch,
}
