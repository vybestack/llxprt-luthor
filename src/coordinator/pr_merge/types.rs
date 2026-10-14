use crate::{
    github::pull_request::{LookupError, PrMerger},
    state::StateError,
};
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MergeError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Upstream(#[from] LookupError),
    #[error("merge verification failed: {0}")]
    Refused(&'static str),
}

#[derive(Debug, Default, Serialize)]
pub struct MergeReport {
    pub merged: Vec<String>,
    pub waiting: Vec<crate::coordinator::notes::TaskNote>,
    /// Merged by someone else, with the delivery verified and the task retired.
    pub delivered: Vec<crate::coordinator::notes::TaskNote>,
}

pub(crate) fn now() -> Result<u64, MergeError> {
    crate::coordinator::ports::observation_time()
        .map_err(|_| MergeError::Refused("clock unavailable"))
}

pub(crate) fn verify_account<Q: PrMerger>(
    prs: &mut Q,
    saved: &crate::model::VerifiedOpenPr,
) -> Result<(), MergeError> {
    if prs.authenticated_identity()? != saved.active_login || saved.active_login != "acoliver" {
        return Err(MergeError::Refused("authenticated identity changed"));
    }
    Ok(())
}

pub(crate) fn audit_error(error: &MergeError) -> Value {
    match error {
        MergeError::State(_) => json!({"category":"state","code":"merge-state-error"}),
        MergeError::Upstream(error) => {
            json!({"category":"upstream","code":error.code,"status":error.status})
        }
        MergeError::Refused(reason) => json!({"category":"verification","reason":reason}),
    }
}
