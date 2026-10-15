//! Repair hold diagnostics are bounded by type/stage, not upstream error text.
use super::ports::DispatchError;
use crate::{state::StateError, supervisor::SupervisorError};

pub(crate) fn reason(error: &DispatchError) -> String {
    let cause = match error {
        DispatchError::PullRequest(lookup) => {
            format!("PR or check state could not be verified ({lookup})")
        }
        DispatchError::Claim(_) | DispatchError::ChangedClaim => {
            "issue claim changed or could not be re-read".into()
        }
        DispatchError::Worktree(_) => "worktree identity or tip did not verify".into(),
        DispatchError::Supervisor(error) => supervisor_cause(error).into(),
        DispatchError::State(StateError::LaunchBlocked) => {
            "recorded context does not allow a repair launch (ambiguous PR or unverified history)"
                .into()
        }
        DispatchError::State(_) => "task state could not be validated".into(),
        DispatchError::RetryHeld { .. } | DispatchError::ExistingPr => {
            "repair launch was refused".into()
        }
    };
    format!("PR repair held: {cause}")
}

/// Exhaustive on purpose: a new supervisor class must select a bounded code.
fn supervisor_cause(error: &SupervisorError) -> &'static str {
    match error {
        SupervisorError::PrRepairHeld(cause) => cause.detail(),
        SupervisorError::State(_) => "supervisor-state-unverified",
        SupervisorError::Worktree(_) => "supervisor-worktree-unverified",
        SupervisorError::Sql(_) => "supervisor-database-unavailable",
        SupervisorError::Config(_) => "supervisor-config-unverified",
        SupervisorError::Io(_) => "supervisor-io-unavailable",
        SupervisorError::Json(_) => "supervisor-json-invalid",
        SupervisorError::WorkerBuildHeld(_) => "worker-build-held",
        SupervisorError::Conflict => "launch-plan-conflict",
        SupervisorError::ReadyTimeout => {
            "supervisor-ready-timeout; launch accounting required before restore can retry"
        }
        SupervisorError::ExecutionUnavailable => "supervisor-execution-unavailable",
        SupervisorError::DispatchAborted => "supervisor-dispatch-aborted",
        SupervisorError::GateClosed => "supervisor-gate-closed",
        SupervisorError::StopUnavailable => "supervisor-stop-unavailable",
        SupervisorError::IdentityUnavailable => "supervisor-identity-unavailable",
        #[cfg(unix)]
        SupervisorError::StopSocketPathTooLong => "stop-socket-path-too-long",
    }
}

#[cfg(test)]
mod tests;
