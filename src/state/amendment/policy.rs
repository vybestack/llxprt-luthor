mod config_transition;
#[cfg(test)]
mod dual_live_check;
#[cfg(test)]
mod live_check;
use crate::state::context::{
    BranchRemovalDelta, InitialBranchRemovalAudit, NeverDispatchedContext, ProcessQuiescence,
    valid_actor,
};
use crate::{
    config::Config,
    model::{EffectiveConfigSnapshot, LaunchPlan, PausePrStatus, StateError},
};
use config_transition::branch_index;
pub(crate) use config_transition::correction_for_config;

pub(crate) fn delta(plan: &LaunchPlan) -> Result<BranchRemovalDelta, StateError> {
    let index = branch_index(&plan.args, &format!("luthor/{}", plan.task_id))?;
    Ok(BranchRemovalDelta {
        index,
        removed: [plan.args[index].clone(), plan.args[index + 1].clone()],
    })
}

pub(crate) fn saved_prompt_version(
    context: &NeverDispatchedContext,
) -> Result<crate::model::SavedPromptVersion, StateError> {
    use crate::model::SavedPromptVersion::{TrackerAndClosingV2, TrackerOnlyV1};
    for version in [TrackerAndClosingV2, TrackerOnlyV1] {
        let rendered = crate::launch_command::initial_command_version(
            context.selection(),
            context.worktree_identity(),
            context.task_id(),
            context.attempt_id(),
            version,
        )
        .map_err(|_| StateError::LaunchBlocked)?;
        if rendered.executable == context.plan().executable && rendered.args == context.plan().args
        {
            return Ok(version);
        }
    }
    Err(StateError::LaunchBlocked)
}

/// Shared proposal validation. This prepares no audit and grants no spawn authority.
pub(crate) fn initial_branch_removal_plan(
    context: &NeverDispatchedContext,
) -> Result<LaunchPlan, StateError> {
    let original = context.plan();
    if original
        .executable
        .file_name()
        .is_none_or(|name| name != "llxprt-code-rs")
        || original.args.iter().any(|arg| arg.contains('\0'))
    {
        return Err(StateError::LaunchBlocked);
    }
    saved_prompt_version(context)?;
    let correction = delta(original)?;
    let mut effective = original.clone();
    effective.args.drain(correction.index..correction.index + 2);
    Ok(effective)
}

fn valid_revision(revision: &str) -> bool {
    !revision.trim().is_empty() && revision.len() <= 128 && !revision.contains(['\0', '\n', '\r'])
}

pub(crate) fn validate_removal_config(
    context: &NeverDispatchedContext,
    config: &Config,
    revision: &str,
) -> Result<(), StateError> {
    if !valid_revision(revision) {
        return Err(StateError::LaunchBlocked);
    }
    correction_for_config(context, &EffectiveConfigSnapshot::from(config))?;
    Ok(())
}

pub(crate) fn validate(
    context: &NeverDispatchedContext,
    audit: &InitialBranchRemovalAudit,
) -> Result<(), StateError> {
    config_transition::validate_version(context, audit)?;
    if audit.prompt_version != saved_prompt_version(context)?
        || !valid_actor(&audit.actor, context)
        || audit.authorized_at_unix_secs == 0
        || audit.task_id != context.task_id()
        || audit.attempt_id != context.attempt_id()
        || audit.saved_launch_plan != context.saved_launch_plan()
        || audit.original_plan != *context.plan()
        || audit.original_rows != context.snapshot
        || !valid_revision(&audit.current_config_revision)
        || audit.current_config.state_root != context.root
    {
        return Err(StateError::LaunchBlocked);
    }
    validate_observations(context, audit)?;
    if audit.delta != delta(context.plan())?
        || audit.effective_plan != initial_branch_removal_plan(context)?
    {
        return Err(StateError::LaunchBlocked);
    }
    validate_config(context, audit)
}

fn validate_config(
    context: &NeverDispatchedContext,
    audit: &InitialBranchRemovalAudit,
) -> Result<(), StateError> {
    // The config revision belongs in provenance; the saved launch revision stays fixed.
    let mut selection = context.selection().clone();
    selection.effective_config = audit.current_config.clone();
    let rendered = crate::launch_command::initial_command_version(
        &selection,
        context.worktree_identity(),
        context.task_id(),
        context.attempt_id(),
        audit.prompt_version,
    )
    .map_err(|_| StateError::LaunchBlocked)?;
    if rendered.executable != audit.effective_plan.executable
        || rendered.args != audit.effective_plan.args
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

fn validate_observations(
    context: &NeverDispatchedContext,
    audit: &InitialBranchRemovalAudit,
) -> Result<(), StateError> {
    if audit.pr.status != PausePrStatus::Absent
        || audit.pr.repository != context.selection().candidate.mapping.code_repository
        || audit.pr.observed_at_unix_secs == 0
        || audit.pr.observed_at_unix_secs > audit.authorized_at_unix_secs
        || !matches!(audit.processes, ProcessQuiescence::Clear { observed_at_unix_secs }
            if observed_at_unix_secs > 0 && observed_at_unix_secs <= audit.authorized_at_unix_secs)
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
