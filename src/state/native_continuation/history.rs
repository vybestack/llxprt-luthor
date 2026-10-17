use super::{
    records::{launch, payload, predecessor, transition},
    validation,
};
use crate::native_transition::{Context, Predecessor};
use crate::{
    model::{LaunchPlan, StateError},
    state::{amended_binding, amended_observation, proofs::parse_saved},
};
use rusqlite::{Connection, params};
use std::path::Path;

pub(crate) fn context(
    db: &Connection,
    root: &Path,
    task: &str,
    previous: &str,
) -> Result<Context, StateError> {
    let ids: Vec<String> = db
        .prepare("SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid")?
        .query_map([task], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    if ids.last().map(String::as_str) != Some(previous) {
        return Err(StateError::LaunchBlocked);
    }
    let (initial, predecessor) = history(db, root, task, &ids)?;
    Ok(Context {
        initial,
        predecessor,
    })
}

fn history(
    db: &Connection,
    root: &Path,
    task: &str,
    ids: &[String],
) -> Result<(crate::state::context::NeverDispatchedContext, Predecessor), StateError> {
    let first = ids.first().ok_or(StateError::LaunchBlocked)?;
    let original = launch(db, task, first)?;
    let dispatch: crate::state::context::AmendedDispatchProof =
        parse_saved(&payload(db, false, task, first, "supervisor_dispatch")?)?;
    let initial = amended_observation::read_context(db, root, &dispatch.effective_plan)?;
    amended_binding::verify_initial_dispatch(db, &initial, &dispatch.effective_plan)?;
    let mut previous = predecessor(
        db,
        &original,
        &dispatch.effective_plan,
        dispatch.amendment_sequence,
        None,
    )?;
    for id in &ids[1..] {
        let (sequence, transition) = transition(db, task, id)?;
        if transition.predecessor != previous {
            return Err(StateError::LaunchBlocked);
        }
        validation::validate(db, &initial, &transition)?;
        let plan = launch(db, task, id)?;
        if plan != transition.plan {
            return Err(StateError::LaunchBlocked);
        }
        previous = predecessor(
            db,
            &plan,
            &plan,
            dispatch.amendment_sequence,
            Some(sequence),
        )?;
    }
    Ok((initial, previous))
}

pub(crate) fn verify(
    db: &Connection,
    root: &Path,
    plan: &LaunchPlan,
    worker: bool,
) -> Result<(), StateError> {
    let ids: Vec<String> = db.prepare("SELECT id FROM attempts WHERE task_id=?1 AND rowid<=(SELECT rowid FROM attempts WHERE id=?2 AND task_id=?1) ORDER BY rowid")?
        .query_map(params![plan.task_id, plan.attempt_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let (_, previous) = history(db, root, &plan.task_id, &ids)?;
    if previous.effective_plan != *plan || previous.transition_sequence.is_none() {
        return Err(StateError::LaunchBlocked);
    }
    validation::dispatch(
        db,
        plan,
        previous
            .transition_sequence
            .ok_or(StateError::LaunchBlocked)?,
    )?;
    if worker {
        validation::active(db, plan)?;
    } else {
        validation::observed(db, plan)?;
    }
    Ok(())
}

pub(crate) fn context_before(
    db: &Connection,
    root: &Path,
    task: &str,
    previous: &str,
    ids: &[String],
) -> Result<Context, StateError> {
    if ids.last().map(String::as_str) != Some(previous) {
        return Err(StateError::LaunchBlocked);
    }
    let (initial, predecessor) = history(db, root, task, ids)?;
    Ok(Context {
        initial,
        predecessor,
    })
}
