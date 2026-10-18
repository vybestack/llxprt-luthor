//! Read-only proof for increase arithmetic. Never reconciles or releases a slot.
use super::{SupervisorError, evidence, processes};
use crate::{
    model::StateError,
    ownership::{self, OwnershipError, WorktreeOwner},
    state::{StateStore, scheduling},
};
use serde::Serialize;
use std::fs;

#[derive(Debug, Serialize)]
pub struct RunningAttempt {
    pub task_id: String,
    pub attempt_id: String,
}

pub(crate) fn confirmed(store: &StateStore, task: &str, attempt: &str) -> Result<bool, StateError> {
    if !crate::cli_identifiers::valid_attempt(task)
        || !crate::cli_identifiers::valid_attempt(attempt)
        || !scheduling::active_attempt_reservation(store, task, attempt)?
    {
        return Ok(false);
    }
    let settled: bool = store.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_release')
         AND EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_sent')
         AND NOT EXISTS(SELECT 1 FROM intents WHERE attempt_id=?2 AND kind='stop')
         AND NOT EXISTS(SELECT 1 FROM evidence WHERE attempt_id=?2 AND kind IN
             ('attempt_exit','telemetry_lost','dispatch_abort_recovered','log_failure'))",
        rusqlite::params![task, attempt], |r| r.get(0),
    )?;
    if !settled {
        return Ok(false);
    }
    crate::state::dispatch_stages::verify(&store.connection, task, attempt)?;
    let Ok((attempts, _plan)) =
        evidence::reconciliation_plan_evidence(store, task, attempt).map_err(state_error)?
    else {
        return Ok(false);
    };
    for suffix in ["receipt.json", "supervisor-error.json"] {
        match fs::symlink_metadata(attempts.join(format!("{attempt}.{suffix}"))) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let Ok(processes) = evidence::reconciliation_process_evidence(store, task, attempt, &attempts)
        .map_err(state_error)?
    else {
        return Ok(false);
    };
    if processes.sent.is_none()
        || !processes::verified_live_process(&processes.child, &processes.supervisor)
    {
        return Ok(false);
    }
    ownership::observation::verify_protocol(&store.connection, store.root(), task, attempt)?;
    // A confirmed worker must still hold its existing lock. An acquirable
    // or unavailable lock is not authority to count it as running.
    match WorktreeOwner::acquire_existing(store.root(), task) {
        Err(OwnershipError::Busy) => Ok(true),
        Ok(_) | Err(OwnershipError::Unavailable) => Ok(false),
    }
}

fn state_error(error: SupervisorError) -> StateError {
    match error {
        SupervisorError::State(error) => error,
        SupervisorError::Sql(error) => StateError::Sql(error),
        SupervisorError::Io(error) => StateError::Io(error),
        SupervisorError::Json(error) => StateError::Serialization(error),
        _ => StateError::LaunchBlocked,
    }
}
