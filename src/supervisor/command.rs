use super::error::SupervisorError;
pub(crate) use crate::launch_command::{PromptRequirements, requires_pair};
use crate::{
    config::RenderedCommand,
    state::{NeverDispatchedContext, SelectionEvidence, WorktreeIdentity},
};

fn command_error(error: crate::launch_command::CommandError) -> SupervisorError {
    match error {
        crate::launch_command::CommandError::Config(error) => SupervisorError::Config(error),
        crate::launch_command::CommandError::Conflict => SupervisorError::Conflict,
    }
}

pub(crate) fn enforce_prompt(
    args: &mut [String],
    requirements: PromptRequirements<'_>,
) -> Result<(), SupervisorError> {
    crate::launch_command::enforce_prompt(args, requirements).map_err(command_error)
}

pub(crate) fn initial_command(
    selection: &SelectionEvidence,
    identity: &WorktreeIdentity,
    task_id: &str,
    attempt_id: &str,
) -> Result<RenderedCommand, SupervisorError> {
    crate::launch_command::initial_command(selection, identity, task_id, attempt_id)
        .map_err(command_error)
}

/// Compare, never replace, the saved executable/argv with the initial launch
/// contract. Unsupported saved argv remains held; this does not reserve a slot.
pub fn validate_saved_initial_plan(
    context: &NeverDispatchedContext,
) -> Result<(), SupervisorError> {
    let command = initial_command(
        context.selection(),
        context.worktree_identity(),
        context.task_id(),
        context.attempt_id(),
    )?;
    let plan = context.plan();
    if command.executable != plan.executable
        || command.args != plan.args
        || plan.args.iter().any(|arg| arg.contains('\0'))
    {
        return Err(SupervisorError::Conflict);
    }
    Ok(())
}
