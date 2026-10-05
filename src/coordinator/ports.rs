use crate::{
    claim::{self, ClaimError},
    github::{
        project::ProjectReader,
        pull_request::{LookupError, PullRequestEvidence, PullRequestReader},
    },
    pr_evidence::{VerifiedOpenPr, expected_for_task},
    state::{StateError, StateStore},
    supervisor::{LaunchPlan, SupervisorError},
    worktree::WorktreeError,
};
use std::time::{SystemTime, UNIX_EPOCH};
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

/// Verify that the selected issue is still assigned solely to the configured
/// login before completion is accepted.
pub(crate) fn verify_completion_claim<P: ProjectReader>(
    projects: &mut P,
    selection: &crate::state::SelectionEvidence,
) -> Result<(), ClaimError> {
    let login = &selection.effective_config.assignment_login;
    if login.trim().is_empty() {
        return Err(ClaimError::Changed);
    }
    let (_, issue) = claim::fresh(projects, &selection.candidate)?;
    if issue.assignees != [login.as_str()] {
        return Err(ClaimError::Changed);
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) enum PrVerificationStage {
    Exit,
    Recovery,
}

impl PrVerificationStage {
    fn identity_failure(self) -> &'static str {
        match self {
            Self::Exit => "exit PR identity unavailable",
            Self::Recovery => "PR identity unavailable",
        }
    }

    fn evidence_failure(self) -> &'static str {
        match self {
            Self::Exit => "exit PR evidence unavailable",
            Self::Recovery => "PR evidence unavailable",
        }
    }

    fn verification_failure(self) -> &'static str {
        match self {
            Self::Exit => "exit PR verification failed",
            Self::Recovery => "PR verification failed",
        }
    }
}

pub(crate) fn verify_open_pr<Q: PullRequestReader>(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    prs: &mut Q,
    evidence: (PullRequestEvidence, u64),
    stage: PrVerificationStage,
) -> Result<VerifiedOpenPr, &'static str> {
    let login = prs
        .authenticated_identity()
        .map_err(|_| stage.identity_failure())?;
    let expected =
        expected_for_task(store, task_id, prs, &login).map_err(|_| stage.evidence_failure())?;
    let (pr, observed_at_unix_secs) = evidence;
    VerifiedOpenPr::from_matching(pr, &expected, &login, attempt_id, observed_at_unix_secs)
        .map_err(|_| stage.verification_failure())
}

pub(crate) fn observation_time() -> Result<u64, SupervisorError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SupervisorError::IdentityUnavailable)?
        .as_secs())
}
