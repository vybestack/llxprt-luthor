use super::{history, validation};
use crate::native_transition::{Context, KIND, SEAL, Transition};
use crate::ownership::WorktreeOwnerProtocolInternal;
use crate::{
    model::{LaunchPlan, StateError},
    ownership::WorktreeOwnerProtocol,
    state::StateStore,
};
use rusqlite::{TransactionBehavior, params};

pub(crate) fn reserve(
    store: &mut StateStore,
    context: &Context,
    transition: &Transition,
) -> Result<(), StateError> {
    let root = store.root().to_path_buf();
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let p = &transition.predecessor.effective_plan;
    let fresh = history::context(&tx, &root, &p.task_id, &p.attempt_id)?;
    if fresh.initial != context.initial || fresh.predecessor != transition.predecessor {
        return Err(StateError::LaunchBlocked);
    }
    validation::validate(&tx, &fresh.initial, transition)?;
    let eligible: bool = tx.query_row("SELECT state=?2 AND NOT EXISTS(SELECT 1 FROM reservations WHERE task_id=?1 AND status='reserved') AND NOT EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND kind IN ('verified_open_pr','session_incompatible')) FROM tasks WHERE id=?1", params![p.task_id, transition.mode.phase()], |r|r.get(0))?;
    if !eligible {
        return Err(StateError::LaunchBlocked);
    }
    crate::state::process_quiescence::require_absent_processes(&tx, &p.task_id)?;
    let (capacity,reserved): (usize,usize) = tx.query_row("SELECT (SELECT value FROM state_meta WHERE key='capacity'),(SELECT COUNT(*) FROM reservations WHERE status='reserved')", [], |r|Ok((r.get(0)?,r.get(1)?)))?;
    if reserved >= capacity {
        return Err(StateError::Capacity { reserved, capacity });
    }
    let plan = &transition.plan;
    tx.execute(
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES(?1,?2,'launch_intended')",
        params![plan.attempt_id, plan.task_id],
    )?;
    tx.execute(
        "INSERT INTO reservations(attempt_id,task_id,status) VALUES(?1,?2,'reserved')",
        params![plan.attempt_id, plan.task_id],
    )?;
    tx.execute(
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'launch',?4)",
        params![
            format!("launch-{}", plan.attempt_id),
            plan.task_id,
            plan.attempt_id,
            serde_json::to_string(plan)?
        ],
    )?;
    let payload = serde_json::to_string(transition)?;
    if payload.len() > crate::state::context::MAX_SEAL_BYTES {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,?3,?4)",
        params![plan.task_id, plan.attempt_id, KIND, payload],
    )?;
    let sequence = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,?4,?5)",
        params![
            format!("native-continuation-{}", plan.attempt_id),
            plan.task_id,
            plan.attempt_id,
            SEAL,
            serde_json::to_string(&(sequence, payload))?
        ],
    )?;
    tx.execute("UPDATE tasks SET state='held' WHERE id=?1", [&plan.task_id])?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn begin_dispatch(
    store: &mut StateStore,
    plan: &LaunchPlan,
    owner: &WorktreeOwnerProtocol,
) -> Result<(), StateError> {
    let root = store.root().to_path_buf();
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let context = history::context(&tx, &root, &plan.task_id, &plan.attempt_id)?;
    if context.predecessor.effective_plan != *plan
        || !owner.matches_attempt(&plan.task_id, &plan.attempt_id)
    {
        return Err(StateError::LaunchBlocked);
    }
    validation::active(&tx, plan)?;
    crate::state::process_quiescence::require_absent_processes(&tx, &plan.task_id)?;
    let previous: usize = tx.query_row("SELECT (SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND kind='supervisor_dispatch')+(SELECT COUNT(*) FROM evidence WHERE attempt_id=?1 AND kind='worktree_owner_protocol')", [&plan.attempt_id], |r|r.get(0))?;
    if previous != 0 {
        return Err(StateError::LaunchBlocked);
    }
    let sequence = context
        .predecessor
        .transition_sequence
        .ok_or(StateError::LaunchBlocked)?;
    let proof = crate::state::context::AmendedDispatchProof {
        amendment_sequence: sequence,
        effective_plan: plan.clone(),
    };
    tx.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'supervisor_dispatch',?4)", params![format!("supervisor-{}",plan.attempt_id),plan.task_id,plan.attempt_id,serde_json::to_string(&proof)?])?;
    tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'worktree_owner_protocol',?3)", params![plan.task_id,plan.attempt_id,serde_json::to_string(owner)?])?;
    tx.commit()?;
    Ok(())
}
