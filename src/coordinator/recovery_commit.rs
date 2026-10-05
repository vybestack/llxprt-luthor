use crate::{
    model::{ExitPrEvidence, StateError, VerifiedOpenPr},
    state::exits,
};
use rusqlite::Connection;

pub(crate) fn commit(
    connection: &mut Connection,
    task_id: &str,
    attempt_id: &str,
    audit: &str,
    lookup: &ExitPrEvidence,
    proof: Option<VerifiedOpenPr>,
) -> Result<RecoveryResult, StateError> {
    if let Some(proof) = proof {
        let pr_id = proof.id;
        exits::commit_telemetry_lost_pr_completion(
            connection, task_id, attempt_id, audit, lookup, &proof,
        )?;
        Ok(RecoveryResult::RecoveredPrComplete { pr_id })
    } else {
        exits::commit_telemetry_lost_recovery(connection, task_id, attempt_id, audit, lookup)?;
        Ok(RecoveryResult::RecoveredHeld)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryResult {
    RecoveredHeld,
    RecoveredPrComplete { pr_id: u64 },
    Held(String),
}
