use super::ports::{DispatchError, ScheduleError};
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
        DispatchError::Claim(ClaimError::Dependencies) => "waiting for issue dependencies".into(),
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
        DispatchError::RetryOwnershipUnavailable => "retry worktree ownership unavailable".into(),
        DispatchError::ConfigMismatch { fields } => {
            format!("retry configuration mismatch: {}", fields.join(", "))
        }
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

/// Preserve the failing typed check without exposing raw SQL, paths, argv or
/// upstream response bodies. Unlike a generic stage label, these diagnostics
/// distinguish the actual refusal or fatal cause.
pub(crate) fn scheduling_reason(error: &DispatchError) -> String {
    use crate::worktree::WorktreeError;
    match error {
        DispatchError::Supervisor(SupervisorError::Io(error)) => {
            format!(
                "storage I/O failed: {:?} (OS code {:?})",
                error.kind(),
                error.raw_os_error()
            )
        }
        DispatchError::Supervisor(SupervisorError::Sql(error)) => {
            format!("database operation failed: {:?}", error.sqlite_error_code())
        }
        DispatchError::Worktree(WorktreeError::Io(error)) => {
            format!(
                "worktree I/O failed: {:?} (OS code {:?})",
                error.kind(),
                error.raw_os_error()
            )
        }
        DispatchError::State(error)
        | DispatchError::Supervisor(SupervisorError::State(error))
        | DispatchError::Worktree(WorktreeError::State(error))
        | DispatchError::Claim(ClaimError::State(error)) => scheduling_state_reason(error),
        DispatchError::Supervisor(SupervisorError::Json(_)) => "recorded JSON is invalid".into(),
        DispatchError::Claim(ClaimError::Source(error)) => source_reason(error),
        DispatchError::Claim(ClaimError::Changed | ClaimError::Dependencies) => {
            operator_error(error)
        }
        DispatchError::Claim(
            ClaimError::Assignment
            | ClaimError::IntentExists
            | ClaimError::Verify
            | ClaimError::ExistingPr,
        ) => error.to_string(),
        DispatchError::PullRequest(error)
        | DispatchError::Claim(ClaimError::PullRequest(error)) => {
            format!(
                "pull request lookup failed: {:?} {} (HTTP {:?})",
                error.category, error.code, error.status
            )
        }
        DispatchError::Worktree(error) => error.to_string(),
        DispatchError::Supervisor(SupervisorError::Config(error)) => {
            error.operator_message("retry")
        }
        DispatchError::Supervisor(error) => error.to_string(),
        _ => operator_error(error),
    }
}

impl ScheduleError {
    /// Cycle reports and stderr use the same bounded, contextual explanation.
    pub fn diagnostic(&self) -> serde_json::Value {
        match self {
            Self::Task {
                task_id,
                attempt_id,
                check,
                source,
            } => serde_json::json!({
                "task_id":task_id,"attempt_id":attempt_id,"check":check,
                "reason":scheduling_reason(source),
            }),
            Self::Dispatch(error) => {
                serde_json::json!({"check":"scheduling", "reason":scheduling_reason(error)})
            }
            Self::State(error) => {
                serde_json::json!({"check":"scheduling_state", "reason":scheduling_state_reason(error)})
            }
            Self::Id(error) => {
                serde_json::json!({"check":"id_generation", "reason":format!("ID generation failed: {:?}", error.kind())})
            }
        }
    }
}

pub(crate) fn resume_failure_reason(error: &DispatchError) -> &str {
    match error {
        DispatchError::Claim(_) | DispatchError::ChangedClaim => "resume claim changed",
        DispatchError::ExistingPr => "resume PR present",
        DispatchError::PullRequest(_) => "resume PR read failed",
        DispatchError::Supervisor(SupervisorError::WorkerBuildHeld(detail)) => detail,
        DispatchError::Supervisor(_) => "resume preparation or dispatch failed",
        DispatchError::RetryHeld { .. } => "retry held before launch",
        DispatchError::RetryOwnershipUnavailable => "retry worktree ownership unavailable",
        DispatchError::ConfigMismatch { .. } => "retry configuration mismatch",
        DispatchError::State(_) => "resume state transition failed",
        DispatchError::Worktree(_) => "resume worktree failed",
    }
}

fn scheduling_state_reason(error: &StateError) -> String {
    match error {
        StateError::Sql(error) => {
            format!("database operation failed: {:?}", error.sqlite_error_code())
        }
        StateError::Io(error) => format!(
            "storage I/O failed: {:?} (OS code {:?})",
            error.kind(),
            error.raw_os_error()
        ),
        StateError::Serialization(_) => "recorded JSON is invalid".into(),
        _ => error.to_string(),
    }
}

fn source_reason(error: &crate::github::project::ProjectError) -> String {
    use crate::github::project::ProjectError;
    match error {
        ProjectError::Read(error) | ProjectError::IssueRead(error) => format!(
            "source read failed: {:?} {:?} {} (HTTP {:?})",
            error.operation, error.category, error.code, error.status,
        ),
        ProjectError::MissingCursor | ProjectError::RepeatedCursor => error.to_string(),
        ProjectError::Inconsistent { .. } => {
            "Project item disagrees with direct issue identity".into()
        }
        ProjectError::Duplicate(_) => {
            "Project enumeration returned duplicate issue identity".into()
        }
        ProjectError::InvalidItem(_) => "Project item has an empty issue identity".into(),
    }
}
