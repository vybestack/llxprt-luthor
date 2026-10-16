//! Expected admission refusals are task-local. Storage, malformed upstream
//! data, and errors after a launch intent remain fatal; never infer quiescence.
use super::{diagnostics::scheduling_reason, ports::DispatchError};
use crate::{
    claim::ClaimError,
    github::{
        project::{ProjectError, ReadCategory},
        pull_request::ErrorCategory,
    },
    state::{StateError, StateStore, journal},
    supervisor::SupervisorError,
    worktree::WorktreeError,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAdmissionFailure {
    pub task_id: String,
    pub attempt_id: String,
    pub check: String,
    pub reason: String,
    pub differing_fields: Vec<String>,
}

impl TaskAdmissionFailure {
    pub(crate) fn record(
        store: &mut StateStore,
        task: &str,
        attempt: &str,
        check: &str,
        error: &DispatchError,
    ) -> Result<Self, StateError> {
        let failure = Self {
            task_id: task.into(),
            attempt_id: attempt.into(),
            check: check.into(),
            reason: scheduling_reason(error),
            differing_fields: match error {
                DispatchError::ConfigMismatch { fields } => fields.clone(),
                _ => Vec::new(),
            },
        };
        journal::record_evidence(
            store,
            task,
            Some(attempt),
            "scheduling_admission_failed",
            &serde_json::to_string(&failure)?,
        )?;
        Ok(failure)
    }
}

pub(crate) fn task_local(error: &DispatchError) -> bool {
    match error {
        DispatchError::RetryHeld { .. }
        | DispatchError::ConfigMismatch { .. }
        | DispatchError::ChangedClaim
        | DispatchError::ExistingPr => true,
        DispatchError::Claim(error) => match error {
            ClaimError::State(_) => false,
            ClaimError::Source(ProjectError::Read(error) | ProjectError::IssueRead(error)) => {
                error.category != ReadCategory::Malformed
            }
            ClaimError::Source(_) => false,
            ClaimError::PullRequest(error) => error.category != ErrorCategory::Malformed,
            _ => true,
        },
        DispatchError::PullRequest(error) => error.category != ErrorCategory::Malformed,
        DispatchError::Worktree(
            WorktreeError::Conflict(_)
            | WorktreeError::BaseFetch
            | WorktreeError::Git(_)
            | WorktreeError::NotClaimed,
        ) => true,
        DispatchError::Supervisor(SupervisorError::WorkerBuildHeld(_)) => true,
        #[cfg(unix)]
        DispatchError::Supervisor(SupervisorError::StopSocketPathTooLong) => true,
        _ => false,
    }
}
