use super::{
    amended_completion, amended_dispatch,
    database::StateStore,
    exit_observation::{natural_exit_for_attention, stopped_exit_for_pause},
    proofs::validate_verified_open_pr,
};
use crate::model::{StateError, VerifiedOpenPr};
use rusqlite::params;

pub fn record_verified_open_pr(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    proof: &VerifiedOpenPr,
) -> Result<(), StateError> {
    record_completion(store, task_id, attempt_id, proof, None)
}

pub(crate) fn record_restored_open_pr(
    store: &mut StateStore,
    task: &str,
    attempt: &str,
    proof: &VerifiedOpenPr,
    restoration: &serde_json::Value,
) -> Result<(), StateError> {
    super::pr_restore::require_open_context(store, task)?;
    let identity = crate::worktree::verify_existing_worktree(store, task)
        .map_err(|_| StateError::LaunchBlocked)?;
    if identity.head != proof.head_commit_sha {
        return Err(StateError::LaunchBlocked);
    }
    record_completion(store, task, attempt, proof, Some(restoration))
}

fn record_completion(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    proof: &VerifiedOpenPr,
    restoration: Option<&serde_json::Value>,
) -> Result<(), StateError> {
    if !(stopped_exit_for_pause(store, task_id, attempt_id)?
        || natural_exit_for_attention(store, task_id, attempt_id)?)
        || proof.id == 0
        || proof.attempt_id != attempt_id
        || proof.observed_at == 0
    {
        return Err(StateError::LaunchBlocked);
    }
    verify_repair_tip(store, task_id, attempt_id, proof)?;
    let tx = store.connection.transaction()?;
    amended_dispatch::verify_committed_amendment(&tx, task_id, attempt_id)?;
    validate_verified_open_pr(&tx, task_id, attempt_id, proof)?;
    if super::pr_repair::authorization(&tx, task_id, attempt_id)?.is_some() {
        let (kind, payload) = super::pr_repair::completion_lookup(&tx, task_id, attempt_id)?;
        tx.execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,?3,?4)",
            params![task_id, attempt_id, kind, payload],
        )?;
    }
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'verified_open_pr',?3)",
        params![task_id, attempt_id, serde_json::to_string(proof)?],
    )?;
    tx.execute(
        "UPDATE tasks SET state='pr_complete' WHERE id=?1 AND state='held'",
        [task_id],
    )?;
    if tx.changes() != 1 {
        return Err(StateError::LaunchBlocked);
    }
    if let Some(restoration) = restoration {
        tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'pr_hold_restored',?3)", params![task_id, attempt_id, restoration.to_string()])?;
    }
    amended_completion::record(&tx, task_id, attempt_id)?;
    amended_dispatch::verify_committed_amendment(&tx, task_id, attempt_id)?;
    tx.commit()?;
    Ok(())
}

fn verify_repair_tip(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    proof: &VerifiedOpenPr,
) -> Result<(), StateError> {
    if let Some(audit) = super::pr_repair::authorization(&store.connection, task_id, attempt_id)? {
        let identity = crate::worktree::verify_existing_worktree(store, task_id)
            .map_err(|_| StateError::LaunchBlocked)?;
        if identity.head != proof.head_commit_sha || audit.saved_pr.id != proof.id {
            return Err(StateError::LaunchBlocked);
        }
    }
    Ok(())
}
