//! Monitor verified PRs and dispatch only bounded, freshly authorized repairs.
use super::{
    failed_checks::{self, FailedChecks},
    ids::IdCreator,
    ports::SupervisorLauncher,
    ports::{
        DispatchError, PrVerificationStage, observation_time, verify_completion_claim,
        verify_open_pr,
    },
};
use crate::{
    claim,
    github::{
        project::ProjectReader,
        pull_request::{LookupResult, PrHealthReader, lookup},
    },
    ownership::{WorktreeOwner, WorktreeOwnerInternal},
    pr_repair::MAX_PR_REPAIRS,
    state::{StateError, StateStore, journal, pr_repair, scheduling, task_records},
    supervisor,
};

pub fn repair_verified_prs<
    P: ProjectReader,
    Q: PrHealthReader,
    L: SupervisorLauncher,
    I: IdCreator,
>(
    store: &mut StateStore,
    config: &crate::config::Config,
    projects: &mut P,
    prs: &mut Q,
    launcher: &mut L,
    ids: &mut I,
) -> Result<Vec<supervisor::LaunchPlan>, DispatchError> {
    repair_verified_prs_after_cleanup(
        store,
        config,
        projects,
        prs,
        launcher,
        ids,
        &Default::default(),
    )
}

/// Do not retry a failed cleanup task in PR repair during the same cycle: its
/// ownership, historical configuration or upstream proof may be unavailable.
pub fn repair_verified_prs_after_cleanup<
    P: ProjectReader,
    Q: PrHealthReader,
    L: SupervisorLauncher,
    I: IdCreator,
>(
    store: &mut StateStore,
    config: &crate::config::Config,
    projects: &mut P,
    prs: &mut Q,
    launcher: &mut L,
    ids: &mut I,
    cleanup: &crate::cleanup::CleanupReport,
) -> Result<Vec<supervisor::LaunchPlan>, DispatchError> {
    let mut launched = Vec::new();
    for task in pr_repair::candidates(store)? {
        if cleanup.skipped.iter().any(|skip| skip.task_id == task) {
            continue;
        }
        match inspect_config(store, config, &task, projects, prs, launcher, ids) {
            Ok(Some(plan)) => launched.push(plan),
            Ok(None) => {}
            Err(error) => {
                task_records::hold_task(
                    store,
                    &task,
                    "PR repair validation or launch failed; inspect task evidence",
                )?;
                return Err(error);
            }
        }
    }
    Ok(launched)
}

fn inspect_config<P: ProjectReader, Q: PrHealthReader, L: SupervisorLauncher, I: IdCreator>(
    store: &mut StateStore,
    config: &crate::config::Config,
    task: &str,
    projects: &mut P,
    prs: &mut Q,
    launcher: &mut L,
    ids: &mut I,
) -> Result<Option<supervisor::LaunchPlan>, DispatchError> {
    let (previous, _) = pr_repair::context(store, task)?;
    let selection = crate::state::selection_for_attempt(store, &previous)?;
    if selection.effective_config != crate::model::EffectiveConfigSnapshot::from(config) {
        return Err(StateError::InvalidConfig.into());
    }
    inspect_one(store, task, projects, prs, launcher, ids)
}

fn inspect_one<P: ProjectReader, Q: PrHealthReader, L: SupervisorLauncher, I: IdCreator>(
    store: &mut StateStore,
    task: &str,
    projects: &mut P,
    prs: &mut Q,
    launcher: &mut L,
    ids: &mut I,
) -> Result<Option<supervisor::LaunchPlan>, DispatchError> {
    let saved = pr_repair::saved_pr(&store.connection, task)?;
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    // Exhaustive absence covers a merged/closed PR and leaves cleanup to its
    // separate exact terminal-PR proof. It must never trigger a new PR.
    let result = lookup(prs, &saved.repository, &saved.tracker_issue_url)?;
    let pr = match result {
        LookupResult::Absent => return Ok(None),
        LookupResult::OpenPreexisting(pr) => *pr,
        LookupResult::Ambiguous(_) => return Err(StateError::LaunchBlocked.into()),
    };
    let observed = verify_open_pr(
        store,
        task,
        &saved.attempt_id,
        prs,
        (pr, observation_time()?),
        PrVerificationStage::Exit,
    )
    .map_err(|_| supervisor::SupervisorError::Conflict)?;
    if observed.id != saved.id
        || observed.number != saved.number
        || observed.head_commit_sha != saved.head_commit_sha
    {
        return Err(supervisor::SupervisorError::Conflict.into());
    }
    verify_completion_claim(projects, &selection)?;
    let health = prs.health(&saved.repository, saved.number)?;
    if health.id != observed.id || health.head != observed.head_commit_sha {
        return Err(supervisor::SupervisorError::Conflict.into());
    }
    journal::record_evidence(
        store,
        task,
        Some(&saved.attempt_id),
        "pr_health",
        &serde_json::to_string(&health).map_err(StateError::from)?,
    )?;
    if limit_reached(store, task, &health)? {
        return Ok(None);
    }
    let excerpts = match failed_checks::inspect(store, task, &saved, &health, prs)? {
        FailedChecks::Waiting => return Ok(None),
        FailedChecks::Ready(excerpts) => excerpts,
    };
    if !health.needs_repair() {
        return Ok(None);
    }
    match scheduling::ensure_dispatch_capacity(store) {
        Ok(()) => {}
        Err(StateError::Capacity { .. }) => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    launch_repair(
        store,
        task,
        projects,
        launcher,
        ids,
        (observed, health, excerpts),
    )
    .map(Some)
}

fn launch_repair<P: ProjectReader, L: SupervisorLauncher, I: IdCreator>(
    store: &mut StateStore,
    task: &str,
    projects: &mut P,
    launcher: &mut L,
    ids: &mut I,
    evidence: (
        crate::model::VerifiedOpenPr,
        crate::github::pull_request::PrHealth,
        Vec<String>,
    ),
) -> Result<supervisor::LaunchPlan, DispatchError> {
    let (observed, health, excerpts) = evidence;
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    let (previous, _) = pr_repair::context(store, task)?;
    let owner = WorktreeOwner::acquire_existing(store.root(), task)
        .map_err(|_| supervisor::SupervisorError::Conflict)?;
    owner
        .verify_protocol(&store.connection, store.root(), task, &previous.attempt_id)
        .map_err(|_| supervisor::SupervisorError::Conflict)?;
    let mut terminal = None;
    if !matches!(
        supervisor::recheck_retry_exit(
            store,
            task,
            &previous.attempt_id,
            &owner,
            false,
            &mut terminal
        )?,
        supervisor::Reconciliation::Completed { .. }
    ) {
        return Err(supervisor::SupervisorError::Conflict.into());
    }
    let (item, issue) = claim::fresh(projects, &selection.candidate)?;
    if issue.assignees != [selection.effective_config.assignment_login.as_str()] {
        return Err(DispatchError::ChangedClaim);
    }
    let source = crate::model::RetrySourceEvidence {
        item,
        issue,
        claim: task_records::source_claim_intent(store, task)?
            .ok_or(DispatchError::ChangedClaim)?,
        observed_at_unix_secs: observation_time()?,
    };
    let attempt = format!(
        "attempt-{}",
        ids.create().map_err(supervisor::SupervisorError::from)?
    );
    let plan = supervisor::prepare_pr_repair(store, &attempt, observed, health, source, excerpts)?;
    launcher.launch(store, &plan, &owner)?;
    Ok(plan)
}

fn limit_reached(
    store: &mut StateStore,
    task: &str,
    health: &crate::github::pull_request::PrHealth,
) -> Result<bool, DispatchError> {
    if health.needs_repair() && pr_repair::count(&store.connection, task)? >= MAX_PR_REPAIRS {
        task_records::hold_task(
            store,
            task,
            &format!(
                "PR repair attempt limit reached; requires a person; failing checks: {:?}",
                health.failed_required_checks
            ),
        )?;
        return Ok(true);
    }
    Ok(false)
}
