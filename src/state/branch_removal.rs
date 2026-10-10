use super::{
    amendment::policy,
    context::{
        AmendmentSeal, BranchRemovalReason, BranchRemovalRequest, InitialBranchRemovalAudit, KIND,
        MAX_AUDIT_BYTES, MAX_SEAL_BYTES, NeverDispatchedContext, SEAL_KIND,
    },
    continuation,
};
use crate::model::{EffectiveConfigSnapshot, LaunchPlan, StateError};
use rusqlite::{Connection, TransactionBehavior, params};
use std::{path::Path, time::SystemTime};

pub(crate) fn authorize_initial_branch_removal(
    connection: &mut Connection,
    root: &Path,
    context: &NeverDispatchedContext,
    effective: &LaunchPlan,
    request: BranchRemovalRequest<'_>,
) -> Result<i64, StateError> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let fresh = continuation::read_context(&tx, root, context.task_id(), context.attempt_id())?;
    if fresh != *context || fresh.amendment().is_some() {
        return Err(StateError::LaunchBlocked);
    }
    let current_config = EffectiveConfigSnapshot::from(request.config);
    let future_template_correction = policy::correction_for_config(&fresh, &current_config)?;
    let audit = InitialBranchRemovalAudit {
        schema_version: if future_template_correction.is_some() {
            3
        } else {
            2
        },
        prompt_version: policy::saved_prompt_version(&fresh)?,
        actor: request.actor.to_owned(),
        reason_code: BranchRemovalReason::NativeInitialBranchIsConversation,
        authorized_at_unix_secs: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_secs(),
        task_id: fresh.task_id().to_owned(),
        attempt_id: fresh.attempt_id().to_owned(),
        saved_launch_plan: fresh.saved_launch_plan().to_owned(),
        original_plan: fresh.plan().clone(),
        effective_plan: effective.clone(),
        delta: policy::delta(fresh.plan())?,
        future_template_correction,
        current_config_revision: request.config_revision.to_owned(),
        current_config,
        pr: request.pr.clone(),
        processes: request.processes,
        original_rows: fresh.snapshot.clone(),
    };
    policy::validate(&fresh, &audit)?;
    let payload = serde_json::to_string(&audit)?;
    if payload.len() > MAX_AUDIT_BYTES {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,?3,?4)",
        params![fresh.task_id(), fresh.attempt_id(), KIND, payload],
    )?;
    let sequence = tx.last_insert_rowid();
    let seal = AmendmentSeal {
        audit_sequence: sequence,
        audit_payload: payload,
    };
    let seal_payload = serde_json::to_string(&seal)?;
    if seal_payload.len() > MAX_SEAL_BYTES {
        return Err(StateError::LaunchBlocked);
    }
    tx.execute(
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,?4,?5)",
        params![
            format!("branch-removal-{}", fresh.attempt_id()),
            fresh.task_id(),
            fresh.attempt_id(),
            SEAL_KIND,
            seal_payload
        ],
    )?;
    tx.commit()?;
    Ok(sequence)
}
