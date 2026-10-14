//! Cleanup eligibility and append-only intent/result journal, separate from launch state.
use super::{database::StateStore, proofs};
use crate::model::{StateError, VerifiedOpenPr};
use rusqlite::params;

pub(crate) fn candidates(store: &StateStore) -> Result<Vec<String>, StateError> {
    let mut statement = store.connection.prepare(
        "SELECT id FROM tasks t WHERE state='pr_complete'
         AND NOT EXISTS(SELECT 1 FROM evidence WHERE task_id=t.id AND kind='cleanup_complete')
         ORDER BY rowid",
    )?;
    Ok(statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

pub(crate) fn verified_pr(store: &StateStore, task: &str) -> Result<VerifiedOpenPr, StateError> {
    let attempt =
        super::task_records::latest_attempt(store, task)?.ok_or(StateError::LaunchBlocked)?;
    let valid: bool = store.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND state='pr_complete')
         AND EXISTS(SELECT 1 FROM attempts WHERE task_id=?1 AND id=?2 AND lifecycle='completed' AND outcome IS NOT NULL)
         AND NOT EXISTS(SELECT 1 FROM reservations WHERE task_id=?1 AND status='reserved')
         AND NOT EXISTS(SELECT 1 FROM attempts WHERE task_id=?1 AND (lifecycle!='completed' OR outcome IS NULL))",
        params![task, attempt], |row| row.get(0),
    )?;
    if !valid {
        return Err(StateError::LaunchBlocked);
    }
    let proof = proofs::current_pr(&store.connection, task, &attempt)?;
    if proof.attempt_id != attempt || proof.id == 0 || proof.number == 0 {
        return Err(StateError::LaunchBlocked);
    }
    let selection = super::task_records::selection_evidence(store, task)?
        .ok_or(StateError::InvalidSelection)?;
    let record =
        super::worktree_records::worktree_record(store, task)?.ok_or(StateError::LaunchBlocked)?;
    let identity = record.identity.ok_or(StateError::LaunchBlocked)?;
    proofs::validate_pr_identity(&selection, &record.intent, &identity, &proof)?;
    Ok(proof)
}

pub(crate) fn begin(store: &mut StateStore, task: &str, detail: &str) -> Result<(), StateError> {
    verified_pr(store, task)?;
    // An interrupted deletion is uncertain, never silently re-adopted or repeated.
    let count: u64 = store.connection.query_row(
        "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND kind='cleanup'",
        [task],
        |row| row.get(0),
    )?;
    if count != 0 {
        return Err(StateError::LaunchBlocked);
    }
    super::journal::record_intent(
        store,
        &format!("cleanup-{task}"),
        task,
        None,
        "cleanup",
        detail,
    )
}

pub(crate) fn has_intent(store: &StateStore, task: &str) -> Result<bool, StateError> {
    Ok(store.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND kind='cleanup')",
        [task],
        |row| row.get(0),
    )?)
}
