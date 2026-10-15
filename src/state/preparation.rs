//! Contemporaneous preparation proof for new, not-yet-dispatched initial attempts.
//!
//! Migration deliberately leaves this table empty. Only creation of a new
//! attempt/reservation/launch can insert a row; there is no adoption API.
use crate::{
    model::StateError,
    ownership::{WorktreeOwner, WorktreeOwnerProtocol, WorktreeOwnerProtocolInternal},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;

pub(crate) fn record(
    tx: &Transaction<'_>,
    task: &str,
    attempt: &str,
    owner: &WorktreeOwnerProtocol,
) -> Result<(), StateError> {
    if !owner.matches_attempt(task, attempt) {
        return Err(StateError::LaunchBlocked);
    }
    let inserted = tx.execute(
        "INSERT INTO launch_preparations(attempt_id,task_id,launch_sequence,owner_payload)
         SELECT ?2,?1,sequence,?3 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='launch'",
        params![task, attempt, serde_json::to_string(owner)?],
    )?;
    if inserted != 1 {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

fn saved(db: &Connection, task: &str, attempt: &str) -> Result<WorktreeOwnerProtocol, StateError> {
    let payload: Option<String> = db.query_row(
        "SELECT p.owner_payload FROM launch_preparations p JOIN intents i ON i.sequence=p.launch_sequence
         WHERE p.task_id=?1 AND p.attempt_id=?2 AND i.task_id=?1 AND i.attempt_id=?2 AND i.kind='launch'",
        params![task, attempt], |row| row.get(0),
    ).optional()?;
    let payload = payload.ok_or(StateError::LaunchBlocked)?;
    let proof: WorktreeOwnerProtocol = serde_json::from_str(&payload)?;
    if !proof.matches_attempt(task, attempt)
        || serde_json::to_value(&proof)? != serde_json::from_str::<serde_json::Value>(&payload)?
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(proof)
}

/// Database eligibility only; the caller must also hold and verify the owner.
pub(crate) fn verify(db: &Connection, task: &str, attempt: &str) -> Result<(), StateError> {
    saved(db, task, attempt).map(|_| ())
}

pub(crate) fn verify_protocol(
    db: &Connection,
    task: &str,
    attempt: &str,
    current: &WorktreeOwnerProtocol,
) -> Result<(), StateError> {
    if saved(db, task, attempt)? != *current {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

pub(crate) fn verify_owner(
    db: &Connection,
    root: &Path,
    task: &str,
    attempt: &str,
    owner: &WorktreeOwner,
) -> Result<(), StateError> {
    let current = owner
        .protocol_evidence(root, task, attempt)
        .map_err(|_| StateError::LaunchBlocked)?;
    verify_protocol(db, task, attempt, &current)
}
