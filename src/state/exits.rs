use super::proofs::validate_verified_open_pr;
use crate::{
    model::VerifiedOpenPr,
    model::{ExitPrEvidence, PausePrStatus, SelectionEvidence, StateError},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

fn refuse_amended_telemetry_loss(
    db: &Connection,
    task_id: &str,
    attempt_id: &str,
) -> Result<(), StateError> {
    // Amended observation has no typed telemetry-loss state contract.
    if super::amended_dispatch::has_amendment(db, task_id, attempt_id)? {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

fn validate_telemetry_lost_audit(
    audit: &str,
    lookup: &ExitPrEvidence,
    expected_status: PausePrStatus,
) -> Result<(), StateError> {
    let audit_value: serde_json::Value = serde_json::from_str(audit)?;
    if lookup.status != expected_status
        || lookup.observed_at_unix_secs == 0
        || ["actor", "reason"].iter().any(|key| {
            audit_value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
        })
        || audit_value
            .get("observed_at_unix_secs")
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|value| value == 0)
        || audit_value
            .get("os_ids")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| values.is_empty())
        || audit_value.get("exit_code").is_some()
        || audit_value.get("signal").is_some()
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

fn transition_telemetry_lost_attempt(
    tx: &Transaction<'_>,
    task_id: &str,
    attempt_id: &str,
    task_phase: &str,
) -> Result<(), StateError> {
    tx.execute(
        "UPDATE attempts SET lifecycle='telemetry_lost',outcome=NULL WHERE task_id=?1 AND id=?2 AND lifecycle='launch_intended' AND outcome IS NULL",
        params![task_id, attempt_id],
    )?;
    if tx.changes() != 1 {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "UPDATE reservations SET status='released' WHERE task_id=?1 AND attempt_id=?2 AND status='reserved'",
        params![task_id, attempt_id],
    )?;
    if tx.changes() != 1 {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "UPDATE tasks SET state=?2 WHERE id=?1 AND state='held'",
        params![task_id, task_phase],
    )?;
    if tx.changes() != 1 {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

fn matches_completed_exit(
    db: &Connection,
    task_id: &str,
    attempt_id: &str,
    evidence: &str,
    outcome: &str,
    persisted_outcome: Option<&str>,
    reservation: &str,
) -> Result<bool, StateError> {
    let exits: Vec<(String, String)> = db
        .prepare(
            "SELECT task_id,payload FROM evidence WHERE attempt_id=?1 AND kind='attempt_exit'",
        )?
        .query_map([attempt_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(persisted_outcome == Some(outcome)
        && reservation == "released"
        && exits == [(task_id.to_owned(), evidence.to_owned())])
}

pub(crate) fn commit_telemetry_lost_recovery(
    connection: &mut Connection,
    task_id: &str,
    attempt_id: &str,
    audit: &str,
    lookup: &ExitPrEvidence,
) -> Result<(), StateError> {
    validate_telemetry_lost_audit(audit, lookup, PausePrStatus::Absent)?;
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    refuse_amended_telemetry_loss(&tx, task_id, attempt_id)?;
    let latest: Option<String> = tx
        .query_row(
            "SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    let phase: Option<String> = tx
        .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
            row.get(0)
        })
        .optional()?;
    let reserved: i64 = tx.query_row(
            "SELECT COUNT(*) FROM reservations WHERE task_id=?1 AND attempt_id=?2 AND status='reserved'",
            params![task_id, attempt_id], |row| row.get(0),
        )?;
    let valid: i64 = tx.query_row(
            "SELECT COUNT(*) FROM attempts a WHERE a.task_id=?1 AND a.id=?2
             AND a.lifecycle='launch_intended' AND a.outcome IS NULL
             AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='launch')=1
             AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='supervisor_dispatch')=1
             AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_release')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='supervisor_ready')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_sent')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='child_registered')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind IN ('attempt_exit','log_failure','telemetry_lost','exit_pr_lookup'))=0
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='worktree_created')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='claim_verified')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='verified_open_pr')=0
             AND NOT EXISTS (SELECT 1 FROM evidence WHERE task_id=?1 AND kind IN ('pause_pr_lookup','exit_pr_lookup')
               AND json_extract(payload,'$.status.status') IN ('open','ambiguous'))",
            params![task_id, attempt_id], |row| row.get(0),
        )?;
    if latest.as_deref() != Some(attempt_id)
        || phase.as_deref() != Some("held")
        || reserved != 1
        || valid != 1
    {
        return Err(StateError::LaunchBlocked);
    }
    let selection: String = tx.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection'",
        [task_id],
        |row| row.get(0),
    )?;
    let selection: SelectionEvidence = serde_json::from_str(&selection)?;
    if lookup.repository != selection.candidate.mapping.code_repository {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'telemetry_lost',?3)",
        params![task_id, attempt_id, audit],
    )?;
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'exit_pr_lookup',?3)",
        params![task_id, attempt_id, serde_json::to_string(lookup)?],
    )?;
    transition_telemetry_lost_attempt(&tx, task_id, attempt_id, "held")?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn commit_telemetry_lost_pr_completion(
    connection: &mut Connection,
    task_id: &str,
    attempt_id: &str,
    audit: &str,
    lookup: &ExitPrEvidence,
    proof: &VerifiedOpenPr,
) -> Result<(), StateError> {
    validate_telemetry_lost_audit(audit, lookup, PausePrStatus::Open)?;
    if proof.id == 0 || proof.attempt_id != attempt_id || proof.observed_at == 0 {
        return Err(StateError::LaunchBlocked);
    }
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    refuse_amended_telemetry_loss(&tx, task_id, attempt_id)?;
    let latest: Option<String> = tx
        .query_row(
            "SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    let reserved: i64 = tx.query_row(
            "SELECT COUNT(*) FROM reservations WHERE task_id=?1 AND attempt_id=?2 AND status='reserved'",
            params![task_id, attempt_id], |row| row.get(0),
        )?;
    let valid: i64 = tx.query_row(
            "SELECT COUNT(*) FROM attempts a JOIN tasks t ON t.id=a.task_id
             WHERE a.task_id=?1 AND a.id=?2 AND a.lifecycle='launch_intended' AND a.outcome IS NULL
             AND t.state='held'
             AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='launch')=1
             AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='supervisor_dispatch')=1
             AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_release')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='supervisor_ready')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_sent')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='child_registered')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind IN ('attempt_exit','log_failure','telemetry_lost','exit_pr_lookup'))=0
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='worktree_created')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='claim_verified')=1
             AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='verified_open_pr')=0
             AND NOT EXISTS (SELECT 1 FROM evidence WHERE task_id=?1 AND kind IN ('pause_pr_lookup','exit_pr_lookup')
               AND json_extract(payload,'$.status.status') IN ('open','ambiguous'))",
            params![task_id, attempt_id], |row| row.get(0),
        )?;
    if latest.as_deref() != Some(attempt_id) || reserved != 1 || valid != 1 {
        return Err(StateError::LaunchBlocked);
    }
    let selection: String = tx.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection'",
        [task_id],
        |row| row.get(0),
    )?;
    let selection: SelectionEvidence = serde_json::from_str(&selection)?;
    if lookup.repository != selection.candidate.mapping.code_repository {
        return Err(StateError::LaunchBlocked);
    }
    validate_verified_open_pr(&tx, task_id, attempt_id, proof)?;
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'telemetry_lost',?3)",
        params![task_id, attempt_id, audit],
    )?;
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'exit_pr_lookup',?3)",
        params![task_id, attempt_id, serde_json::to_string(lookup)?],
    )?;
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'verified_open_pr',?3)",
        params![task_id, attempt_id, serde_json::to_string(proof)?],
    )?;
    transition_telemetry_lost_attempt(&tx, task_id, attempt_id, "pr_complete")?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn reconcile_verified_exit(
    connection: &mut Connection,
    task_id: &str,
    attempt_id: &str,
    evidence: &str,
    outcome: &str,
) -> Result<(), StateError> {
    let tx = connection.transaction()?;
    super::amended_dispatch::verify_committed_amendment(&tx, task_id, attempt_id)?;
    let current: Option<(String, Option<String>, String)> = tx
        .query_row(
            "SELECT a.lifecycle,a.outcome,r.status FROM attempts a
             JOIN reservations r ON r.attempt_id=a.id
             WHERE a.id=?1 AND a.task_id=?2 AND r.task_id=?2",
            params![attempt_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((ref lifecycle, ref persisted, ref reservation)) = current
        && lifecycle == "completed"
    {
        if matches_completed_exit(
            &tx,
            task_id,
            attempt_id,
            evidence,
            outcome,
            persisted.as_deref(),
            reservation,
        )? {
            tx.commit()?;
            return Ok(());
        }
        return Err(StateError::LaunchBlocked);
    }
    if !matches!(current, Some((ref lifecycle, None, ref reservation))
            if lifecycle == "launch_intended" && reservation == "reserved")
    {
        return Err(StateError::LaunchBlocked);
    }
    let valid: i64 = tx.query_row(
            "SELECT COUNT(*) FROM attempts a JOIN reservations r ON r.attempt_id=a.id
             JOIN tasks t ON t.id=a.task_id
             WHERE a.id=?1 AND a.task_id=?2 AND a.lifecycle='launch_intended'
               AND a.outcome IS NULL AND r.task_id=?2 AND r.status='reserved' AND t.state='held'
               AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND task_id=?2 AND kind='launch')=1
               AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND task_id=?2 AND kind='supervisor_dispatch')=1
               AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND task_id=?2 AND kind='gate_release')=1
               AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=?1 AND task_id=?2 AND kind='supervisor_ready')=1
               AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=?1 AND task_id=?2 AND kind='gate_sent')<=1
               AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND task_id=?2 AND kind='launch')+
                   (SELECT COUNT(*) FROM evidence WHERE attempt_id=?1 AND task_id=?2 AND kind='attempt_exit')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND kind='claim_verified')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND kind='worktree_created')=1
               AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=?1 AND kind='attempt_exit')=0
               AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=?1 AND kind='log_failure')=0",
            params![attempt_id, task_id], |row| row.get(0)
        )?;
    if valid != 1 {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'attempt_exit',?3)",
        params![task_id, attempt_id, evidence],
    )?;
    tx.execute("UPDATE attempts SET lifecycle='completed',outcome=?3 WHERE id=?1 AND task_id=?2 AND lifecycle='launch_intended' AND outcome IS NULL", params![attempt_id, task_id, outcome])?;
    if tx.changes() != 1 {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute("UPDATE reservations SET status='released' WHERE attempt_id=?1 AND task_id=?2 AND status='reserved'", params![attempt_id, task_id])?;
    if tx.changes() != 1 {
        return Err(StateError::LaunchBlocked);
    }
    tx.commit()?;
    Ok(())
}
