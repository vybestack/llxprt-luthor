pub(crate) fn has_amendment(db: &Connection, plan: &LaunchPlan) -> Result<bool, StateError> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM evidence WHERE (task_id=?1 OR attempt_id=?2) AND kind='initial_branch_removed')
         OR EXISTS(SELECT 1 FROM intents WHERE (task_id=?1 OR attempt_id=?2) AND kind='initial_branch_removal_seal')",
        params![plan.task_id, plan.attempt_id], |row| row.get(0),
    )?)
}

pub(crate) fn verify_attempt_artifact(
    db: &Connection,
    root: &Path,
    task: &str,
    attempt: &str,
) -> Result<(), StateError> {
    let plan: LaunchPlan =
        super::processes::private_bytes(&root.join(format!("attempts/{attempt}.plan.json")))
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(StateError::LaunchBlocked)?;
    if plan.task_id != task || plan.attempt_id != attempt {
        return Err(StateError::LaunchBlocked);
    }
    verify_observed_plan(db, root, &plan)
}

use crate::{
    model::{LaunchPlan, SelectionEvidence, StateError},
    state::amended_dispatch,
};
use rusqlite::{Connection, params};
use std::{fs, path::Path};

/// Observation binds history only. It never authorizes a worker or releases capacity.
pub(crate) fn verify_observed_plan(
    db: &Connection,
    root: &Path,
    plan: &LaunchPlan,
) -> Result<(), StateError> {
    verify(db, root, plan, false)
}

pub(crate) fn verify_worker_plan(
    db: &Connection,
    root: &Path,
    plan: &LaunchPlan,
) -> Result<(), StateError> {
    verify(db, root, plan, true)
}

fn verify(db: &Connection, root: &Path, plan: &LaunchPlan, worker: bool) -> Result<(), StateError> {
    let tx = db.unchecked_transaction()?;
    verify_in_snapshot(&tx, root, plan, worker)?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn verify_in_snapshot(
    db: &Connection,
    root: &Path,
    plan: &LaunchPlan,
    worker: bool,
) -> Result<(), StateError> {
    if has_amendment(db, plan)? {
        let selection: String = db.query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection'",
            [&plan.task_id], |row| row.get(0),
        )?;
        let selection: SelectionEvidence = serde_json::from_str(&selection)?;
        let saved_root = &selection.effective_config.state_root;
        let actual_root = fs::canonicalize(root)?;
        let root_matches = if saved_root.is_absolute() {
            fs::canonicalize(saved_root)? == actual_root
        } else {
            actual_root.ends_with(saved_root)
        };
        if !root_matches {
            return Err(StateError::LaunchBlocked);
        }
        if worker {
            amended_dispatch::verify_worker_binding(db, saved_root, plan)?;
        } else {
            amended_dispatch::verify_observation_binding(db, saved_root, plan)?;
        }
    } else {
        verify_original(db, plan)?;
    }
    Ok(())
}

fn verify_original(db: &Connection, plan: &LaunchPlan) -> Result<(), StateError> {
    let rows: Vec<(String, String, Option<String>, String)> = db
        .prepare(
            "SELECT kind,task_id,attempt_id,detail FROM intents WHERE (task_id=?1 OR attempt_id=?2)
         AND kind IN ('launch','supervisor_dispatch') ORDER BY sequence",
        )?
        .query_map(params![plan.task_id, plan.attempt_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    let launches: Vec<_> = rows
        .iter()
        .filter(|r| r.0 == "launch" && r.2.as_deref() == Some(&plan.attempt_id))
        .collect();
    let dispatches: Vec<_> = rows
        .iter()
        .filter(|r| r.0 == "supervisor_dispatch" && r.2.as_deref() == Some(&plan.attempt_id))
        .collect();
    let ([launch], [dispatch]) = (launches.as_slice(), dispatches.as_slice()) else {
        return Err(StateError::LaunchBlocked);
    };
    if launch.1 != plan.task_id
        || dispatch.1 != plan.task_id
        || launch.3 != dispatch.3
        || serde_json::from_str::<LaunchPlan>(&launch.3)? != *plan
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
