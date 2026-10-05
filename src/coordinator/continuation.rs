use super::SupervisorLauncher;
use crate::{
    config::Config,
    github::{project::ProjectReader, pull_request::PullRequestReader},
    state::{
        EffectiveConfigSnapshot, NeverDispatchedContext, NeverDispatchedReason, StateError,
        StateStore,
    },
    supervisor,
};
use checks::{refuse, verify_local, verify_pr, verify_source};

mod amended;
mod amended_ports;
mod checks;
pub use amended::amend_never_dispatched_initial_branch;
pub use amended_ports::{
    AmendedContinuationDependencies, AmendedSupervisorLauncher, NativeAmendedSupervisorLauncher,
};
mod local;
mod ports;
mod processes;
pub use local::OsContinuationLocalInspector;
pub use ports::{
    ContinuationDependencies, ContinuationLocalInspector, ContinuationProcessInspector,
    ContinuationRefusal, ContinuationResult, ProcessInspectionError,
};
pub use processes::OsContinuationProcessInspector;

/// Authorize one existing reserved launch, then pass its exact saved plan to the
/// ordinary supervisor launcher. Intact SQLite history establishes no Luthor
/// dispatch; OS inspection only addresses current out-of-band conflicts.
pub fn continue_never_dispatched<P, Q, L, I, O>(
    store: &mut StateStore,
    dependencies: ContinuationDependencies<'_, P, Q, L, I, O>,
) -> Result<ContinuationResult, StateError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    L: SupervisorLauncher,
    I: ContinuationLocalInspector,
    O: ContinuationProcessInspector,
{
    let context =
        match store.never_dispatched_context(dependencies.task_id, dependencies.attempt_id) {
            Ok(context) => context,
            Err(StateError::LaunchBlocked | StateError::Serialization(_)) => {
                // Do not annotate a task whose identity/state has not been established.
                return Ok(ContinuationResult::Held(ContinuationRefusal::Ineligible));
            }
            Err(error) => return Err(error),
        };
    let verified = verify_config(
        store,
        &context,
        dependencies.config,
        dependencies.config_revision,
    )
    .and_then(|()| verify_local(&context, dependencies.local, dependencies.processes))
    .and_then(|()| verify_source(&context, dependencies.projects))
    .and_then(|()| verify_pr(&context, dependencies.actor, dependencies.prs))
    .and_then(|()| verify_source(&context, dependencies.projects))
    .and_then(|()| verify_local(&context, dependencies.local, dependencies.processes));
    if let Err(reason) = verified {
        return refuse(store, &context, reason);
    }
    if context
        .authorize(
            store,
            dependencies.actor,
            NeverDispatchedReason::LegacyPreflightRecovery,
        )
        .is_err()
    {
        return refuse(store, &context, ContinuationRefusal::AuthorizationFailed);
    }
    if dependencies.launcher.launch(store, context.plan()).is_err() {
        return refuse(store, &context, ContinuationRefusal::LaunchFailed);
    }
    Ok(ContinuationResult::Dispatched(Box::new(
        context.plan().clone(),
    )))
}

fn verify_config(
    store: &StateStore,
    context: &NeverDispatchedContext,
    config: &Config,
    revision: &str,
) -> Result<(), ContinuationRefusal> {
    if EffectiveConfigSnapshot::from(config) != context.selection().effective_config
        || config.state_root != store.root()
        || revision != context.plan().config_revision
        || revision.trim().is_empty()
    {
        return Err(ContinuationRefusal::ConfigChanged);
    }
    // Legacy templates may be rejected by current config policy. Never alter
    // them to fit that policy or reconstruct a different launch command.
    supervisor::validate_saved_initial_plan(context).map_err(|_| ContinuationRefusal::PlanInvalid)
}
