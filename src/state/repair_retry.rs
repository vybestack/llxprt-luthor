//! Narrow restoration of accounted repair failures; semantic/uncertain holds
//! never qualify. The scheduled audit does not assert a successful repair.
use super::{StateStore, exit_observation, journal, pr_repair, proofs, task_records};
use crate::{
    model::{ExitReceipt, LaunchPlan, StateError, VerifiedOpenPr},
    pr_repair::{RESULT_UNAVAILABLE, RepairRetry},
};
use rusqlite::params;

pub(crate) fn candidates(store: &StateStore) -> Result<Vec<String>, StateError> {
    Ok(store.connection.prepare(
        "SELECT t.id FROM tasks t WHERE t.state='held'
         AND (SELECT payload FROM evidence WHERE task_id=t.id AND kind='held_reason' ORDER BY sequence DESC LIMIT 1)=?1
         ORDER BY t.rowid",
    )?.query_map([RESULT_UNAVAILABLE], |row| row.get(0))?.collect::<Result<_, _>>()?)
}

pub(crate) fn failure_context(
    store: &StateStore,
    task: &str,
    attempt: &str,
) -> Result<Option<(LaunchPlan, ExitReceipt)>, StateError> {
    let Some(audit) = pr_repair::authorization(&store.connection, task, attempt)? else {
        return Ok(None);
    };
    super::continuation_hold::require_unamended_task(&store.connection, task)?;
    if !exit_observation::natural_exit_for_attention(store, task, attempt)?
        || journal::stop_intent(store, task, attempt)?.is_some()
        || journal::evidence_payload(store, task, Some(attempt), "pr_repair_result")?.is_some()
    {
        return Ok(None);
    }
    let forbidden: bool = store.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND kind='cleanup')
         OR EXISTS(SELECT 1 FROM evidence e WHERE task_id=?1 AND kind='pr_repair_blocked' AND (payload!=?3 OR (attempt_id IS NOT ?2 AND NOT EXISTS(SELECT 1 FROM evidence s WHERE s.task_id=e.task_id AND s.attempt_id=e.attempt_id AND s.kind='pr_repair_retry_scheduled'))))",
        params![task, attempt, RESULT_UNAVAILABLE], |row| row.get(0),
    )?;
    if forbidden {
        return Ok(None);
    }
    let receipt = receipt(store, task, attempt)?;
    if receipt.signal.is_some() || receipt.exit_code.is_none() {
        return Ok(None);
    }
    Ok(Some((audit.plan, receipt)))
}

/// Re-read the same authorization and receipt when observing a pending retry.
/// Physical receipt/ownership/lineage proofs are separately rechecked at launch.
pub(crate) fn scheduled_context(
    store: &StateStore,
    task: &str,
    attempt: &str,
) -> Result<(LaunchPlan, ExitReceipt), StateError> {
    let audit = pr_repair::authorization(&store.connection, task, attempt)?
        .ok_or(StateError::LaunchBlocked)?;
    Ok((audit.plan, receipt(store, task, attempt)?))
}

fn receipt(store: &StateStore, task: &str, attempt: &str) -> Result<ExitReceipt, StateError> {
    let payload = journal::evidence_payload(store, task, Some(attempt), "attempt_exit")?
        .ok_or(StateError::LaunchBlocked)?;
    let receipt: ExitReceipt = proofs::parse_saved(&payload)?;
    if receipt.attempt_id != attempt {
        return Err(StateError::LaunchBlocked);
    }
    Ok(receipt)
}

pub(crate) fn scheduled(
    store: &StateStore,
    task: &str,
    attempt: &str,
) -> Result<Option<RepairRetry>, StateError> {
    journal::evidence_payload(store, task, Some(attempt), "pr_repair_retry_scheduled")?
        .map(|payload| {
            let retry: RepairRetry = proofs::parse_saved(&payload)?;
            if retry.task_id != task
                || retry.attempt_id != attempt
                || retry.backoff_until_unix_secs == 0
            {
                return Err(StateError::LaunchBlocked);
            }
            Ok(retry)
        })
        .transpose()
}

pub(crate) fn blocked(store: &StateStore, task: &str, attempt: &str) -> Result<bool, StateError> {
    let reason = journal::evidence_payload(store, task, Some(attempt), "pr_repair_blocked")?;
    Ok(match reason {
        None => false,
        Some(reason) => reason != RESULT_UNAVAILABLE || scheduled(store, task, attempt)?.is_none(),
    })
}

pub(crate) fn attempt_count(store: &StateStore, task: &str) -> Result<u32, StateError> {
    Ok(store.connection.query_row(
        "SELECT COUNT(*) FROM attempts WHERE task_id=?1",
        [task],
        |row| row.get(0),
    )?)
}

/// Bind a fresh exact-head PR observation to the failed attempt and schedule
/// atomically. There is deliberately no pr_repair_result or completion lookup.
pub(crate) fn schedule(
    store: &mut StateStore,
    retry: &RepairRetry,
    observed: &VerifiedOpenPr,
) -> Result<(), StateError> {
    let task = &retry.task_id;
    let attempt = &retry.attempt_id;
    if task_records::held_reason(store, task)?.as_deref() != Some(RESULT_UNAVAILABLE)
        || failure_context(store, task, attempt)?.is_none()
        || scheduled(store, task, attempt)?.is_some()
        || observed.attempt_id != *attempt
    {
        return Err(StateError::LaunchBlocked);
    }
    let tx = store.connection.transaction()?;
    proofs::validate_verified_open_pr(&tx, task, attempt, observed)?;
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'pr_head_verified',?3)",
        params![task, attempt, serde_json::to_string(observed)?],
    )?;
    tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'pr_repair_retry_scheduled',?3)", params![task, attempt, serde_json::to_string(retry)?])?;
    tx.execute(
        "UPDATE tasks SET state='pr_complete' WHERE id=?1 AND state='held'",
        [task],
    )?;
    if tx.changes() != 1 {
        return Err(StateError::LaunchBlocked);
    }
    tx.commit()?;
    Ok(())
}

/// A scheduled repair has the same released capacity as a completed PR, but
/// has no fabricated completion result. Count only fully accounted exact proofs.
pub(crate) fn released_count(db: &rusqlite::Connection) -> Result<usize, StateError> {
    Ok(db.query_row(
        "SELECT COUNT(*) FROM tasks t JOIN attempts a ON a.task_id=t.id
         JOIN reservations r ON r.task_id=t.id AND r.attempt_id=a.id
         WHERE t.state='pr_complete' AND a.id=(SELECT id FROM attempts WHERE task_id=t.id ORDER BY rowid DESC LIMIT 1)
           AND a.lifecycle='completed' AND a.outcome IS NOT NULL AND r.status='released'
           AND NOT EXISTS(SELECT 1 FROM reservations WHERE task_id=t.id AND status='reserved')
           AND NOT EXISTS(SELECT 1 FROM attempts WHERE task_id=t.id AND (lifecycle!='completed' OR outcome IS NULL))
           AND NOT EXISTS(SELECT 1 FROM intents WHERE task_id=t.id AND (kind='cleanup' OR (attempt_id=a.id AND kind='stop')))
           AND (SELECT COUNT(*) FROM evidence WHERE task_id=t.id AND kind='claim_verified')=1
           AND (SELECT COUNT(*) FROM evidence WHERE task_id=t.id AND kind='worktree_created')=1
           AND (SELECT COUNT(*) FROM evidence WHERE task_id=t.id AND attempt_id=a.id AND kind='pr_repair_authorized')=1
           AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=a.id AND kind='attempt_exit')=1
           AND EXISTS(SELECT 1 FROM evidence WHERE task_id=t.id AND attempt_id=a.id AND kind='attempt_exit'
             AND CASE WHEN json_valid(payload) THEN json_extract(payload,'$.attempt_id')=a.id
               AND json_extract(payload,'$.exit_code') IS NOT NULL AND json_extract(payload,'$.signal') IS NULL
               AND json_array_length(payload,'$.stop_signals')=0 ELSE 0 END)
           AND (SELECT COUNT(*) FROM evidence WHERE task_id=t.id AND attempt_id=a.id AND kind='pr_repair_retry_scheduled')=1
           AND (SELECT COUNT(*) FROM evidence WHERE task_id=t.id AND attempt_id=a.id AND kind='verified_open_pr')=0
           AND (SELECT COUNT(*) FROM evidence WHERE task_id=t.id AND attempt_id=a.id AND kind='pr_repair_retry_scheduled'
             AND CASE WHEN json_valid(payload) THEN json_extract(payload,'$.task_id')=t.id
               AND json_extract(payload,'$.attempt_id')=a.id AND json_extract(payload,'$.backoff_until_unix_secs')>0 ELSE 0 END)=1
           AND EXISTS(SELECT 1 FROM evidence WHERE task_id=t.id AND attempt_id=a.id AND kind='pr_head_verified'
             AND CASE WHEN json_valid(payload) THEN json_extract(payload,'$.attempt_id')=a.id AND json_extract(payload,'$.id')>0 ELSE 0 END)",
        [], |row| row.get(0),
    )?)
}
