use super::ports::{ContinuationProcessInspector, ProcessInspectionError};
use crate::{
    ownership::WorktreeOwnerInternal,
    ownership::{OwnershipError, WorktreeOwner},
    state::NeverDispatchedContext,
};

pub struct OsContinuationProcessInspector;

impl ContinuationProcessInspector for OsContinuationProcessInspector {
    fn inspect(
        &mut self,
        context: &NeverDispatchedContext,
        owner: &WorktreeOwner,
    ) -> Result<(), ProcessInspectionError> {
        owner
            .verify_binding(
                &context.selection().effective_config.state_root,
                context.task_id(),
            )
            .map_err(|error| match error {
                OwnershipError::Busy => ProcessInspectionError::Conflict,
                OwnershipError::Unavailable => ProcessInspectionError::Unavailable,
            })
    }
}
