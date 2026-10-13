use crate::{
    github::pull_request::LookupError, state::StateError, supervisor::SupervisorError,
    worktree::WorktreeError,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CleanupError {
    #[error("cleanup held: {0}")]
    Conflict(&'static str),
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error(transparent)]
    PullRequest(#[from] LookupError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
