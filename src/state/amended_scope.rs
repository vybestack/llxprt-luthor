use crate::model::StateError;
use rusqlite::{Connection, params};
pub(crate) fn has_amendment(
    db: &Connection,
    task: &str,
    attempt: &str,
) -> Result<bool, StateError> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM evidence WHERE (task_id=?1 OR attempt_id=?2) AND kind='initial_branch_removed')
         OR EXISTS(SELECT 1 FROM intents WHERE (task_id=?1 OR attempt_id=?2) AND kind='initial_branch_removal_seal')
         OR EXISTS(SELECT 1 FROM intents WHERE (task_id=?1 OR attempt_id=?2) AND kind='supervisor_dispatch'
            AND CASE WHEN json_valid(detail) THEN json_type(detail,'$.amendment_sequence') IS NOT NULL
                OR json_type(detail,'$.effective_plan') IS NOT NULL ELSE 0 END)",
        params![task, attempt], |r| r.get(0),
    )?)
}
