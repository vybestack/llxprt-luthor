//! Repinning never substitutes for exit reconciliation or process ownership.
use super::pin_adoption;
use crate::model::StateError;
use rusqlite::Connection;

pub(crate) fn require_idle(db: &Connection, task: &str) -> Result<String, StateError> {
    let phase = pin_adoption::require_quiescent(db, task).map_err(|error| match error {
        StateError::PinAdoptionRefused(reason) => StateError::PinReplacementRefused(reason),
        error => error,
    })?;
    if !matches!(
        phase.as_str(),
        "attention" | "paused" | "held" | "pr_complete"
    ) {
        return Err(StateError::PinReplacementRefused("task is not continuable"));
    }
    super::process_quiescence::require_absent_processes(db, task).map_err(|error| match error {
        StateError::LaunchBlocked => StateError::PinReplacementRefused(
            "registered process or process group is present or its absence is unproven",
        ),
        error => error,
    })?;
    Ok(phase)
}
