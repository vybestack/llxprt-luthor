use super::{
    command::{self, PromptRequirements, enforce_prompt, requires_pair},
    error::SupervisorError,
    storage::validate_stop_socket_path,
};
use crate::state::{launches, task_records, worktree_records};
use crate::{
    config::{Config, RenderedCommand, TaskValues},
    model::{LaunchPlan, SessionEnvironment, TerminalExitProof},
    state::{ExitPrEvidence, RetryAuthorization, SelectionEvidence, StateStore, WorktreeIdentity},
    worker_instructions::prompt,
    worktree,
};
fn enforce_worktree_inspection(args: &mut [String], previous: &str) -> Result<(), SupervisorError> {
    let indexes: Vec<usize> = args
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| matches!(pair[0].as_str(), "-p" | "--prompt").then_some(index))
        .collect();
    if indexes.len() != 1 {
        return Err(SupervisorError::Conflict);
    }
    args[indexes[0] + 1].push_str(&format!(
        "\n\nBefore continuing, inspect the files left in the worktree by the {previous}. Do not assume its transcript was restored; use the files as the source of truth for what remains to be done.",
    ));
    Ok(())
}

pub fn ensure_distinct_resume_prompt(
    first: &LaunchPlan,
    latest: &LaunchPlan,
    args: &[String],
) -> Result<(), SupervisorError> {
    let continuation = prompt(args).ok_or(SupervisorError::Conflict)?;
    if prompt(&first.args) == Some(continuation) || prompt(&latest.args) == Some(continuation) {
        return Err(SupervisorError::Conflict);
    }
    Ok(())
}

/// Renders one initial attempt; the caller remains responsible for fresh claim
/// and absent-PR evidence. This function deliberately cannot start a worker.
pub fn prepare_initial(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<LaunchPlan, SupervisorError> {
    validate_stop_socket_path(store.root(), attempt_id)?;
    let selection = worktree_records::claimed_worktree_context(store, task_id)?;
    let identity = worktree::verify_existing_worktree(store, task_id)?;
    let RenderedCommand { executable, args } =
        command::initial_command(&selection, &identity, task_id, attempt_id)?;
    let worktree = identity.path.clone();
    let session_environment = SessionEnvironment::capture()?;
    let plan = LaunchPlan {
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        session_id: task_id.to_owned(),
        worktree,
        expected_worktree: identity,
        executable,
        args,
        config_revision: selection.config_revision,
        session_environment,
    };
    launches::hold_launch_intent(store, task_id, attempt_id, &serde_json::to_string(&plan)?)?;
    Ok(plan)
}

/// Renders a continuation for a verified paused task without starting a worker.
/// The new attempt must preserve the original session and verified worktree.
pub fn prepare_resume(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<LaunchPlan, SupervisorError> {
    let (first, latest, _) = launches::resume_context(store, task_id)?;
    let first: LaunchPlan = serde_json::from_str(&first)?;
    let latest: LaunchPlan = serde_json::from_str(&latest)?;
    if !first.session_environment.matches_current()? {
        return Err(SupervisorError::Conflict);
    }
    let selection =
        task_records::selection_evidence(store, task_id)?.ok_or(SupervisorError::Conflict)?;
    let identity = worktree::verify_existing_worktree(store, task_id)?;
    verify_continuation_identity(&first, &latest, task_id, &identity)?;
    if first.config_revision != selection.config_revision
        || latest.config_revision != selection.config_revision
    {
        return Err(SupervisorError::Conflict);
    }
    let worktree = identity.path.clone();
    let RenderedCommand { executable, args } = continuation_command(
        &selection,
        task_id,
        attempt_id,
        &identity,
        &first,
        &latest,
        "interrupted or canceled turn",
    )?;
    let plan = LaunchPlan {
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        session_id: task_id.to_owned(),
        worktree,
        expected_worktree: identity,
        executable,
        args,
        config_revision: selection.config_revision,
        session_environment: first.session_environment.clone(),
    };
    launches::hold_launch_intent(store, task_id, attempt_id, &serde_json::to_string(&plan)?)?;
    Ok(plan)
}

pub(crate) struct RetryPlanAuthorization<'a> {
    pub attempt_id: &'a str,
    pub config: &'a Config,
    pub revision: &'a str,
    pub actor: &'a str,
    pub reason: &'a str,
    pub pr: ExitPrEvidence,
    pub source: crate::state::RetrySourceEvidence,
    pub terminal_exit: Option<TerminalExitProof>,
}

/// Renders a separately authorized natural-exit continuation without starting a worker.
pub(crate) fn prepare_retry(
    store: &mut StateStore,
    previous: &LaunchPlan,
    authorization: RetryPlanAuthorization<'_>,
) -> Result<LaunchPlan, SupervisorError> {
    let RetryPlanAuthorization {
        attempt_id,
        config,
        revision,
        actor,
        reason,
        pr,
        source,
        terminal_exit,
    } = authorization;
    let task_id = previous.task_id.as_str();
    let first = crate::state::initial_launch(store, task_id)?;
    let first: LaunchPlan = serde_json::from_str(&first)?;
    let latest = previous;
    if !first.session_environment.matches_current()? {
        return Err(SupervisorError::Conflict);
    }
    let mut selection = crate::state::selection_for_attempt(store, previous)?;
    let previous_config = selection.effective_config.clone();
    selection.effective_config = config.into();
    let identity = worktree::verify_existing_worktree(store, task_id)?;
    verify_continuation_identity(&first, latest, task_id, &identity)?;
    let RenderedCommand { executable, args } = continuation_command(
        &selection,
        task_id,
        attempt_id,
        &identity,
        &first,
        latest,
        "naturally exited worker",
    )?;
    let worktree = identity.path.clone();
    let plan = LaunchPlan {
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        session_id: task_id.to_owned(),
        worktree,
        expected_worktree: identity,
        executable,
        args,
        config_revision: revision.to_owned(),
        session_environment: first.session_environment.clone(),
    };
    crate::state::hold_retry_intent(
        store,
        &RetryAuthorization {
            actor: actor.to_owned(),
            reason: reason.to_owned(),
            previous_plan: previous.clone(),
            previous_config,
            config: config.into(),
            plan: plan.clone(),
            reservation: attempt_id.to_owned(),
            pr,
            source,
            terminal_exit,
        },
    )?;
    Ok(plan)
}

fn continuation_command(
    selection: &SelectionEvidence,
    task_id: &str,
    attempt_id: &str,
    identity: &WorktreeIdentity,
    first: &LaunchPlan,
    latest: &LaunchPlan,
    previous_turn: &str,
) -> Result<RenderedCommand, SupervisorError> {
    let worktree = identity.path.clone();
    let cwd = worktree.to_str().ok_or(SupervisorError::Conflict)?;
    let values = TaskValues {
        task_issue_number: selection.candidate.issue_number.to_string(),
        task_repository: selection.candidate.repository.clone(),
        task_issue_url: selection.candidate.issue_url.clone(),
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        worktree: cwd.to_owned(),
    };
    let RenderedCommand {
        executable,
        mut args,
    } = selection.effective_config.resume.render(&values)?;
    let raw_continuation = prompt(&args).ok_or(SupervisorError::Conflict)?;
    if selection.effective_config.resume.args == selection.effective_config.initial.args
        || !requires_pair(&args, "--session", task_id)
        || !requires_pair(&args, "--cwd", cwd)
        || raw_continuation.trim().is_empty()
    {
        return Err(SupervisorError::Conflict);
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
    )?;
    enforce_worktree_inspection(&mut args, previous_turn)?;
    ensure_distinct_resume_prompt(first, latest, &args)?;
    Ok(RenderedCommand { executable, args })
}

fn verify_continuation_identity(
    first: &LaunchPlan,
    latest: &LaunchPlan,
    task_id: &str,
    identity: &WorktreeIdentity,
) -> Result<(), SupervisorError> {
    if first.task_id != task_id
        || latest.task_id != task_id
        || first.session_id != task_id
        || latest.session_id != task_id
        || first.worktree != identity.path
        || latest.worktree != identity.path
        || !worktree::matches_snapshot(&first.expected_worktree, identity)?
        || !worktree::matches_snapshot(&latest.expected_worktree, identity)?
    {
        return Err(SupervisorError::Conflict);
    }
    Ok(())
}
