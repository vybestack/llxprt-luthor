use super::{StateStore, attempts, continuation_hold, proofs};
use crate::{
    model::{ExitPrEvidence, LaunchPlan, PausePrStatus, StateError},
    supervisor::dispatch_abort::DispatchAbort,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbortRecoveryAudit {
    actor: String,
    reason: String,
    observed_at_unix_secs: u64,
    abort: DispatchAbort,
    pr: ExitPrEvidence,
}

pub(crate) fn commit(
    store: &mut StateStore,
    abort: DispatchAbort,
    actor: &str,
    reason: &str,
    pr: ExitPrEvidence,
) -> Result<(), StateError> {
    let task = &abort.plan.task_id;
    let attempt = &abort.plan.attempt_id;
    let tx = store
        .connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    continuation_hold::require_unamended_task(&tx, task)?;
    let valid: bool = tx.query_row(
        "SELECT COUNT(*)=1 FROM attempts a JOIN tasks t ON t.id=a.task_id
         JOIN reservations r ON r.attempt_id=a.id AND r.task_id=t.id
         WHERE a.task_id=?1 AND a.id=?2 AND a.lifecycle='launch_intended' AND a.outcome IS NULL
         AND t.state='held' AND r.status='reserved'
         AND a.id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1)
         AND NOT EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND kind='verified_open_pr')",
        params![task, attempt],
        |row| row.get(0),
    )?;
    let selection = attempts::attempt_selection(&tx, &abort.plan)?;
    if !valid
        || !crate::supervisor::abort_recovery::unreleased(&tx, task, attempt)?
        || actor.trim().is_empty()
        || reason.trim().is_empty()
        || pr.observed_at_unix_secs == 0
        || pr.status != PausePrStatus::Absent
        || pr.repository != selection.candidate.mapping.code_repository
        || proofs::unique_payload(&tx, true, task, Some(attempt), "dispatch_abort_recovered")?
            .is_some()
    {
        return Err(StateError::LaunchBlocked);
    }
    let audit = serde_json::to_string(&AbortRecoveryAudit {
        actor: actor.into(),
        reason: reason.into(),
        observed_at_unix_secs: pr.observed_at_unix_secs,
        abort: abort.clone(),
        pr,
    })?;
    tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'dispatch_abort_recovered',?3)", params![task, attempt, audit])?;
    tx.execute(
        "UPDATE attempts SET lifecycle='completed',outcome='dispatch_aborted' WHERE id=?1",
        [attempt],
    )?;
    tx.execute(
        "UPDATE reservations SET status='released' WHERE attempt_id=?1",
        [attempt],
    )?;
    tx.execute("UPDATE tasks SET state='attention' WHERE id=?1", [task])?;
    tx.commit()?;
    Ok(())
}

/// Explicit retries may use an audited abort, never an invented worker receipt.
/// Natural-exit automatic retry eligibility remains unchanged.
pub(crate) fn retry_plan(
    db: &Connection,
    task: &str,
    attempt: &str,
) -> Result<LaunchPlan, StateError> {
    continuation_hold::require_unamended_task(db, task)?;
    let payload =
        proofs::unique_payload(db, true, task, Some(attempt), "dispatch_abort_recovered")?
            .ok_or(StateError::LaunchBlocked)?;
    let audit: AbortRecoveryAudit = proofs::parse_saved(&payload)?;
    let valid: bool = db.query_row(
        "SELECT COUNT(*)=1 FROM attempts a JOIN tasks t ON t.id=a.task_id
         JOIN reservations r ON r.attempt_id=a.id AND r.task_id=t.id
         WHERE a.task_id=?1 AND a.id=?2 AND a.lifecycle='completed' AND a.outcome='dispatch_aborted'
         AND t.state='attention' AND r.status='released'
         AND a.id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1)
         AND NOT EXISTS(SELECT 1 FROM reservations WHERE task_id=?1 AND status='reserved')
         AND NOT EXISTS(SELECT 1 FROM attempts WHERE task_id=?1 AND (lifecycle!='completed' OR outcome IS NULL))",
        params![task, attempt], |row| row.get(0),
    )?;
    let saved = proofs::unique_payload(db, false, task, Some(attempt), "launch")?
        .ok_or(StateError::LaunchBlocked)?;
    let plan: LaunchPlan = proofs::parse_saved(&saved)?;
    let selection = attempts::attempt_selection(db, &plan)?;
    if !valid
        || !crate::supervisor::abort_recovery::unreleased(db, task, attempt)?
        || audit.abort.plan != plan
        || plan.task_id != task
        || plan.attempt_id != attempt
        || audit.actor.trim().is_empty()
        || audit.reason.trim().is_empty()
        || audit.observed_at_unix_secs == 0
        || audit.pr.observed_at_unix_secs != audit.observed_at_unix_secs
        || audit.pr.status != PausePrStatus::Absent
        || audit.pr.repository != selection.candidate.mapping.code_repository
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(plan)
}

pub(crate) fn recovered_abort(
    db: &Connection,
    task: &str,
    attempt: &str,
) -> Result<Option<DispatchAbort>, StateError> {
    proofs::unique_payload(db, true, task, Some(attempt), "dispatch_abort_recovered")?
        .map(|payload| proofs::parse_saved::<AbortRecoveryAudit>(&payload).map(|audit| audit.abort))
        .transpose()
}

pub(crate) fn retryable_attention_count(db: &Connection) -> Result<usize, StateError> {
    let candidates: Vec<(String, String)> = db.prepare(
        "SELECT t.id,a.id FROM tasks t JOIN attempts a ON a.task_id=t.id
         WHERE t.state='attention' AND a.id=(SELECT id FROM attempts WHERE task_id=t.id ORDER BY rowid DESC LIMIT 1)
         AND EXISTS(SELECT 1 FROM evidence WHERE task_id=t.id AND attempt_id=a.id AND kind='dispatch_abort_recovered')"
    )?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut count = 0;
    for (task, attempt) in candidates {
        match retry_plan(db, &task, &attempt) {
            Ok(_) => count += 1,
            Err(StateError::LaunchBlocked) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(count)
}
