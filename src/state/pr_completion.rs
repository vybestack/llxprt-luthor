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
    if !(stopped_exit_for_pause(store, task_id, attempt_id)?
        || natural_exit_for_attention(store, task_id, attempt_id)?)
        || proof.id == 0
        || proof.attempt_id != attempt_id
        || proof.observed_at == 0
    {
        return Err(StateError::LaunchBlocked);
    }
    let tx = store.connection.transaction()?;
    amended_dispatch::verify_committed_amendment(&tx, task_id, attempt_id)?;
    validate_verified_open_pr(&tx, task_id, attempt_id, proof)?;
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
    amended_completion::record(&tx, task_id, attempt_id)?;
    amended_dispatch::verify_committed_amendment(&tx, task_id, attempt_id)?;
    tx.commit()?;
    Ok(())
}
