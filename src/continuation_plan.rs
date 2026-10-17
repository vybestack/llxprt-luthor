//! Pure native continuation rendering, shared by authorization and process gates.
use crate::{
    launch_command::{PromptRequirements, enforce_prompt, requires_pair},
    model::{LaunchPlan, SelectionEvidence, StateError},
    worker_instructions::prompt,
};

pub(crate) fn render(
    selection: &SelectionEvidence,
    first: &LaunchPlan,
    previous: &LaunchPlan,
    attempt: &str,
    stopped: bool,
) -> Result<LaunchPlan, StateError> {
    let identity = &previous.expected_worktree;
    let values = crate::config::TaskValues {
        task_issue_number: selection.candidate.issue_number.to_string(),
        task_repository: selection.candidate.repository.clone(),
        task_issue_url: selection.candidate.issue_url.clone(),
        task_id: previous.task_id.clone(),
        attempt_id: attempt.to_owned(),
        worktree: previous
            .worktree
            .to_str()
            .ok_or(StateError::LaunchBlocked)?
            .to_owned(),
    };
    let command = selection
        .effective_config
        .resume
        .render(&values)
        .map_err(|_| StateError::LaunchBlocked)?;
    let mut args = command.args;
    if command.executable != first.executable
        || args
            .iter()
            .any(|a| a == "--branch" || a.starts_with("--branch="))
        || !requires_pair(&args, "--session", &first.session_id)
        || !requires_pair(&args, "--cwd", &values.worktree)
        || prompt(&args).is_none_or(|p| p.trim().is_empty())
    {
        return Err(StateError::LaunchBlocked);
    }
    enforce_prompt(
        &mut args,
        PromptRequirements {
            issue: &selection.candidate,
            code_repository: &selection.candidate.mapping.code_repository,
            base: &identity.base,
            head_repository: &selection.candidate.mapping.allowed_pr_head_repository,
            branch: &identity.branch,
            remote: &identity.remote,
            author: &selection.candidate.mapping.allowed_pr_author,
            assignee: &selection.effective_config.assignment_login,
        },
    )
    .map_err(|_| StateError::LaunchBlocked)?;
    let indexes: Vec<_> = args
        .windows(2)
        .enumerate()
        .filter_map(|(i, pair)| matches!(pair[0].as_str(), "-p" | "--prompt").then_some(i))
        .collect();
    let [index] = indexes.as_slice() else {
        return Err(StateError::LaunchBlocked);
    };
    let turn = if stopped {
        "interrupted or canceled turn"
    } else {
        "naturally exited worker"
    };
    args[index + 1].push_str(&format!(
        "\n\nBefore continuing attempt {attempt}, inspect the files left in the worktree by the {turn} (predecessor {}). Do not assume its transcript was restored; use the files as the source of truth for what remains to be done.", previous.attempt_id
    ));
    if prompt(&args) == prompt(&first.args) || prompt(&args) == prompt(&previous.args) {
        return Err(StateError::LaunchBlocked);
    }
    let mut plan = previous.clone();
    plan.attempt_id = attempt.to_owned();
    plan.config_revision = selection.config_revision.clone();
    plan.args = args;
    Ok(plan)
}
