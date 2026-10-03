use super::ports::{DispatchError, SupervisorLauncher};
use crate::{
    claim,
    config::Config,
    github::{
        project::ProjectReader,
        pull_request::{LookupResult, PullRequestReader, lookup},
    },
    state::{ExitPrEvidence, PausePrStatus, StateError, StateStore},
    supervisor::{self, LaunchPlan, SupervisorError},
    worktree,
};
use std::time::{SystemTime, UNIX_EPOCH};
pub struct RetryDependencies<'a, P, Q, L> {
    pub task_id: &'a str,
    pub previous_attempt_id: &'a str,
    pub attempt_id: &'a str,
    pub config: &'a Config,
    pub config_revision: &'a str,
    pub actor: &'a str,
    pub reason: &'a str,
    pub revalidate_terminal_exit: bool,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub launcher: &'a mut L,
}

/// One explicit continuation. Refusals retain the task and all old attempt evidence.
/// No assignment, worktree creation, or automatic retry is performed here.
pub fn retry_one<P: ProjectReader, Q: PullRequestReader, L: SupervisorLauncher>(
    store: &mut StateStore,
    dependencies: RetryDependencies<'_, P, Q, L>,
) -> Result<LaunchPlan, DispatchError> {
    let RetryDependencies {
        task_id,
        previous_attempt_id,
        attempt_id,
        config,
        config_revision,
        actor,
        reason,
        revalidate_terminal_exit,
        projects,
        prs,
        launcher,
    } = dependencies;
    let (previous, _) = crate::state::retry_context_for_task(store, task_id, previous_attempt_id)?;
    let selection = crate::state::selection_for_attempt(store, &previous)?;
    validate_retry_config(
        store,
        &previous,
        &selection,
        config,
        config_revision,
        actor,
        reason,
    )?;
    supervisor::validate_stop_socket_path(store.root(), attempt_id)?;
    store.ensure_dispatch_capacity()?;
    let terminal_exit = verified_retry_exit(
        store,
        task_id,
        previous_attempt_id,
        revalidate_terminal_exit,
    )?;
    let claim = store
        .source_claim_intent(task_id)?
        .ok_or(DispatchError::ChangedClaim)?;
    let expected = serde_json::json!({"principal": config.assignment_login,
        "repository": selection.candidate.repository, "number": selection.candidate.issue_number});
    if serde_json::from_str::<serde_json::Value>(&claim).ok() != Some(expected) {
        return Err(DispatchError::ChangedClaim);
    }
    let (item, issue) = claim::fresh(projects, &selection.candidate)?;
    if issue.assignees != [config.assignment_login.as_str()] {
        return Err(DispatchError::ChangedClaim);
    }
    worktree::verify_existing_worktree(store, task_id)?;
    let proof = retry_pr_absence(prs, &selection, actor)?;
    let source = crate::state::RetrySourceEvidence {
        item,
        issue,
        claim,
        observed_at_unix_secs: proof.observed_at_unix_secs,
    };
    let plan = supervisor::prepare_retry(
        store,
        &previous,
        supervisor::RetryPlanAuthorization {
            attempt_id,
            config,
            revision: config_revision,
            actor,
            reason,
            pr: proof,
            source,
            terminal_exit,
        },
    )?;
    launcher.launch(store, &plan)?;
    Ok(plan)
}

fn validate_retry_config(
    store: &StateStore,
    previous: &LaunchPlan,
    selection: &crate::model::SelectionEvidence,
    config: &Config,
    config_revision: &str,
    actor: &str,
    reason: &str,
) -> Result<(), DispatchError> {
    let current = crate::state::EffectiveConfigSnapshot::from(config);
    if config.validate().is_err()
        || config.state_root != store.root()
        || !crate::state::same_task_config(&selection.effective_config, &current)
        || config_revision.trim().is_empty()
        || config_revision == previous.config_revision
        || reason.trim().is_empty()
        || actor != selection.candidate.mapping.allowed_pr_author
    {
        return Err(StateError::InvalidConfig.into());
    }
    Ok(())
}

fn retry_pr_absence(
    prs: &mut impl PullRequestReader,
    selection: &crate::model::SelectionEvidence,
    actor: &str,
) -> Result<ExitPrEvidence, DispatchError> {
    if prs.authenticated_identity()? != actor {
        return Err(DispatchError::ChangedClaim);
    }
    let result = lookup(
        prs,
        &selection.candidate.mapping.code_repository,
        &selection.candidate.issue_url,
    );
    let status = match &result {
        Ok(LookupResult::Absent) => PausePrStatus::Absent,
        Ok(LookupResult::OpenPreexisting(_)) => PausePrStatus::Open,
        Ok(LookupResult::Ambiguous(_)) => PausePrStatus::Ambiguous,
        Err(error) => PausePrStatus::Error {
            category: error.category,
            code: error.code.into(),
            http_status: error.status,
        },
    };
    let proof = ExitPrEvidence {
        observed_at_unix_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SupervisorError::IdentityUnavailable)?
            .as_secs(),
        repository: selection.candidate.mapping.code_repository.clone(),
        status,
    };
    if result? != LookupResult::Absent {
        return Err(DispatchError::ExistingPr);
    }
    Ok(proof)
}

fn verified_retry_exit(
    store: &mut StateStore,
    task_id: &str,
    previous_attempt_id: &str,
    revalidate_terminal_exit: bool,
) -> Result<Option<crate::model::TerminalExitProof>, DispatchError> {
    let mut terminal_exit = None;
    match supervisor::recheck_retry_exit(
        store,
        task_id,
        previous_attempt_id,
        revalidate_terminal_exit,
        &mut terminal_exit,
    )? {
        supervisor::Reconciliation::Completed {
            exit_code: Some(_),
            signal: None,
        } => {}
        supervisor::Reconciliation::Held { reason } => {
            return Err(DispatchError::RetryHeld { reason });
        }
        _ => {
            return Err(DispatchError::RetryHeld {
                reason: "previous attempt is not a verified natural exit".into(),
            });
        }
    }
    Ok(terminal_exit)
}
