//! Append-only authority for successors of an amended native attempt.
mod history;
mod journal;
mod records;
mod validation;
use crate::model::LaunchPlan;
pub use crate::native_transition::Mode;
pub(crate) use history::{context, verify};
pub(crate) use journal::{begin_dispatch, reserve};

pub(crate) fn is_successor(
    db: &rusqlite::Connection,
    plan: &crate::model::LaunchPlan,
) -> Result<bool, crate::model::StateError> {
    Ok(db.query_row(
        "SELECT id!=?2 FROM attempts WHERE task_id=?1 ORDER BY rowid LIMIT 1",
        rusqlite::params![plan.task_id, plan.attempt_id],
        |r| r.get(0),
    )?)
}

pub(crate) fn selection(
    db: &rusqlite::Connection,
    plan: &crate::model::LaunchPlan,
) -> Result<crate::model::SelectionEvidence, crate::model::StateError> {
    let (sequence, t) = records::transition(db, &plan.task_id, &plan.attempt_id)?;
    let payload = records::payload(db, false, &plan.task_id, &plan.attempt_id, "launch")?;
    if super::proofs::parse_saved::<LaunchPlan>(&payload)? != *plan || t.plan != *plan {
        return Err(crate::model::StateError::LaunchBlocked);
    }
    let ids: Vec<String> = db.prepare("SELECT id FROM attempts WHERE task_id=?1 AND rowid<(SELECT rowid FROM attempts WHERE id=?2) ORDER BY rowid")?.query_map(rusqlite::params![plan.task_id,plan.attempt_id], |r|r.get(0))?.collect::<Result<_,_>>()?;
    let previous = ids.last().ok_or(crate::model::StateError::LaunchBlocked)?;
    let context = history::context_before(db, &t.config.state_root, &plan.task_id, previous, &ids)?;
    if context.predecessor != t.predecessor || sequence <= 0 {
        return Err(crate::model::StateError::LaunchBlocked);
    }
    validation::validate(db, &context.initial, &t)?;
    validation::selection(&context.initial, &t)
}
