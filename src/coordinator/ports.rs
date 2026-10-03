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
