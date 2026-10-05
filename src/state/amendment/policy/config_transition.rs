use crate::{
    model::{EffectiveConfigSnapshot, StateError},
    state::context::{BranchRemovalDelta, FutureTemplateCorrection, NeverDispatchedContext},
};

pub(crate) fn branch_index(args: &[String], value: &str) -> Result<usize, StateError> {
    let positions: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == "--branch" || arg.starts_with("--branch="))
        .map(|(index, _)| index)
        .collect();
    match positions.as_slice() {
        [index]
            if args[*index] == "--branch"
                && args.get(index + 1).is_some_and(|arg| arg == value) =>
        {
            Ok(*index)
        }
        _ => Err(StateError::LaunchBlocked),
    }
}

fn remove_template_pair(args: &mut Vec<String>) -> Result<BranchRemovalDelta, StateError> {
    let index = branch_index(args, "luthor/{task.id}")?;
    let delta = BranchRemovalDelta {
        index,
        removed: [args[index].clone(), args[index + 1].clone()],
    };
    args.drain(index..index + 2);
    Ok(delta)
}

fn initial_corrected(
    context: &NeverDispatchedContext,
) -> Result<(EffectiveConfigSnapshot, BranchRemovalDelta), StateError> {
    let mut corrected = context.selection().effective_config.clone();
    let initial = remove_template_pair(&mut corrected.initial.args)?;
    Ok((corrected, initial))
}

/// Classify only the two exact transitions, without rebinding the saved task.
pub(crate) fn correction_for_config(
    context: &NeverDispatchedContext,
    current: &EffectiveConfigSnapshot,
) -> Result<Option<FutureTemplateCorrection>, StateError> {
    let (mut corrected, initial) = initial_corrected(context)?;
    if *current == corrected {
        return Ok(None);
    }
    if corrected
        .resume
        .executable
        .file_name()
        .is_none_or(|name| name != "llxprt-code-rs")
    {
        return Err(StateError::LaunchBlocked);
    }
    let resume = remove_template_pair(&mut corrected.resume.args)?;
    if *current != corrected {
        return Err(StateError::LaunchBlocked);
    }
    Ok(Some(
        FutureTemplateCorrection::NativeInitialAndResumeBranchRemovalV1 { initial, resume },
    ))
}

pub(crate) fn validate_version(
    context: &NeverDispatchedContext,
    audit: &crate::state::context::InitialBranchRemovalAudit,
) -> Result<(), StateError> {
    let expected = correction_for_config(context, &audit.current_config)?;
    let version = if expected.is_some() { 3 } else { 2 };
    if audit.schema_version != version || audit.future_template_correction != expected {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
