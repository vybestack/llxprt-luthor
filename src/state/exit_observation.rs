use super::database::StateStore;
use crate::model::{ExitPrEvidence, PausePrEvidence, PausePrStatus, SelectionEvidence, StateError};
use rusqlite::{OptionalExtension, params};

pub(crate) fn reconciled_exit(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    evidence: &str,
    outcome: &str,
) -> Result<bool, StateError> {
    let row: Option<(String, Option<String>, String)> = store
        .connection
        .query_row(
            "SELECT a.lifecycle,a.outcome,r.status FROM attempts a
             JOIN reservations r ON r.attempt_id=a.id
             WHERE a.id=?1 AND a.task_id=?2 AND r.task_id=?2",
            params![attempt_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((lifecycle, persisted_outcome, reservation)) = row else {
        return Err(StateError::LaunchBlocked);
    };
    let exits: Vec<(String, String)> = store
        .connection
        .prepare(
            "SELECT task_id,payload FROM evidence WHERE attempt_id=?1 AND kind='attempt_exit'",
        )?
        .query_map([attempt_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    if lifecycle == "launch_intended"
        && persisted_outcome.is_none()
        && reservation == "reserved"
        && exits.is_empty()
    {
        return Ok(false);
    }
    if lifecycle == "completed"
        && persisted_outcome.as_deref() == Some(outcome)
        && reservation == "released"
        && exits == [(task_id.to_owned(), evidence.to_owned())]
    {
        return Ok(true);
    }
    Err(StateError::LaunchBlocked)
}

/// Only a completed, independently reconciled stop can request PR proof.
pub fn stopped_exit_for_pause(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<bool, StateError> {
    let exit: Option<String> = store.connection.query_row(
        "SELECT e.payload FROM evidence e JOIN attempts a ON a.id=e.attempt_id AND a.task_id=e.task_id
             JOIN reservations r ON r.attempt_id=a.id AND r.task_id=a.task_id
             JOIN tasks t ON t.id=a.task_id
             WHERE e.task_id=?1 AND e.attempt_id=?2 AND e.kind='attempt_exit'
               AND t.state='held' AND a.lifecycle='completed' AND a.outcome IS NOT NULL AND r.status='released'
               AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='attempt_exit')=1
               AND (SELECT COUNT(*) FROM reservations WHERE task_id=?1 AND status='reserved')=0
               AND (SELECT COUNT(*) FROM attempts WHERE task_id=?1 AND (lifecycle!='completed' OR outcome IS NULL))=0
               AND a.id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1)",
        params![task_id, attempt_id], |row| row.get(0)
    ).optional()?;
    let Some(exit) = exit else {
        return Ok(false);
    };
    let receipt: crate::model::ExitReceipt = serde_json::from_str(&exit)?;
    Ok(receipt.attempt_id == attempt_id && !receipt.stop_signals.is_empty())
}

/// Only a terminal, released, naturally exited latest attempt may enter attention.
pub fn natural_exit_for_attention(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<bool, StateError> {
    let exit: Option<(String, String)> = store.connection.query_row(
        "SELECT e.payload,a.outcome FROM evidence e
             JOIN attempts a ON a.id=e.attempt_id AND a.task_id=e.task_id
             JOIN reservations r ON r.attempt_id=a.id AND r.task_id=a.task_id
             JOIN tasks t ON t.id=a.task_id
             WHERE e.task_id=?1 AND e.attempt_id=?2 AND e.kind='attempt_exit'
               AND t.state='held' AND a.lifecycle='completed' AND a.outcome IS NOT NULL AND r.status='released'
               AND a.id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1)
               AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=?2 AND kind='attempt_exit')=1
               AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='launch')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='worktree_created')=1
               AND (SELECT COUNT(*) FROM reservations WHERE task_id=?1 AND status='reserved')=0
               AND (SELECT COUNT(*) FROM attempts WHERE task_id=?1 AND (lifecycle!='completed' OR outcome IS NULL))=0",
        params![task_id, attempt_id], |row| Ok((row.get(0)?, row.get(1)?))
    ).optional()?;
    let Some((exit, outcome)) = exit else {
        return Ok(false);
    };
    let receipt: crate::model::ExitReceipt = serde_json::from_str(&exit)?;
    Ok(receipt.attempt_id == attempt_id
        && receipt.stop_signals.is_empty()
        && outcome
            == format!(
                "exit_code={:?};signal={:?}",
                receipt.exit_code, receipt.signal
            ))
}

/// An absent PR observation and the attention transition commit together.
pub fn record_exit_pr_lookup(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    proof: &ExitPrEvidence,
) -> Result<(), StateError> {
    if !natural_exit_for_attention(store, task_id, attempt_id)? || proof.observed_at_unix_secs == 0
    {
        return Err(StateError::LaunchBlocked);
    }
    let tx = store.connection.transaction()?;
    let prior: Vec<String> = tx
        .prepare(
            "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='exit_pr_lookup' ORDER BY sequence",
        )?
        .query_map(params![task_id, attempt_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if prior.iter().any(|payload| {
        !matches!(
            serde_json::from_str::<ExitPrEvidence>(payload).map(|evidence| evidence.status),
            Ok(PausePrStatus::Error { .. } | PausePrStatus::Ambiguous | PausePrStatus::Open)
        )
    }) {
        return Err(StateError::LaunchBlocked);
    }
    let selection: String = tx.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection'",
        [task_id],
        |row| row.get(0),
    )?;
    let selection: SelectionEvidence = serde_json::from_str(&selection)?;
    if selection.candidate.mapping.code_repository != proof.repository {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'exit_pr_lookup',?3)",
        params![task_id, attempt_id, serde_json::to_string(proof)?],
    )?;
    match &proof.status {
        PausePrStatus::Absent => {
            tx.execute(
                "UPDATE tasks SET state='attention' WHERE id=?1 AND state='held'",
                [task_id],
            )?;
            if tx.changes() != 1 {
                return Err(StateError::LaunchBlocked);
            }
            tx.execute(
                "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'attention_reason','natural exit without open PR')",
                params![task_id, attempt_id]
            )?;
        }
        status => {
            let reason = match status {
                PausePrStatus::Open => "exit PR present",
                PausePrStatus::Ambiguous => "exit PR ambiguous",
                PausePrStatus::Error { .. } => "exit PR read failed",
                PausePrStatus::Absent => unreachable!(),
            };
            tx.execute(
                "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'held_reason',?3)",
                params![task_id, attempt_id, reason]
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// The fresh lookup and phase change are one transaction. A crash between
/// lookup and commit leaves held work requiring another read.
pub fn record_pause_pr_lookup(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    proof: &PausePrEvidence,
) -> Result<(), StateError> {
    if !stopped_exit_for_pause(store, task_id, attempt_id)? || proof.observed_at_unix_secs == 0 {
        return Err(StateError::LaunchBlocked);
    }
    let tx = store.connection.transaction()?;
    let selection: String = tx.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection'",
        [task_id],
        |row| row.get(0),
    )?;
    let selection: SelectionEvidence = serde_json::from_str(&selection)?;
    if selection.candidate.mapping.code_repository != proof.repository {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'pause_pr_lookup',?3)",
        params![task_id, attempt_id, serde_json::to_string(proof)?],
    )?;
    match &proof.status {
        PausePrStatus::Absent => {
            tx.execute(
                "UPDATE tasks SET state='paused' WHERE id=?1 AND state='held'",
                [task_id],
            )?;
            if tx.changes() != 1 {
                return Err(StateError::LaunchBlocked);
            }
        }
        status => {
            let reason = match status {
                PausePrStatus::Open => "pause PR present",
                PausePrStatus::Ambiguous => "pause PR ambiguous",
                PausePrStatus::Error { .. } => "pause PR read failed",
                PausePrStatus::Absent => unreachable!(),
            };
            tx.execute(
                "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'held_reason',?3)",
                params![task_id, attempt_id, reason]
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}
