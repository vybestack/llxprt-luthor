use crate::{
    model::{LaunchPlan, StateError},
    native_transition::{KIND, Predecessor, SEAL, Transition},
    state::proofs::parse_saved,
};
use rusqlite::{Connection, params};
pub(crate) fn predecessor(
    db: &Connection,
    original: &LaunchPlan,
    effective: &LaunchPlan,
    amendment_sequence: i64,
    transition_sequence: Option<i64>,
) -> Result<Predecessor, StateError> {
    Ok(Predecessor {
        original_plan: original.clone(),
        effective_plan: effective.clone(),
        amendment_sequence,
        transition_sequence,
        intents: rows(db, false, &effective.task_id, &effective.attempt_id)?,
        evidence: rows(db, true, &effective.task_id, &effective.attempt_id)?,
    })
}
fn rows(
    db: &Connection,
    evidence: bool,
    task: &str,
    attempt: &str,
) -> Result<Vec<(i64, String, String)>, StateError> {
    let sql = if evidence {
        "SELECT sequence,kind,payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 ORDER BY sequence"
    } else {
        "SELECT sequence,kind,detail FROM intents WHERE task_id=?1 AND attempt_id=?2 ORDER BY sequence"
    };
    Ok(db
        .prepare(sql)?
        .query_map(params![task, attempt], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?)
}
pub(crate) fn payload(
    db: &Connection,
    evidence: bool,
    task: &str,
    attempt: &str,
    kind: &str,
) -> Result<String, StateError> {
    crate::state::proofs::unique_payload(db, evidence, task, Some(attempt), kind)?
        .ok_or(StateError::LaunchBlocked)
}
pub(crate) fn launch(db: &Connection, task: &str, attempt: &str) -> Result<LaunchPlan, StateError> {
    let valid: bool = db.query_row("SELECT NOT EXISTS(SELECT 1 FROM evidence WHERE attempt_id=?2 AND task_id IS NOT ?1) AND NOT EXISTS(SELECT 1 FROM intents WHERE attempt_id=?2 AND task_id IS NOT ?1)",params![task,attempt],|r|r.get(0))?;
    if !valid {
        return Err(StateError::LaunchBlocked);
    }
    parse_saved(&payload(db, false, task, attempt, "launch")?)
}
pub(crate) fn transition(
    db: &Connection,
    task: &str,
    attempt: &str,
) -> Result<(i64, Transition), StateError> {
    let rows: Vec<(i64, String)> = db
        .prepare(
            "SELECT sequence,payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind=?3",
        )?
        .query_map(params![task, attempt, KIND], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let [(sequence, payload)] = rows.as_slice() else {
        return Err(StateError::LaunchBlocked);
    };
    let seal = self::payload(db, false, task, attempt, SEAL)?;
    let sealed: (i64, String) = parse_saved(&seal)?;
    if sealed != (*sequence, payload.clone())
        || payload.len() > crate::state::context::MAX_SEAL_BYTES
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok((*sequence, parse_saved(payload)?))
}
