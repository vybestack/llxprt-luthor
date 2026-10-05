use crate::{
    claim::ClaimError,
    github::pull_request::LookupError,
    state::{StateError, StateStore},
    supervisor::{LaunchPlan, SupervisorError},
    worktree::WorktreeError,
};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum DispatchError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Claim(#[from] ClaimError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    PullRequest(#[from] LookupError),
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error("retry held: {reason}")]
    RetryHeld { reason: String },
    #[error("claim changed before launch")]
    ChangedClaim,
    #[error("pull request is no longer absent")]
    ExistingPr,
}
pub trait SupervisorLauncher {
    fn launch(&mut self, store: &mut StateStore, plan: &LaunchPlan) -> Result<(), SupervisorError>;
}

pub(crate) fn completion_claim_failure_reason(error: &ClaimError) -> &'static str {
    match error {
        ClaimError::Changed => "completion claim changed",
        ClaimError::Source(_) => "completion claim read failed",
        _ => unreachable!("completion claim verification only returns source or changed errors"),
    }
}

pub(crate) fn dispatch_failure_reason(
    error: &DispatchError,
    launch_preflight_complete: bool,
) -> &'static str {
    // Keep the reason bounded to a stage/type: external error strings may carry credentials.
    match error {
        DispatchError::Claim(_) => "claim failed",
        DispatchError::Worktree(_) => "worktree failed",
        DispatchError::ChangedClaim => "prelaunch claim changed",
        DispatchError::ExistingPr => "prelaunch PR present",
        DispatchError::PullRequest(_) => "prelaunch PR read failed",
        DispatchError::Supervisor(SupervisorError::Conflict) if !launch_preflight_complete => {
            "launch preflight conflict"
        }
        #[cfg(unix)]
        DispatchError::Supervisor(SupervisorError::StopSocketPathTooLong)
            if !launch_preflight_complete =>
        {
            "launch preflight socket path too long"
        }
        DispatchError::Supervisor(SupervisorError::Io(_)) if !launch_preflight_complete => {
            "launch preflight storage unavailable"
        }
        DispatchError::Supervisor(_) => "launch preparation or dispatch failed",
        DispatchError::RetryHeld { .. } => "retry held before launch",
        DispatchError::State(_) => "state transition failed",
    }
}
