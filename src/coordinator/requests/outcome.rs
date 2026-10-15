use crate::{
    coordinator::{DispatchError, operator_error},
    state::StateError,
    supervisor::SupervisorError,
};

/// Storage/integrity errors abort the cycle instead of masquerading as an
/// operator refusal. Transient read-only failures retain the durable decision.
pub(crate) fn failure(error: DispatchError) -> Result<(&'static str, String), StateError> {
    match error {
        DispatchError::State(
            error @ (StateError::Sql(_)
            | StateError::Io(_)
            | StateError::Serialization(_)
            | StateError::UnsupportedDatabaseVersion(_)),
        )
        | DispatchError::Supervisor(SupervisorError::State(
            error @ (StateError::Sql(_)
            | StateError::Io(_)
            | StateError::Serialization(_)
            | StateError::UnsupportedDatabaseVersion(_)),
        )) => Err(error),
        DispatchError::State(StateError::Capacity { reserved, capacity }) => Ok((
            "pending",
            format!("capacity full: limit {capacity}, reserved or unresolved {reserved}"),
        )),
        error if crate::coordinator::ports::transient_read(&error) => Ok((
            "pending",
            "claim or pull request read temporarily unavailable".into(),
        )),
        error => Ok(("refused", operator_error(&error))),
    }
}
