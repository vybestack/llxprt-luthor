//! Explicit continuation of amended history; ordinary resume/retry stay blocked.
use super::{
    continuation_checks,
    ports::{DispatchError, SupervisorLauncher},
};
use crate::{
    claim,
    config::Config,
    github::{project::ProjectReader, pull_request::PullRequestReader},
    model::{LaunchPlan, RetrySourceEvidence, StateError},
    native_transition::{Context, Mode, Transition},
    ownership::{WorktreeOwner, WorktreeOwnerInternal},
    state::{StateStore, native_continuation as state, scheduling},
    supervisor::{self, Reconciliation, SupervisorError},
    worktree,
};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct NativeContinuationDependencies<'a, P, Q, L> {
    pub task_id: &'a str,
    pub previous_attempt_id: &'a str,
    pub attempt_id: &'a str,
    pub mode: Mode,
    pub config: &'a Config,
    pub config_revision: &'a str,
    pub actor: &'a str,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub launcher: &'a mut L,
}

pub fn continue_amended<P: ProjectReader, Q: PullRequestReader, L: SupervisorLauncher>(
    store: &mut StateStore,
    mut d: NativeContinuationDependencies<'_, P, Q, L>,
) -> Result<LaunchPlan, DispatchError> {
    let context = state::context(
        &store.connection,
        store.root(),
        d.task_id,
        d.previous_attempt_id,
    )?;
    d.config.validate().map_err(SupervisorError::from)?;
    let mut selection = context.initial.selection().clone();
    selection.effective_config = d.config.into();
    selection.config_revision = d.config_revision.to_owned();
    let previous = &context.predecessor.effective_plan;
    let plan = crate::continuation_plan::render(
        &selection,
        context.initial.effective_plan(),
        previous,
        d.attempt_id,
        d.mode.stopped(),
    )?;
    let owner = WorktreeOwner::acquire_existing(store.root(), d.task_id)
        .map_err(|_| SupervisorError::Conflict)?;
    let (pr, source) = inspect(store, &context, &plan, &owner, &mut d)?;
    let transition = Transition {
        mode: d.mode,
        actor: d.actor.to_owned(),
        predecessor: context.predecessor.clone(),
        config: d.config.into(),
        plan: plan.clone(),
        pr,
        source,
        observed_at_unix_secs: timestamp()?,
    };
    state::reserve(store, &context, &transition)?;
    // Re-read external evidence after committing authority, before the sole
    // dispatch. A failure retains the reservation, never retries the spawn.
    inspect(store, &context, &plan, &owner, &mut d)?;
    d.launcher.launch(store, &plan, &owner)?;
    Ok(plan)
}

fn timestamp() -> Result<u64, SupervisorError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|t| t.as_secs())
        .map_err(|_| SupervisorError::IdentityUnavailable)
}

fn inspect<P: ProjectReader, Q: PullRequestReader, L>(
    store: &mut StateStore,
    context: &Context,
    plan: &LaunchPlan,
    owner: &WorktreeOwner,
    d: &mut NativeContinuationDependencies<'_, P, Q, L>,
) -> Result<(crate::model::ExitPrEvidence, RetrySourceEvidence), DispatchError> {
    let previous = &context.predecessor.effective_plan;
    if d.actor
        != context
            .initial
            .selection()
            .candidate
            .mapping
            .allowed_pr_author
        || d.config.state_root != store.root()
        || d.config_revision.trim().is_empty()
        || d.config_revision == context.initial.plan().config_revision
        || !previous.session_environment.matches_current()?
    {
        return Err(SupervisorError::Conflict.into());
    }
    crate::state::validate_native_continuation_config(
        &context.initial,
        d.config,
        d.config_revision,
    )?;
    owner
        .verify_protocol(
            &store.connection,
            store.root(),
            d.task_id,
            d.previous_attempt_id,
        )
        .map_err(|_| SupervisorError::Conflict)?;
    match supervisor::recheck_retry_exit(
        store,
        d.task_id,
        d.previous_attempt_id,
        owner,
        false,
        &mut None,
    )? {
        Reconciliation::Completed { .. } => {}
        _ => return Err(SupervisorError::Conflict.into()),
    }
    let identity = worktree::verify_existing_worktree(store, d.task_id)?;
    if !worktree::matches_snapshot(&previous.expected_worktree, &identity)?
        || supervisor::inspect_never_dispatched_storage(store.root(), d.attempt_id)?
    {
        return Err(SupervisorError::Conflict.into());
    }
    plan.worker_build
        .as_ref()
        .ok_or(StateError::LaunchBlocked)?
        .verify()
        .map_err(|_| SupervisorError::Conflict)?;
    crate::disk::inspect(&d.config.worktree_root, d.config.min_free_space_bytes)?.enforce()?;
    remote_inspection(store, context, d)
}
fn remote_inspection<P: ProjectReader, Q: PullRequestReader, L>(
    store: &mut StateStore,
    context: &Context,
    d: &mut NativeContinuationDependencies<'_, P, Q, L>,
) -> Result<(crate::model::ExitPrEvidence, RetrySourceEvidence), DispatchError> {
    let selection = context.initial.selection();
    let claim = continuation_checks::verified_retry_claim(store, d.task_id, d.config, selection)?;
    let (item, issue) = claim::fresh(d.projects, &selection.candidate)?;
    if issue.assignees != [d.config.assignment_login.as_str()] {
        return Err(DispatchError::ChangedClaim);
    }
    let pr = continuation_checks::retry_pr_absence(d.prs, selection, d.actor)?;
    if !crate::state::task_records::has_attempt(store, d.task_id, d.attempt_id)? {
        scheduling::ensure_dispatch_capacity(store)?;
    }
    Ok((
        pr,
        RetrySourceEvidence {
            item,
            issue,
            claim,
            observed_at_unix_secs: timestamp()?,
        },
    ))
}
