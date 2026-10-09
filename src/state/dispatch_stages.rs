use crate::model::StateError;
use rusqlite::{Connection, params};

/// Stage evidence can be absent before READY, but cannot be duplicated or
/// attached to another task/attempt. Observation never invents missing stages.
pub(crate) fn verify(db: &Connection, task: &str, attempt: &str) -> Result<(), StateError> {
    let invalid: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM intents WHERE (task_id=?1 OR attempt_id=?2)
            AND kind IN ('supervisor_dispatch','gate_release','stop')
            AND (task_id!=?1 OR attempt_id IS NULL OR attempt_id!=?2))
         OR EXISTS(SELECT 1 FROM evidence WHERE (task_id=?1 OR attempt_id=?2)
            AND kind IN ('supervisor_ready','child_registered','tracked_descendant','gate_sent',
                'attempt_exit','log_failure','exit_pr_lookup','pause_pr_lookup','attention_reason',
                'independent_stop_decision','independent_stop_signal','independent_group_absent')
            AND (task_id!=?1 OR attempt_id IS NULL OR attempt_id!=?2))
         OR EXISTS(SELECT kind FROM intents WHERE (task_id=?1 OR attempt_id=?2)
            AND kind IN ('supervisor_dispatch','gate_release','stop') GROUP BY kind HAVING COUNT(*)>1)
         OR EXISTS(SELECT kind FROM evidence WHERE (task_id=?1 OR attempt_id=?2)
            AND kind IN ('supervisor_ready','child_registered','gate_sent','attempt_exit',
                'attention_reason') GROUP BY kind HAVING COUNT(*)>1)",
        params![task, attempt], |r| r.get(0),
    )?;
    if invalid {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
