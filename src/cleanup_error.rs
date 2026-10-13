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

impl CleanupError {
    /// Only fixed diagnostic text is safe for persisted/operator-visible output.
    /// In particular, IO, SQL and JSON Display implementations may echo paths or data.
    pub fn operator_reason(&self) -> String {
        match self {
            Self::Conflict(reason) => (*reason).into(),
            Self::Worktree(error) => worktree_reason(error),
            Self::State(error) => state_reason(error).into(),
            Self::Supervisor(error) => supervisor_reason(error),
            Self::PullRequest(error) => error.to_string(),
            Self::Io(error) => format!("cleanup IO failed ({:?})", error.kind()),
        }
    }
}

fn worktree_reason(error: &WorktreeError) -> String {
    match error {
        WorktreeError::Conflict(reason) | WorktreeError::Git(reason) => (*reason).into(),
        WorktreeError::Io(error) => format!("cleanup worktree IO failed ({:?})", error.kind()),
        WorktreeError::State(error) => state_reason(error).into(),
        WorktreeError::InvalidTaskId => "cleanup worktree invalid task id".into(),
        WorktreeError::NotClaimed => "cleanup worktree task not claimed".into(),
        WorktreeError::BaseFetch => "cleanup worktree base fetch failed".into(),
    }
}

fn state_reason(error: &StateError) -> &'static str {
    match error {
        StateError::Sql(_) => "cleanup state database failed",
        StateError::Io(_) => "cleanup state IO failed",
        StateError::Serialization(_) => "cleanup state serialization failed",
        StateError::InvalidSelection => "cleanup selection invalid",
        StateError::LaunchBlocked => "cleanup terminal task proof invalid",
        StateError::DuplicateTask(_, _) => "cleanup task identity duplicated",
        StateError::Capacity { .. } => "cleanup state capacity exhausted",
        StateError::InvalidCapacity => "cleanup state capacity invalid",
        StateError::LowDiskSpace { .. } => "cleanup disk space insufficient",
        StateError::InvalidConfig => "cleanup configuration invalid",
        StateError::InvalidTarget => "cleanup target invalid",
        StateError::AmbiguousTarget(_, _) => "cleanup target ambiguous",
        StateError::CapacityMismatch { .. } => "cleanup state capacity mismatch",
        StateError::UnsupportedDatabaseVersion(_) => "cleanup state database version unsupported",
    }
}

fn supervisor_reason(error: &SupervisorError) -> String {
    match error {
        SupervisorError::Worktree(error) => worktree_reason(error),
        SupervisorError::State(error) => state_reason(error).into(),
        SupervisorError::Io(error) => format!("cleanup supervisor IO failed ({:?})", error.kind()),
        SupervisorError::Sql(_) => "cleanup supervisor database failed".into(),
        SupervisorError::Json(_) => "cleanup supervisor serialization failed".into(),
        SupervisorError::Config(_) => "cleanup supervisor configuration invalid".into(),
        // The remaining supervisor variants have fixed, non-secret messages.
        SupervisorError::Conflict
        | SupervisorError::ReadyTimeout
        | SupervisorError::ExecutionUnavailable
        | SupervisorError::GateClosed
        | SupervisorError::StopUnavailable
        | SupervisorError::IdentityUnavailable => error.to_string(),
        #[cfg(unix)]
        SupervisorError::StopSocketPathTooLong => error.to_string(),
    }
}
