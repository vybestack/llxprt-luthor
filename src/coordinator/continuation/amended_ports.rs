use super::ports::ContinuationDependencies;
use crate::{
    config::Config,
    state::{NeverDispatchedContext, StateStore},
    supervisor::SupervisorError,
};

pub type AmendedContinuationDependencies<'a, P, Q, L, I, O> =
    ContinuationDependencies<'a, P, Q, L, I, O>;

/// Only the amended supervisor may consume this context. Implementations must
/// commit begin_amended_supervision before launch and use its exact effective plan.
pub trait AmendedSupervisorLauncher {
    fn launch_amended(
        &mut self,
        store: &mut StateStore,
        context: &NeverDispatchedContext,
        config: &Config,
        revision: &str,
        owner: &crate::ownership::WorktreeOwner,
    ) -> Result<(), SupervisorError>;
}

pub struct NativeAmendedSupervisorLauncher;

impl AmendedSupervisorLauncher for NativeAmendedSupervisorLauncher {
    #[cfg(unix)]
    fn launch_amended(
        &mut self,
        store: &mut StateStore,
        context: &NeverDispatchedContext,
        config: &Config,
        revision: &str,
        owner: &crate::ownership::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        crate::supervisor::execute_amended(store, context, config, revision, owner)
    }

    #[cfg(not(unix))]
    fn launch_amended(
        &mut self,
        _store: &mut StateStore,
        _context: &NeverDispatchedContext,
        _config: &Config,
        _revision: &str,
        _owner: &crate::ownership::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        Err(SupervisorError::ExecutionUnavailable)
    }
}
