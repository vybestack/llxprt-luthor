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
            })?;
        let root = &context.selection().effective_config.state_root;
        let db = rusqlite::Connection::open_with_flags(
            root.join("state.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| ProcessInspectionError::Unavailable)?;
        crate::state::preparation::verify_owner(
            &db,
            root,
            context.task_id(),
            context.attempt_id(),
            owner,
        )
        .map_err(|error| match error {
            crate::state::StateError::Sql(_) => ProcessInspectionError::Unavailable,
            _ => ProcessInspectionError::Conflict,
        })
    }
}
