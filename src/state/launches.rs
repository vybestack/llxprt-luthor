use super::{
    attempts::{attempt_selection, retry_context, retry_evidence_matches, same_task_config},
    continuation_hold,
    database::StateStore,
};
use crate::{
    model::{PausePrStatus, RetryAuthorization, SelectionEvidence, StateError},
    ownership::WorktreeOwnerProtocolInternal,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

fn read_resume_context(
    connection: &Connection,
    task_id: &str,
) -> Result<(String, String, String), StateError> {
    continuation_hold::require_unamended_task(connection, task_id)?;
    let latest: Option<(String, String, String)> = connection.query_row(
        "SELECT a.id, i.detail, e.payload FROM attempts a
         JOIN tasks t ON t.id=a.task_id
         JOIN reservations r ON r.attempt_id=a.id AND r.task_id=a.task_id
         JOIN intents i ON i.attempt_id=a.id AND i.task_id=a.task_id AND i.kind='launch'
         JOIN evidence e ON e.attempt_id=a.id AND e.task_id=a.task_id AND e.kind='attempt_exit'
         WHERE a.task_id=?1 AND t.state='paused' AND a.lifecycle='completed'
           AND a.outcome IS NOT NULL AND r.status='released'
           AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=a.id AND kind='attempt_exit')=1
           AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=a.id AND task_id=?1 AND kind='pause_pr_lookup' AND json_extract(payload,'$.status.status')='absent')=1
           AND (SELECT COUNT(*) FROM intents WHERE attempt_id=a.id AND kind='launch')=1
           AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=a.id AND kind='stop')=1
           AND (SELECT COUNT(*) FROM reservations WHERE task_id=?1 AND status='reserved')=0
           AND (SELECT COUNT(*) FROM attempts WHERE task_id=?1 AND (lifecycle!='completed' OR outcome IS NULL))=0
           AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified')=1
           AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='worktree_created')=1
         ORDER BY a.rowid DESC LIMIT 1",
        [task_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    ).optional()?;
    let (latest_id, latest_plan, exit) = latest.ok_or(StateError::LaunchBlocked)?;
    let actual_latest: Option<String> = connection
        .query_row(
            "SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    if actual_latest.as_deref() != Some(&latest_id) {
        return Err(StateError::LaunchBlocked);
    }
    let initial: Vec<String> = connection
        .prepare(
            "SELECT i.detail FROM attempts a JOIN intents i ON i.attempt_id=a.id
         WHERE a.task_id=?1 AND i.task_id=?1 AND i.kind='launch'
         ORDER BY a.rowid LIMIT 1",
        )?
        .query_map([task_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let first_plan = initial
        .into_iter()
        .next()
        .ok_or(StateError::LaunchBlocked)?;
    let receipt: crate::model::ExitReceipt = serde_json::from_str(&exit)?;
    let outcome: String = connection.query_row(
        "SELECT outcome FROM attempts WHERE id=?1",
        [&latest_id],
        |row| row.get(0),
    )?;
    if receipt.attempt_id != latest_id
        || receipt.stop_signals.is_empty()
        || outcome
            != format!(
                "exit_code={:?};signal={:?}",
                receipt.exit_code, receipt.signal
            )
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok((first_plan, latest_plan, exit))
}

/// Returns the first and latest launch plans and the verified latest exit only
/// when a paused task has no outstanding worker or reservation.
pub fn resume_context(
    store: &StateStore,
    task_id: &str,
) -> Result<(String, String, String), StateError> {
    read_resume_context(&store.connection, task_id)
}

/// Persists a plan without granting permission to start a process.
pub fn hold_launch_intent(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    detail: &str,
) -> Result<(), StateError> {
    hold_launch(store, task_id, attempt_id, detail, None)
}

pub fn launch_intent(store: &StateStore, attempt_id: &str) -> Result<Option<String>, StateError> {
    store
        .connection
        .query_row(
            "SELECT detail FROM intents WHERE attempt_id=?1 AND kind='launch'",
            [attempt_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(StateError::from)
}

/// A dispatch marker is written once, before spawning. An uncertain spawn
/// cannot be retried automatically, even if the supervisor never became ready.
pub fn begin_supervision(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    plan: &str,
    owner_protocol: &crate::ownership::WorktreeOwnerProtocol,
) -> Result<(), StateError> {
    let tx = store.connection.transaction()?;
    let persisted: Option<String> = tx
        .query_row(
            "SELECT i.detail FROM intents i JOIN attempts a ON a.id=i.attempt_id
             JOIN reservations r ON r.attempt_id=a.id
             JOIN tasks t ON t.id=a.task_id
             WHERE i.kind='launch' AND i.attempt_id=?1 AND i.task_id=?2
               AND r.task_id=?2 AND r.status='reserved' AND t.state='held'
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND kind='claim_verified')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND kind='worktree_created')=1",
            params![attempt_id, task_id],
            |row| row.get(0),
        )
        .optional()?;
    let amendments: i64 = tx.query_row(
        "SELECT (SELECT COUNT(*) FROM evidence WHERE (task_id=?1 OR attempt_id=?2) AND kind='initial_branch_removed')
             + (SELECT COUNT(*) FROM intents WHERE (task_id=?1 OR attempt_id=?2) AND kind='initial_branch_removal_seal')",
        params![task_id, attempt_id], |row| row.get(0),
    )?;
    if persisted.as_deref() != Some(plan)
        || amendments != 0
        || !owner_protocol.matches_attempt(task_id, attempt_id)
    {
        return Err(StateError::LaunchBlocked);
    }
    let previous: i64 = tx.query_row(
        "SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND kind='supervisor_dispatch'",
        [attempt_id],
        |row| row.get(0),
    )?;
    if previous != 0 {
        return Err(StateError::LaunchBlocked);
    }
    let owner_payload = serde_json::to_string(owner_protocol)?;
    let owner_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='worktree_owner_protocol'",
        params![task_id, attempt_id],
        |row| row.get(0),
    )?;
    if owner_count != 0 {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'supervisor_dispatch',?4)",
        params![format!("supervisor-{attempt_id}"), task_id, attempt_id, plan])?;
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'worktree_owner_protocol',?3)",
        params![task_id, attempt_id, owner_payload],
    )?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn initial_launch(store: &StateStore, task_id: &str) -> Result<String, StateError> {
    let first: String = store.connection.query_row(
        "SELECT i.detail FROM attempts a JOIN intents i ON i.attempt_id=a.id
         WHERE a.task_id=?1 AND i.task_id=?1 AND i.kind='launch' ORDER BY a.rowid LIMIT 1",
        [task_id],
        |row| row.get(0),
    )?;
    Ok(first)
}

fn hold_launch(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    detail: &str,
    retry: Option<&RetryAuthorization>,
) -> Result<(), StateError> {
    if attempt_id.is_empty()
        || attempt_id.len() > 128
        || !attempt_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(StateError::LaunchBlocked);
    }
    let tx = store.connection.transaction()?;
    let phase: Option<String> = tx
        .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
            row.get(0)
        })
        .optional()?;
    authorize_launch_phase(&tx, task_id, attempt_id, phase.as_deref(), retry)?;
    let prior_intents: i64 = tx.query_row(
        "SELECT COUNT(*) FROM intents WHERE attempt_id=?1",
        [attempt_id],
        |row| row.get(0),
    )?;
    if prior_intents != 0 {
        return Err(StateError::LaunchBlocked);
    }
    let capacity: usize = tx.query_row(
        "SELECT value FROM state_meta WHERE key='capacity'",
        [],
        |row| row.get(0),
    )?;
    let reserved: usize = tx.query_row(
        "SELECT COUNT(*) FROM reservations WHERE status='reserved'",
        [],
        |row| row.get(0),
    )?;
    if reserved >= capacity {
        return Err(StateError::Capacity { reserved, capacity });
    }
    tx.execute(
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES(?1,?2,'launch_intended')",
        params![attempt_id, task_id],
    )?;
    tx.execute(
        "INSERT INTO reservations(attempt_id,task_id,status) VALUES(?1,?2,'reserved')",
        params![attempt_id, task_id],
    )?;
    tx.execute(
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'launch',?4)",
        params![format!("launch-{attempt_id}"), task_id, attempt_id, detail],
    )?;
    if let Some(audit) = retry {
        tx.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'retry_authorized',?3)",
            params![task_id, attempt_id, serde_json::to_string(audit)?],
        )?;
        tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'retry_pr_lookup',?3)", params![task_id, attempt_id, serde_json::to_string(&audit.pr)?])?;
    }
    tx.execute("UPDATE tasks SET state='held' WHERE id=?1", [task_id])?;
    tx.commit()?;
    Ok(())
}

pub fn retry_context_for_task(
    store: &StateStore,
    task_id: &str,
    previous_attempt_id: &str,
) -> Result<(crate::model::LaunchPlan, crate::model::ExitReceipt), StateError> {
    retry_context(&store.connection, task_id, previous_attempt_id)
}

pub(crate) fn selection_for_attempt(
    store: &StateStore,
    plan: &crate::model::LaunchPlan,
) -> Result<SelectionEvidence, StateError> {
    attempt_selection(&store.connection, plan)
}

pub(crate) fn hold_retry_intent(
    store: &mut StateStore,
    audit: &RetryAuthorization,
) -> Result<(), StateError> {
    hold_launch(
        store,
        &audit.plan.task_id,
        &audit.plan.attempt_id,
        &serde_json::to_string(&audit.plan)?,
        Some(audit),
    )
}

fn authorize_launch_phase(
    tx: &Transaction<'_>,
    task_id: &str,
    attempt_id: &str,
    phase: Option<&str>,
    retry: Option<&RetryAuthorization>,
) -> Result<(), StateError> {
    let claims: i64 = tx.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified'",
        [task_id],
        |row| row.get(0),
    )?;
    let worktrees: i64 = tx.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='worktree_created'",
        [task_id],
        |row| row.get(0),
    )?;
    let attempts: i64 = tx.query_row(
        "SELECT COUNT(*) FROM attempts WHERE task_id=?1",
        [task_id],
        |row| row.get(0),
    )?;
    match phase {
        Some("claimed") if retry.is_none() && attempts == 0 && claims == 1 && worktrees == 1 => {}
        Some("paused") if retry.is_none() && claims == 1 && worktrees == 1 => {
            read_resume_context(tx, task_id)?;
        }
        Some("attention") if retry.is_some() && claims == 1 && worktrees == 1 => {
            authorize_retry(tx, retry.expect("retry checked above"), task_id, attempt_id)?;
        }
        _ => return Err(StateError::LaunchBlocked),
    }
    Ok(())
}

fn authorize_retry(
    tx: &Transaction<'_>,
    audit: &RetryAuthorization,
    task_id: &str,
    attempt_id: &str,
) -> Result<(), StateError> {
    let (prior, _) = retry_context(tx, task_id, &audit.previous_plan.attempt_id)?;
    let previous = attempt_selection(tx, &prior)?;
    if audit.previous_plan != prior
        || !retry_evidence_matches(tx, audit, &previous)?
        || audit.previous_config != previous.effective_config
        || !same_task_config(&previous.effective_config, &audit.config)
        || audit.actor != previous.candidate.mapping.allowed_pr_author
        || audit.reason.trim().is_empty()
        || audit.plan.attempt_id != attempt_id
        || audit.plan.task_id != task_id
        || audit.reservation != attempt_id
        || audit.pr.status != PausePrStatus::Absent
        || audit.pr.observed_at_unix_secs == 0
        || audit.pr.repository != previous.candidate.mapping.code_repository
        || audit.plan.config_revision.trim().is_empty()
        || audit.plan.config_revision == prior.config_revision
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
