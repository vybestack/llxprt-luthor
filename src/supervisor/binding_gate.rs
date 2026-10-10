use super::binding;
use crate::model::{LaunchPlan, StateError};
use rusqlite::{Connection, params};
use std::path::Path;

/// Ordinary standalone runner tests keep their exact saved launch contract;
/// amended workers always require committed dispatch and registered gating.
pub(crate) fn verify(db: &Connection, root: &Path, plan: &LaunchPlan) -> Result<bool, StateError> {
    let amended = binding::has_amendment(db, plan)?;
    let dispatched: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM intents WHERE (task_id=?1 OR attempt_id=?2)
         AND kind='supervisor_dispatch' AND attempt_id=?2)",
        params![plan.task_id, plan.attempt_id],
        |r| r.get(0),
    )?;
    if amended || dispatched {
        binding::verify_in_snapshot(db, root, plan, true)?;
    } else {
        let plans: Vec<(String, String)> = db
            .prepare("SELECT task_id,detail FROM intents WHERE attempt_id=?1 AND kind='launch'")?
            .query_map([&plan.attempt_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let [(task, saved)] = plans.as_slice() else {
            return Err(StateError::LaunchBlocked);
        };
        if task != &plan.task_id || serde_json::from_str::<LaunchPlan>(saved)? != *plan {
            return Err(StateError::LaunchBlocked);
        }
    }
    Ok(amended)
}
