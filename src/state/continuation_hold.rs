use crate::model::StateError;
use rusqlite::Connection;

/// An initial-plan amendment does not establish a later attempt's lineage.
/// Both supported versions stay held until a separate continuation contract exists.
pub(crate) fn require_unamended_task(db: &Connection, task: &str) -> Result<(), StateError> {
    let amended: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND kind='initial_branch_removed')
         OR EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND kind='initial_branch_removal_seal')
         OR EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND kind='supervisor_dispatch'
            AND CASE WHEN json_valid(detail) THEN json_type(detail,'$.amendment_sequence') IS NOT NULL
                OR json_type(detail,'$.effective_plan') IS NOT NULL ELSE 0 END)",
        [task],
        |row| row.get(0),
    )?;
    if amended {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
