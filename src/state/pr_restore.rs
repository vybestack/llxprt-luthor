//! Audited return of finished tasks that an earlier release wrongly held.
use super::database::StateStore;
use crate::{model::StateError, pr_repair::UNSPECIFIC_REPAIR_HOLD_REASON};
use rusqlite::params;

/// A task qualifies only when the unspecific repair hold is its latest recorded
/// reason and nothing about the task is live: no reservation, no unfinished
/// attempt (an interrupted repair launch keeps both) and no cleanup intent.
const ACCOUNTED: &str = "t.state='held'
    AND EXISTS(SELECT 1 FROM attempts WHERE task_id=t.id)
    AND NOT EXISTS(SELECT 1 FROM reservations WHERE task_id=t.id AND status='reserved')
    AND NOT EXISTS(SELECT 1 FROM attempts WHERE task_id=t.id AND (lifecycle!='completed' OR outcome IS NULL))
    AND NOT EXISTS(SELECT 1 FROM intents WHERE task_id=t.id AND kind='cleanup')";

fn terminal_predicate() -> String {
    format!(
        "{ACCOUNTED} AND (SELECT payload FROM evidence WHERE task_id=t.id AND kind='held_reason' ORDER BY sequence DESC LIMIT 1)=?1"
    )
}

pub(crate) fn candidates(store: &StateStore) -> Result<Vec<String>, StateError> {
    Ok(store
        .connection
        .prepare(&format!(
            "SELECT t.id FROM tasks t WHERE {} ORDER BY t.rowid",
            terminal_predicate()
        ))?
        .query_map([UNSPECIFIC_REPAIR_HOLD_REASON], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

/// Moves the task back to `pr_complete` and appends the proof that justified
/// it. The qualification is rechecked in the same transaction as the update.
pub(crate) fn restore(
    store: &mut StateStore,
    task: &str,
    proof: &serde_json::Value,
) -> Result<(), StateError> {
    let tx = store.connection.transaction()?;
    let qualified: bool = tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM tasks t WHERE {} AND t.id=?2)",
            terminal_predicate()
        ),
        params![UNSPECIFIC_REPAIR_HOLD_REASON, task],
        |row| row.get(0),
    )?;
    if !qualified {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "UPDATE tasks SET state='pr_complete' WHERE id=?1 AND state='held'",
        [task],
    )?;
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'pr_hold_restored',?2)",
        params![task, proof.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Exact historical and typed identity holds only. Limits, semantic conflicts,
/// config/session holds and uncertain launches stay held.
const OPEN_REASONS: &str = "(SELECT payload FROM evidence WHERE task_id=t.id AND kind='held_reason' ORDER BY sequence DESC LIMIT 1) IN (SELECT value FROM json_each(?1))";

fn open_reasons() -> Result<String, StateError> {
    Ok(serde_json::to_string(
        &crate::pr_repair::open_hold_reasons(),
    )?)
}

fn open_predicate() -> String {
    format!(
        "{ACCOUNTED} AND {OPEN_REASONS} AND NOT EXISTS(SELECT 1 FROM evidence WHERE task_id=t.id AND kind IN ('pr_repair_blocked','session_incompatible','initial_branch_removed'))"
    )
}

pub(crate) fn open_candidates(store: &StateStore) -> Result<Vec<String>, StateError> {
    Ok(store
        .connection
        .prepare(&format!(
            "SELECT t.id FROM tasks t WHERE {} ORDER BY t.rowid",
            open_predicate()
        ))?
        .query_map([open_reasons()?], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

pub(crate) fn restore_open(
    store: &mut StateStore,
    task: &str,
    proof: &serde_json::Value,
) -> Result<(), StateError> {
    let tx = store.connection.transaction()?;
    let qualified: bool = tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM tasks t WHERE {} AND t.id=?2)",
            open_predicate()
        ),
        params![open_reasons()?, task],
        |row| row.get(0),
    )?;
    if !qualified {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "UPDATE tasks SET state='pr_complete' WHERE id=?1 AND state='held'",
        [task],
    )?;
    if tx.changes() != 1 {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'pr_hold_restored',?2)", params![task, proof.to_string()])?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn require_open_context(store: &StateStore, task: &str) -> Result<(), StateError> {
    super::continuation_hold::require_unamended_task(&store.connection, task)?;
    if !open_candidates(store)?
        .iter()
        .any(|candidate| candidate == task)
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
