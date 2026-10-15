use super::DispatchError;
use crate::{claim::ClaimError, state::StateError, supervisor::SupervisorError};

/// Bounded operator diagnostics: never print SQL, argv, external stderr or paths.
pub fn operator_error(error: &DispatchError) -> String {
    match error {
        DispatchError::State(error) | DispatchError::Supervisor(SupervisorError::State(error)) => {
            state_reason(error)
        }
        DispatchError::Claim(ClaimError::Changed) | DispatchError::ChangedClaim => {
            "claim drift: issue assignment or source evidence changed".into()
        }
        DispatchError::Claim(_) => "claim failed: verification unavailable".into(),
        DispatchError::Worktree(_) => "worktree failed: verification precondition".into(),
        DispatchError::ExistingPr => "PR present or ambiguous; absent PR required".into(),
        DispatchError::PullRequest(_) => "pull request lookup failed".into(),
        DispatchError::Supervisor(SupervisorError::WorkerBuildHeld(reason)) => reason.clone(),
        DispatchError::Supervisor(SupervisorError::Conflict) => {
            "worker or worktree ownership precondition failed".into()
        }
        DispatchError::Supervisor(_) => "worker launch failed".into(),
        // Fixed reconciliation messages, not supplied identities or external text.
        DispatchError::RetryHeld { reason } => reason.clone(),
    }
}

fn state_reason(error: &StateError) -> String {
    match error {
        StateError::Capacity { reserved, capacity } => {
            format!("capacity full: limit {capacity}, reserved or unresolved {reserved}")
        }
        StateError::CoordinatorBusy => "coordinator busy; try again".into(),
        StateError::LowDiskSpace {
            available,
            required,
        } => {
            format!("disk capacity insufficient: {available} bytes available, {required} required")
        }
        StateError::InvalidConfig => "configuration precondition failed".into(),
        StateError::LaunchBlocked => {
            "task phase, latest attempt, exit or reservation precondition failed".into()
        }
        StateError::InvalidSelection => "task selection missing or changed".into(),
        StateError::CapacityMismatch {
            configured,
            persisted,
        } => {
            format!("configured capacity {configured} differs from persisted capacity {persisted}")
        }
        _ => "state storage or integrity check failed".into(),
    }
}
