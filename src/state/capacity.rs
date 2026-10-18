//! Explicit queue-level capacity authorization; never rewrites task provenance.
use super::{capacity_holds, database, proofs};
pub use crate::supervisor::capacity_observation::RunningAttempt;
use crate::{
    model::{ExitReceipt, StateError},
    ownership::WorktreeOwner,
};
pub use capacity_holds::PermanentHold;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;
use std::path::Path;

pub struct CapacityChangeRequest<'a> {
    pub expected_capacity: usize,
    pub expected_reservations: usize,
    pub new_capacity: usize,
    pub actor: &'a str,
    pub reason: &'a str,
}

#[derive(Debug, Serialize)]
pub struct CapacityChange {
    pub sequence: i64,
    pub actor: String,
    pub reason: String,
    pub old_capacity: usize,
    pub new_capacity: usize,
    pub reservations: usize,
    pub permanent_holds: Vec<PermanentHold>,
    pub running_attempts: Vec<RunningAttempt>,
    pub created_at: String,
}

fn refused(reason: &'static str) -> StateError {
    StateError::CapacityChangeRefused(reason)
}

/// Acquire the coordinator lock using the expected *old* limit, not a bypass
/// of the normal mismatch guard. An existing versioned database is mandatory.
/// Hold quiescent task owners through commit; confirmed live attempts retain
/// their independently verified existing ownership throughout the operation.
pub fn change(
    root: &Path,
    request: CapacityChangeRequest<'_>,
) -> Result<CapacityChange, StateError> {
    validate(&request)?;
    let mut store = database::open_existing(root, request.expected_capacity)?;
    // The coordinator stays held across read-only OS/dispatch observation and
    // the immediate transaction. No reservation/launch writer can intervene.
    let running_attempts =
        observe_running(&store, request.new_capacity > request.expected_capacity)?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let reserved: usize = tx.query_row(
        "SELECT COUNT(*) FROM reservations WHERE status='reserved'",
        [],
        |row| row.get(0),
    )?;
    if reserved != request.expected_reservations {
        return Err(refused("reservation count does not match expectation"));
    }
    if reserved > request.expected_capacity {
        return Err(refused(
            "reservations exceed the persisted limit; reconcile first",
        ));
    }
    if request.new_capacity < reserved {
        return Err(StateError::Capacity {
            reserved,
            capacity: request.new_capacity,
        });
    }
    let permanent_holds = require_accounted_launches(
        &tx,
        request.new_capacity > request.expected_capacity,
        &running_attempts,
    )?;
    let _owners = lock_owners(&tx, root, &running_attempts)?;
    tx.execute(
        "INSERT INTO capacity_changes(actor,reason,old_capacity,new_capacity,reservations,permanent_holds,running_attempts) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![request.actor, request.reason, request.expected_capacity as i64, request.new_capacity as i64, reserved as i64, serde_json::to_string(&permanent_holds)?, serde_json::to_string(&running_attempts)?],
    )?;
    let sequence = tx.last_insert_rowid();
    let created_at = tx.query_row(
        "SELECT created_at FROM capacity_changes WHERE sequence=?1",
        [sequence],
        |row| row.get(0),
    )?;
    let updated = tx.execute(
        "UPDATE state_meta SET value=?1 WHERE key='capacity' AND value=?2",
        params![
            request.new_capacity as i64,
            request.expected_capacity as i64
        ],
    )?;
    if updated != 1 {
        return Err(refused("persisted capacity changed during authorization"));
    }
    tx.commit()?;
    Ok(CapacityChange {
        sequence,
        actor: request.actor.into(),
        reason: request.reason.into(),
        old_capacity: request.expected_capacity,
        new_capacity: request.new_capacity,
        reservations: reserved,
        permanent_holds,
        running_attempts,
        created_at,
    })
}

fn validate(request: &CapacityChangeRequest<'_>) -> Result<(), StateError> {
    if request.new_capacity == 0
        || request.expected_capacity == 0
        || i64::try_from(request.new_capacity).is_err()
        || i64::try_from(request.expected_capacity).is_err()
    {
        return Err(StateError::InvalidCapacity);
    }
    if request.new_capacity == request.expected_capacity {
        return Err(refused("new capacity must differ from previous capacity"));
    }
    if request.actor.trim().is_empty() || request.reason.trim().is_empty() {
        return Err(refused("actor and reason are required"));
    }
    Ok(())
}

fn require_accounted_launches(
    db: &Connection,
    increasing: bool,
    running: &[RunningAttempt],
) -> Result<Vec<PermanentHold>, StateError> {
    let invalid_reservation: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM reservations r WHERE NOT EXISTS
         (SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=r.attempt_id AND a.task_id=r.task_id))",
        [], |row| row.get(0),
    )?;
    if invalid_reservation {
        return Err(refused("reservation identity is inconsistent"));
    }
    let launches: Vec<(String, Option<String>)> = db.prepare(
        "SELECT DISTINCT task_id,attempt_id FROM intents WHERE kind IN ('launch','supervisor_dispatch','gate_release') ORDER BY task_id,attempt_id",
    )?.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut holds = Vec::new();
    for (task, attempt) in launches {
        let attempt = attempt.as_deref().ok_or(refused("launch has no attempt"))?;
        if running
            .iter()
            .any(|r| r.task_id == task && r.attempt_id == attempt)
        {
            continue;
        }
        if increasing && let Some(hold) = capacity_holds::classify(db, &task, attempt)? {
            holds.push(hold);
        } else {
            require_accounted(db, &task, attempt)?;
        }
    }
    Ok(holds)
}

fn require_accounted(db: &Connection, task: &str, attempt: &str) -> Result<(), StateError> {
    use rusqlite::OptionalExtension;
    let row: Option<(String, Option<String>, String)> = db.query_row(
        "SELECT a.lifecycle,a.outcome,r.status FROM attempts a JOIN reservations r ON r.attempt_id=a.id AND r.task_id=a.task_id WHERE a.id=?1 AND a.task_id=?2",
        params![attempt, task], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    let Some((lifecycle, outcome, reservation)) = row else {
        return Err(refused("launch identity is inconsistent"));
    };
    if reservation != "released" {
        return Err(refused("active or unreconciled launch; reconcile first"));
    }
    let exit = proofs::unique_payload(db, true, task, Some(attempt), "attempt_exit")?;
    if lifecycle == "completed" {
        if let Some(exit) = exit.as_deref() {
            let receipt: ExitReceipt = serde_json::from_str(exit)?;
            if receipt.attempt_id == attempt
                && outcome
                    == Some(format!(
                        "exit_code={:?};signal={:?}",
                        receipt.exit_code, receipt.signal
                    ))
            {
                return Ok(());
            }
        } else if outcome.as_deref() == Some("dispatch_aborted")
            && proofs::unique_payload(db, true, task, Some(attempt), "dispatch_abort_recovered")?
                .is_some()
        {
            return Ok(());
        }
    }
    if lifecycle == "telemetry_lost"
        && outcome.is_none()
        && exit.is_none()
        && proofs::unique_payload(db, true, task, Some(attempt), "telemetry_lost")?.is_some()
    {
        return Ok(());
    }
    if lifecycle == "launch_intended"
        && outcome.is_none()
        && exit.is_none()
        && abandoned_before_dispatch(db, task, attempt)?
    {
        return Ok(());
    }
    Err(refused("active or unreconciled launch; reconcile first"))
}

fn abandoned_before_dispatch(
    db: &Connection,
    task: &str,
    attempt: &str,
) -> Result<bool, StateError> {
    // Retirement already verified contemporaneous preparation/ownership proof.
    // Do not manufacture it from a currently unlocked pathname.
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks t JOIN evidence e ON e.task_id=t.id
         WHERE t.id=?1 AND t.state='abandoned' AND e.kind='task_retired'
         AND json_extract(e.payload,'$.never_dispatched_attempt')=?2)
         AND NOT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind IN ('supervisor_dispatch','gate_release'))
         AND NOT EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind IN ('supervisor_ready','child_registered','gate_sent','tracked_descendant'))",
        params![task, attempt], |row| row.get(0),
    )?)
}

fn lock_owners(
    db: &Connection,
    root: &Path,
    running: &[RunningAttempt],
) -> Result<Vec<WorktreeOwner>, StateError> {
    let tasks: Vec<(String, bool)> = db.prepare(
        "SELECT t.id, EXISTS(SELECT 1 FROM evidence e WHERE e.task_id=t.id AND e.kind IN ('worktree_created','worktree_owner_protocol'))
         OR EXISTS(SELECT 1 FROM intents i WHERE i.task_id=t.id AND i.kind IN ('launch','worktree_create')) FROM tasks t ORDER BY t.id",
    )?.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut owners = Vec::new();
    for (task, required) in tasks {
        if let Some(active) = running.iter().find(|r| r.task_id == task) {
            super::process_quiescence::require_absent_except(db, &task, Some(&active.attempt_id))?;
            continue;
        }
        if !crate::cli_identifiers::valid_attempt(&task) {
            return Err(refused("invalid task owner identity"));
        }
        super::process_quiescence::require_absent_processes(db, &task).map_err(
            |error| match error {
                StateError::LaunchBlocked => {
                    refused("registered process may be active; reconcile first")
                }
                error => error,
            },
        )?;
        let exists = match std::fs::symlink_metadata(root.join(format!("worktree-{task}.lock"))) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if required || exists {
            owners.push(
                WorktreeOwner::acquire_existing(root, &task).map_err(|_| {
                    refused("worktree owner is live or unavailable; reconcile first")
                })?,
            );
        }
    }
    Ok(owners)
}

fn observe_running(
    store: &database::StateStore,
    increasing: bool,
) -> Result<Vec<RunningAttempt>, StateError> {
    let mut running = Vec::new();
    if !increasing {
        return Ok(running);
    }
    let candidates: Vec<(String, String)> = store.connection.prepare(
        "SELECT task_id,attempt_id FROM reservations WHERE status='reserved' ORDER BY task_id,attempt_id",
    )?.query_map([], |r| Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
    for (task, attempt) in candidates {
        if crate::supervisor::capacity_observation::confirmed(store, &task, &attempt)? {
            running.push(RunningAttempt {
                task_id: task,
                attempt_id: attempt,
            });
        }
    }
    Ok(running)
}
