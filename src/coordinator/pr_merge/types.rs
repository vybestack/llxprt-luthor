use crate::{
    github::pull_request::{LookupError, PrMerger},
    state::{StateError, StateStore, pr_merge},
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
    #[serde(skip_serializing)]
    pub(crate) cleanup: crate::cleanup::CleanupReport,
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

/// What the sweep concluded about one task.
pub(crate) enum Outcome {
    /// Luthor's own merge is confirmed and the tracker handled.
    Merged(crate::cleanup::CleanupReport),
    /// Nothing is written; the reason is reported and the task stays as it is.
    Waiting(String),
    /// GitHub merged the PR outside Luthor and the delivery is verified.
    Delivered(
        Box<crate::state::retirement::Retirement>,
        crate::cleanup::CleanupReport,
    ),
}

/// A failure of one task's merge step is journaled and held with its reason so
/// the rest of the queue keeps running. Only a failure of local state, which
/// no task can be trusted past, stops the sweep.
pub(crate) fn isolate(
    store: &mut StateStore,
    task: &str,
    result: Result<Outcome, MergeError>,
) -> Result<Outcome, MergeError> {
    let error = match result {
        Ok(outcome) => return Ok(outcome),
        Err(error) => error,
    };
    pr_merge::record_error(store, task, &audit_error(&error).to_string())?;
    if matches!(error, MergeError::State(_)) {
        return Err(error);
    }
    Ok(Outcome::Waiting(format!("merge step failed: {error}")))
}
