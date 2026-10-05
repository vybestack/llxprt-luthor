use super::{
    context::{
        AmendmentSeal, InitialBranchRemovalAudit, KIND, MAX_AUDIT_BYTES, MAX_SEAL_BYTES,
        NeverDispatchedContext, SEAL_KIND,
    },
    proofs::parse_saved,
};
use crate::model::StateError;
use rusqlite::{Connection, params};
pub(crate) mod policy;

pub(crate) fn read_audit(
    db: &Connection,
    context: &NeverDispatchedContext,
) -> Result<Option<(i64, InitialBranchRemovalAudit)>, StateError> {
    let rows: Vec<(i64, String, Option<String>, String)> = db
        .prepare(
            "SELECT sequence,task_id,attempt_id,payload FROM evidence
         WHERE (task_id=?1 OR attempt_id=?2) AND kind=?3 ORDER BY sequence",
        )?
        .query_map(
            params![context.task_id(), context.attempt_id(), KIND],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?
        .collect::<Result<_, _>>()?;
    let seals: Vec<(String, String, Option<String>, String)> = db
        .prepare(
            "SELECT id,task_id,attempt_id,detail FROM intents
         WHERE (task_id=?1 OR attempt_id=?2) AND kind=?3 ORDER BY sequence",
        )?
        .query_map(
            params![context.task_id(), context.attempt_id(), SEAL_KIND],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?
        .collect::<Result<_, _>>()?;
    if seals.is_empty() && rows.is_empty() {
        return Ok(None);
    }
    let seal = read_seal(&seals, context)?;
    match rows.as_slice() {
        [(sequence, task, attempt, payload)]
            if *sequence > 0
                && task == context.task_id()
                && audit_payload_matches(attempt, payload, context)
                && seal.audit_sequence == *sequence
                && seal.audit_payload == *payload =>
        {
            let audit = parse_saved(payload)?;
            policy::validate(context, &audit)?;
            Ok(Some((*sequence, audit)))
        }
        _ => Err(StateError::LaunchBlocked),
    }
}

fn read_seal(
    seals: &[(String, String, Option<String>, String)],
    context: &NeverDispatchedContext,
) -> Result<AmendmentSeal, StateError> {
    match seals {
        [(id, task, attempt, payload)]
            if id == &format!("branch-removal-{}", context.attempt_id())
                && task == context.task_id()
                && attempt.as_deref() == Some(context.attempt_id())
                && payload.len() <= MAX_SEAL_BYTES =>
        {
            parse_saved::<AmendmentSeal>(payload)
        }
        _ => Err(StateError::LaunchBlocked),
    }
}

fn audit_payload_matches(
    attempt: &Option<String>,
    payload: &str,
    context: &NeverDispatchedContext,
) -> bool {
    attempt.as_deref() == Some(context.attempt_id()) && payload.len() <= MAX_AUDIT_BYTES
}
