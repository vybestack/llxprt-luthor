use super::{
    amended_ports::{AmendedContinuationDependencies, AmendedSupervisorLauncher},
    checks::{refuse, verify_local, verify_pr, verify_source},
    ports::{
        ContinuationLocalInspector, ContinuationProcessInspector, ContinuationRefusal as Refusal,
        ContinuationResult,
    },
};
use crate::{
    github::{project::ProjectReader, pull_request::PullRequestReader},
    state::{
        self, BranchRemovalRequest, ExitPrEvidence, NeverDispatchedContext, PausePrStatus,
        ProcessQuiescence, StateError, StateStore,
    },
};
use std::time::{SystemTime, UNIX_EPOCH};

/// Correct only native initial conversation selection on the same reserved attempt.
/// An existing audit never implicitly authorizes another launch invocation.
pub fn amend_never_dispatched_initial_branch<P, Q, L, I, O>(
    store: &mut StateStore,
    mut dependencies: AmendedContinuationDependencies<'_, P, Q, L, I, O>,
) -> Result<ContinuationResult, StateError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    L: AmendedSupervisorLauncher,
    I: ContinuationLocalInspector,
    O: ContinuationProcessInspector,
{
    let context = match read_context(store, dependencies.task_id, dependencies.attempt_id)? {
        Some(context) if context.amendment().is_none() => context,
        _ => return Ok(ContinuationResult::Held(Refusal::Ineligible)),
    };
    if state::validate_removal_config(&context, dependencies.config, dependencies.config_revision)
        .is_err()
        || dependencies.config.state_root != store.root()
    {
        return refuse(store, &context, Refusal::ConfigChanged);
    }
    let ownership =
        match crate::ownership::WorktreeOwner::acquire_existing(store.root(), context.task_id()) {
            Ok(owner) => owner,
            Err(_) => return refuse(store, &context, Refusal::LaunchFailed),
        };
    let effective = match state::initial_branch_removal_plan(&context) {
        Ok(plan) => plan,
        Err(_) => return refuse(store, &context, Refusal::PlanInvalid),
    };
    let (pr, processes) = match inspect(&context, &mut dependencies, &ownership) {
        Ok(observations) => observations,
        Err(reason) => return refuse(store, &context, reason),
    };
    let sequence = match store.authorize_initial_branch_removal(
        &context,
        &effective,
        BranchRemovalRequest {
            actor: dependencies.actor,
            config: dependencies.config,
            config_revision: dependencies.config_revision,
            pr: &pr,
            processes,
        },
    ) {
        Ok(sequence) => sequence,
        Err(_) => return refuse(store, &context, Refusal::AuthorizationFailed),
    };
    let amended = match read_context(store, context.task_id(), context.attempt_id())? {
        Some(amended) => amended,
        None => return refuse(store, &context, Refusal::AuthorizationFailed),
    };
    if let Err(reason) = inspect(&amended, &mut dependencies, &ownership) {
        return refuse(store, &context, reason);
    }
    if !dispatch_ready(store, &amended, &dependencies, sequence)? {
        return refuse(store, &context, Refusal::AuthorizationFailed);
    }
    if dependencies
        .launcher
        .launch_amended(
            store,
            &amended,
            dependencies.config,
            dependencies.config_revision,
            &ownership,
        )
        .is_err()
    {
        return refuse(store, &context, Refusal::LaunchFailed);
    }
    Ok(ContinuationResult::Dispatched(Box::new(effective)))
}

fn read_context(
    store: &StateStore,
    task: &str,
    attempt: &str,
) -> Result<Option<NeverDispatchedContext>, StateError> {
    match store.never_dispatched_context(task, attempt) {
        Ok(context) => Ok(Some(context)),
        Err(StateError::LaunchBlocked | StateError::Serialization(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

fn inspect<P, Q, L, I, O>(
    context: &NeverDispatchedContext,
    dependencies: &mut AmendedContinuationDependencies<'_, P, Q, L, I, O>,
    owner: &crate::ownership::WorktreeOwner,
) -> Result<(ExitPrEvidence, ProcessQuiescence), Refusal>
where
    P: ProjectReader,
    Q: PullRequestReader,
    I: ContinuationLocalInspector,
    O: ContinuationProcessInspector,
{
    verify_source(context, dependencies.projects)?;
    verify_pr(context, dependencies.actor, dependencies.prs)?;
    let pr = ExitPrEvidence {
        observed_at_unix_secs: timestamp()?,
        repository: context
            .selection()
            .candidate
            .mapping
            .code_repository
            .clone(),
        status: PausePrStatus::Absent,
    };
    verify_local(context, dependencies.local, dependencies.processes, owner)?;
    let processes = ProcessQuiescence::Clear {
        observed_at_unix_secs: timestamp()?,
    };
    Ok((pr, processes))
}

fn timestamp() -> Result<u64, Refusal> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| Refusal::AuthorizationFailed)
}

fn dispatch_ready<P, Q, L, I, O>(
    store: &StateStore,
    amended: &NeverDispatchedContext,
    dependencies: &AmendedContinuationDependencies<'_, P, Q, L, I, O>,
    sequence: i64,
) -> Result<bool, StateError> {
    if read_context(store, amended.task_id(), amended.attempt_id())?.as_ref() != Some(amended) {
        return Ok(false);
    }
    Ok(store
        .amended_dispatch_proof(amended, dependencies.config, dependencies.config_revision)
        .is_ok_and(|proof| {
            proof.amendment_sequence == sequence
                && proof.effective_plan == *amended.effective_plan()
        }))
}
