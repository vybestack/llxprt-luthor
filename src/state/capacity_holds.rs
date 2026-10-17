//! Read-only classification for resize arithmetic, never release authority.
use crate::model::StateError;
use rusqlite::{Connection, params};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PermanentHold {
    pub task_id: String,
    pub attempt_id: String,
    pub reason: String,
}

/// #27 deliberately provides no absence-only recovery for an initial attempt
/// without contemporaneous preparation. Count that historical reservation only
/// when it is held, unknown, and also lacks a complete dispatch inventory.
/// Complete inventories must go through reconciliation, not this exception.
/// Owners and all registered processes are checked separately, without exception.
pub(super) fn classify(
    db: &Connection,
    task: &str,
    attempt: &str,
) -> Result<Option<PermanentHold>, StateError> {
    let historical: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts a
         JOIN tasks t ON t.id=a.task_id
         JOIN reservations r ON r.task_id=a.task_id AND r.attempt_id=a.id
         WHERE a.id=?2 AND a.task_id=?1 AND t.state='held'
         AND a.lifecycle='launch_intended' AND a.outcome IS NULL AND r.status='reserved'
         AND (SELECT COUNT(*) FROM attempts WHERE task_id=?1)=1
         AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='launch')=1
         AND NOT EXISTS(SELECT 1 FROM launch_preparations WHERE task_id=?1 OR attempt_id=?2)
         AND NOT EXISTS(SELECT 1 FROM evidence WHERE (task_id=?1 OR attempt_id=?2)
             AND kind IN ('attempt_exit','telemetry_lost','dispatch_abort_recovered',
                 'retry_authorized','pr_repair_authorized','initial_branch_removed'))
         AND NOT (
             EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='supervisor_dispatch')
             AND EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_release')
             AND EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='worktree_owner_protocol')
             AND EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='supervisor_ready')
             AND EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='child_registered')
             AND EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_sent')))",
        params![task, attempt],
        |row| row.get(0),
    )?;
    Ok(historical.then(|| PermanentHold {
        task_id: task.into(),
        attempt_id: attempt.into(),
        reason: "historical initial attempt lacks preparation and complete dispatch proof; reservation remains held".into(),
    }))
}
