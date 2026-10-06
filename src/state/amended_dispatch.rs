use super::{
    context::{AmendedDispatchProof, NeverDispatchedContext},
    continuation,
};
use crate::{
    config::Config,
    model::{EffectiveConfigSnapshot, LaunchPlan, StateError},
    ownership::WorktreeOwnerProtocolInternal,
};
use rusqlite::{Connection, TransactionBehavior, params};
use std::path::Path;

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

pub(crate) fn verify_committed_amendment(
    db: &Connection,
    task: &str,
    attempt: &str,
) -> Result<(), StateError> {
    if !has_amendment(db, task, attempt)? {
        return Ok(());
    }
    let detail: String = db.query_row(
        "SELECT detail FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='supervisor_dispatch'",
        params![task, attempt], |r| r.get(0),
    )?;
    let proof: AmendedDispatchProof = super::proofs::parse_saved(&detail)?;
    let selection: String = db.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind='selection' AND attempt_id IS NULL",
        [task],
        |r| r.get(0),
    )?;
    let selection: crate::model::SelectionEvidence = super::proofs::parse_saved(&selection)?;
    if proof.effective_plan.task_id != task || proof.effective_plan.attempt_id != attempt {
        return Err(StateError::LaunchBlocked);
    }
    verify_observation_binding(
        db,
        &selection.effective_config.state_root,
        &proof.effective_plan,
    )
}

/// Inspect an explicit effective-plan proof without recording a dispatch.
pub(crate) fn amended_dispatch_proof(
    connection: &Connection,
    root: &Path,
    context: &NeverDispatchedContext,
    config: &Config,
    revision: &str,
) -> Result<AmendedDispatchProof, StateError> {
    let tx = connection.unchecked_transaction()?;
    let proof = prepare_proof(&tx, root, context, config, revision)?;
    tx.commit()?;
    Ok(proof)
}

/// Phase B dispatch entry point: recheck authorization and current config,
/// then record the exact effective plan and audit sequence once, before spawn.
/// Production launchers must opt in at all gates before using this API.
pub(crate) fn begin_amended_supervision(
    connection: &mut Connection,
    root: &Path,
    context: &NeverDispatchedContext,
    config: &Config,
    revision: &str,
    owner_protocol: &crate::ownership::WorktreeOwnerProtocol,
) -> Result<AmendedDispatchProof, StateError> {
    if !owner_protocol.matches_attempt(context.task_id(), context.attempt_id()) {
        return Err(StateError::LaunchBlocked);
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let proof = prepare_proof(&tx, root, context, config, revision)?;
    let owner_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='worktree_owner_protocol'",
        params![context.task_id(), context.attempt_id()],
        |row| row.get(0),
    )?;
    let previous: i64 = tx.query_row(
        "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='supervisor_dispatch'",
        params![context.task_id(), context.attempt_id()],
        |row| row.get(0),
    )?;
    if owner_count != 0 || previous != 0 {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'supervisor_dispatch',?4)",
            params![format!("supervisor-{}", context.attempt_id()), context.task_id(),
                context.attempt_id(), serde_json::to_string(&proof)?],
        )?;
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'worktree_owner_protocol',?3)",
        params![context.task_id(), context.attempt_id(), serde_json::to_string(owner_protocol)?],
    )?;
    tx.commit()?;
    Ok(proof)
}

fn prepare_proof(
    db: &Connection,
    root: &Path,
    context: &NeverDispatchedContext,
    config: &Config,
    revision: &str,
) -> Result<AmendedDispatchProof, StateError> {
    let fresh = continuation::read_context(db, root, context.task_id(), context.attempt_id())?;
    if fresh != *context {
        return Err(StateError::LaunchBlocked);
    }
    let (sequence, audit) = fresh.amendment.as_ref().ok_or(StateError::LaunchBlocked)?;
    if audit.current_config != EffectiveConfigSnapshot::from(config)
        || audit.current_config_revision != revision
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(AmendedDispatchProof {
        amendment_sequence: *sequence,
        effective_plan: audit.effective_plan.clone(),
    })
}

/// Read-only plan binding for phase B supervisor and worker gates, within one DB
/// snapshot. Other OS identity, worktree and gate-release checks still apply.
/// This first-launch API deliberately refuses terminal/recovery states.
pub fn verify_amended_worker_plan(
    connection: &Connection,
    root: &Path,
    plan: &LaunchPlan,
) -> Result<(), StateError> {
    let tx = connection.unchecked_transaction()?;
    verify_worker_binding(&tx, root, plan)?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn verify_worker_binding(
    db: &Connection,
    root: &Path,
    plan: &LaunchPlan,
) -> Result<(), StateError> {
    let context = continuation::read_context_mode(db, root, &plan.task_id, &plan.attempt_id, true)?;
    verify_dispatch(db, &context, plan)
}

/// Read-only binding for blocked/recovery and terminal observation. This is not
/// never-dispatched eligibility and cannot authorize another launch.
pub fn verify_amended_observation_plan(
    connection: &Connection,
    root: &Path,
    plan: &LaunchPlan,
) -> Result<(), StateError> {
    let tx = connection.unchecked_transaction()?;
    verify_observation_binding(&tx, root, plan)?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn verify_observation_binding(
    db: &Connection,
    root: &Path,
    plan: &LaunchPlan,
) -> Result<(), StateError> {
    let context = super::amended_observation::read_context(db, root, plan)?;
    verify_dispatch(db, &context, plan)
}

fn verify_dispatch(
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
         WHERE (task_id=?1 OR attempt_id=?2) AND kind='supervisor_dispatch' ORDER BY sequence",
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
        "SELECT NOT EXISTS(SELECT 1 FROM intents WHERE (task_id=?1 OR attempt_id=?2)
             AND kind IN ('supervisor_dispatch','gate_release') AND created_at <
                 (SELECT created_at FROM evidence WHERE sequence=?3))
         AND NOT EXISTS(SELECT 1 FROM evidence WHERE (task_id=?1 OR attempt_id=?2)
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
