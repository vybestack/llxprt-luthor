use super::context::{AmendedDispatchProof, NeverDispatchedContext};
use crate::model::{LaunchPlan, StateError};
use rusqlite::{Connection, params};
pub(crate) fn verify_initial_dispatch(
    db: &Connection,
    context: &NeverDispatchedContext,
    plan: &LaunchPlan,
) -> Result<(), StateError> {
    super::dispatch_stages::verify(db, &plan.task_id, &plan.attempt_id)?;
    let (sequence, audit) = context
        .amendment
        .as_ref()
        .ok_or(StateError::LaunchBlocked)?;
    let rows: Vec<(String, Option<String>, String)> = db
        .prepare(
            "SELECT task_id,attempt_id,detail FROM intents
         WHERE attempt_id=?2 AND kind='supervisor_dispatch' ORDER BY sequence",
        )?
        .query_map(params![plan.task_id, plan.attempt_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    let [(task, attempt, payload)] = rows.as_slice() else {
        return Err(StateError::LaunchBlocked);
    };
    let proof: AmendedDispatchProof = super::proofs::parse_saved(payload)?;
    if task != &plan.task_id
        || attempt.as_deref() != Some(&plan.attempt_id)
        || proof.amendment_sequence != *sequence
        || proof.effective_plan != audit.effective_plan
        || *plan != audit.effective_plan
    {
        return Err(StateError::LaunchBlocked);
    }
    let ordered: bool = db.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM intents WHERE attempt_id=?2
             AND kind IN ('supervisor_dispatch','gate_release') AND created_at <
                 (SELECT created_at FROM evidence WHERE sequence=?3))
         AND NOT EXISTS(SELECT 1 FROM evidence WHERE attempt_id=?2
             AND kind IN ('supervisor_ready','child_registered','tracked_descendant','gate_sent')
             AND sequence <= ?3)",
        params![plan.task_id, plan.attempt_id, sequence],
        |row| row.get(0),
    )?;
    if !ordered {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
