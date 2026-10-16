use crate::{
    claim::{self, ClaimError},
    github::{
        project::ProjectReader,
        pull_request::{LookupError, PullRequestEvidence, PullRequestReader},
    },
    ownership::WorktreeOwner,
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
    #[error("retry worktree ownership unavailable")]
    RetryOwnershipUnavailable,
    #[error("retry held: {reason}")]
    RetryHeld { reason: String },
    #[error("retry configuration mismatch: {fields:?}")]
    ConfigMismatch { fields: Vec<String> },
    #[error("claim changed before launch")]
    ChangedClaim,
    #[error("pull request is no longer absent")]
    ExistingPr,
}
pub trait SupervisorLauncher {
    fn launch(
        &mut self,
        store: &mut StateStore,
        plan: &LaunchPlan,
        ownership: &WorktreeOwner,
    ) -> Result<(), SupervisorError>;
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
        DispatchError::Worktree(WorktreeError::BaseFetch) => "remote base fetch failed",
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
        DispatchError::RetryOwnershipUnavailable => "retry worktree ownership unavailable",
        DispatchError::ConfigMismatch { .. } => "retry configuration mismatch",
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

/// Finishing workers must supply both exact tracker references and the live tip.
/// Do not reuse this observation as terminal process or completion proof.
pub(crate) fn verify_finishing_pr<Q: PullRequestReader>(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    prs: &mut Q,
    evidence: (PullRequestEvidence, u64),
) -> Result<VerifiedOpenPr, &'static str> {
    let selection = crate::state::task_records::selection_evidence(store, task_id)
        .map_err(|_| "PR selection unavailable")?
        .ok_or("PR selection unavailable")?;
    let candidate = &selection.candidate;
    let closing = crate::worker_instructions::closing_reference(
        &candidate.repository,
        &candidate.mapping.code_repository,
        candidate.issue_number,
    );
    if !evidence.0.body.lines().any(|line| line == closing) {
        return Err("PR closing reference missing");
    }
    let proof = verify_open_pr(
        store,
        task_id,
        attempt_id,
        prs,
        evidence,
        PrVerificationStage::Exit,
    )?;
    verify_local_pr_tip(store, task_id, &proof)?;
    Ok(proof)
}

/// A PR on the task branch is not its expected finish unless GitHub has the
/// exact live local tip. The immutable dispatch snapshot may precede commits
/// made by the worker, so inspect the owned worktree rather than its saved SHA.
fn verify_local_pr_tip(
    store: &StateStore,
    task_id: &str,
    proof: &VerifiedOpenPr,
) -> Result<(), &'static str> {
    let identity = crate::worktree::verify_existing_worktree(store, task_id)
        .map_err(|_| "PR local worktree unavailable")?;
    if identity.head != proof.head_commit_sha {
        return Err("PR head differs from local tip");
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ScheduleError {
    #[error("task {task_id} attempt {attempt_id} check {check}: {source}")]
    Task {
        task_id: String,
        attempt_id: String,
        check: &'static str,
        source: Box<DispatchError>,
    },
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("ID generation failed: {0}")]
    Id(#[from] std::io::Error),
}

/// Only idempotent reads may be deferred. Never retry a failed/uncertain launch
/// or assignment by classifying its error text as a transient read failure.
pub(crate) fn transient_read(error: &DispatchError) -> bool {
    use crate::github::{
        project::{ProjectError, ReadCategory},
        pull_request::ErrorCategory,
    };
    match error {
        DispatchError::PullRequest(error) => matches!(
            error.category,
            ErrorCategory::Transport | ErrorCategory::RateLimit
        ),
        DispatchError::Claim(ClaimError::Source(
            ProjectError::Read(error) | ProjectError::IssueRead(error),
        )) => matches!(
            error.category,
            ReadCategory::Transport | ReadCategory::RateLimit
        ),
        _ => false,
    }
}

/// Capacity is always waitable; transient reads are waitable only when the
/// operator decision has already been committed to the durable inbox.
pub(crate) fn defer_resume_error(error: &DispatchError, durable_request: bool) -> bool {
    matches!(error, DispatchError::State(StateError::Capacity { .. }))
        || durable_request && transient_read(error)
}
