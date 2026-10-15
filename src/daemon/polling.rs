//! Retry only typed transient failures and explicitly signalled reconciliation
//! barriers. Invalid config, malformed upstream data and storage bugs fail fast.
pub(crate) type Error = Box<dyn std::error::Error>;
const POLL_INTERVAL: Duration = Duration::from_secs(30);
use crate::{
    github::{
        project::{ProjectError, ReadCategory},
        pull_request::{ErrorCategory, LookupError},
    },
    state::StateError,
};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ReconciliationBlocked(pub &'static str);

fn transient(error: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(crate::eligibility::EligibilityError::Project(error)) =
        error.downcast_ref::<crate::eligibility::EligibilityError>()
    {
        return transient(error);
    }
    if matches!(
        error.downcast_ref::<crate::github::identity::IdentityError>(),
        Some(crate::github::identity::IdentityError::Transient)
    ) {
        return true;
    }
    if let Some(error) = error.downcast_ref::<StateError>() {
        return match error {
            StateError::CoordinatorBusy => true,
            StateError::Io(error) => transient(error),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return matches!(
            error.kind(),
            std::io::ErrorKind::Interrupted
                | std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
        );
    }
    if let Some(error) = error.downcast_ref::<LookupError>() {
        return matches!(
            error.category,
            ErrorCategory::Transport | ErrorCategory::RateLimit
        );
    }
    if let Some(ProjectError::Read(error) | ProjectError::IssueRead(error)) =
        error.downcast_ref::<ProjectError>()
    {
        return matches!(
            error.category,
            ReadCategory::Transport | ReadCategory::RateLimit
        );
    }
    error.is::<ReconciliationBlocked>()
}

fn retry_reason(error: &(dyn std::error::Error + 'static)) -> &'static str {
    if let Some(ReconciliationBlocked(reason)) = error.downcast_ref::<ReconciliationBlocked>() {
        return reason;
    }
    if matches!(
        error.downcast_ref::<StateError>(),
        Some(StateError::CoordinatorBusy)
    ) {
        return "coordinator busy";
    }
    "transient upstream or operating-system read failure"
}

/// `sleep` and `cycle` are injected so backoff and recovery are deterministic in
/// tests, without changing production polling or introducing a test-only flag.
pub(crate) fn run(
    once: bool,
    mut cycle: impl FnMut() -> Result<(), Error>,
    mut sleep: impl FnMut(Duration),
) -> Result<(), Error> {
    let mut delay = POLL_INTERVAL;
    loop {
        let result = cycle();
        if once {
            return result;
        }
        match result {
            Ok(()) => delay = POLL_INTERVAL,
            Err(error) if transient(error.as_ref()) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"mode":"daemon","status":"cycle_error",
                    "reason":retry_reason(error.as_ref()),
                    "retry_in_seconds":delay.as_secs()})
                );
                sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(300));
                continue;
            }
            Err(error) => return Err(error),
        }
        sleep(delay);
    }
}

#[cfg(test)]
mod tests;
