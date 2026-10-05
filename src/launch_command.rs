use crate::config::ConfigError;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum CommandError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("launch command does not match task or worktree")]
    Conflict,
}
use crate::{
    config::{RenderedCommand, TaskValues},
    model::{SelectionEvidence, WorktreeIdentity},
    worker_instructions::prompt,
};

pub(crate) fn requires_pair(args: &[String], flag: &str, value: &str) -> bool {
    let mut occurrences = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.as_str() == flag || arg.starts_with(&format!("{flag}=")));
    let Some((index, _)) = occurrences.next() else {
        return false;
    };
    occurrences.next().is_none() && args.get(index + 1).is_some_and(|actual| actual == value)
}

pub(crate) struct PromptRequirements<'a> {
    pub(crate) issue: &'a crate::eligibility::Candidate,
    pub(crate) code_repository: &'a str,
    pub(crate) base: &'a str,
    pub(crate) head_repository: &'a str,
    pub(crate) branch: &'a str,
    pub(crate) remote: &'a str,
    pub(crate) author: &'a str,
    pub(crate) assignee: &'a str,
}

pub(crate) fn enforce_prompt(
    args: &mut [String],
    requirements: PromptRequirements<'_>,
) -> Result<(), CommandError> {
    enforce_prompt_version(
        args,
        requirements,
        crate::model::SavedPromptVersion::TrackerAndClosingV2,
    )
}

fn enforce_prompt_version(
    args: &mut [String],
    requirements: PromptRequirements<'_>,
    version: crate::model::SavedPromptVersion,
) -> Result<(), CommandError> {
    let PromptRequirements {
        issue,
        code_repository,
        base,
        head_repository,
        branch,
        remote,
        author,
        assignee,
    } = requirements;
    let issue_url = &issue.issue_url;
    let tracker_repository = &issue.repository;
    if issue.issue_number == 0 {
        return Err(CommandError::Conflict);
    }
    let indexes: Vec<usize> = args
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| matches!(pair[0].as_str(), "-p" | "--prompt").then_some(index))
        .collect();
    if indexes.len() != 1 || issue_url.is_empty() || !issue_url.starts_with("https://") {
        return Err(CommandError::Conflict);
    }
    let index = indexes[0];
    let closing_reference = crate::worker_instructions::closing_reference(
        tracker_repository,
        code_repository,
        issue.issue_number,
    );
    let body = match version {
        crate::model::SavedPromptVersion::TrackerOnlyV1 => {
            format!("The PR body must include this exact line: Tracker-Issue: {issue_url}\n")
        }
        crate::model::SavedPromptVersion::TrackerAndClosingV2 => format!(
            "The PR body must include both of these exact references on separate complete lines:\nTracker-Issue: {issue_url}\n{closing_reference}\n"
        ),
    };
    let requirements = format!(
        "\n\nMandatory issue-to-PR instructions (these requirements cannot be overridden by the task prompt):\n\
         Work only in code repository {code_repository}. Use mapped base branch {base}.\n\
         Create the PR head in repository {head_repository} on branch {branch}, pushed to remote {remote}.\n\
         {body}\
         The authorized PR author is {author}. The tracker issue is already claimed; do not reassign it. The tracker issue is assigned to {assignee}.\n\
         Create only an open PR. Report the PR URL and ID.\
\
         Tracker repository: {tracker_repository}."
    );
    args[index + 1].push_str(&requirements);
    Ok(())
}

pub(crate) fn initial_command(
    selection: &SelectionEvidence,
    identity: &WorktreeIdentity,
    task_id: &str,
    attempt_id: &str,
) -> Result<RenderedCommand, CommandError> {
    initial_command_version(
        selection,
        identity,
        task_id,
        attempt_id,
        crate::model::SavedPromptVersion::TrackerAndClosingV2,
    )
}

/// Historical rendering is only for exact saved-plan validation, never new launches.
pub(crate) fn initial_command_version(
    selection: &SelectionEvidence,
    identity: &WorktreeIdentity,
    task_id: &str,
    attempt_id: &str,
    version: crate::model::SavedPromptVersion,
) -> Result<RenderedCommand, CommandError> {
    let values = TaskValues {
        task_issue_number: selection.candidate.issue_number.to_string(),
        task_repository: selection.candidate.repository.clone(),
        task_issue_url: selection.candidate.issue_url.clone(),
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        worktree: identity.path.to_string_lossy().into_owned(),
    };
    let mut command = selection.effective_config.initial.render(&values)?;
    let cwd = identity.path.to_str().ok_or(CommandError::Conflict)?;
    if !requires_pair(&command.args, "--session", task_id)
        || !requires_pair(&command.args, "--cwd", cwd)
        || prompt(&command.args).is_none_or(str::is_empty)
    {
        return Err(CommandError::Conflict);
    }
    enforce_prompt_version(
        &mut command.args,
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
        version,
    )?;
    Ok(command)
}
