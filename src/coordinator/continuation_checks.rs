use super::ports::DispatchError;
use crate::{
    config::Config,
    github::pull_request::{LookupResult, PullRequestReader, lookup},
    state::{ExitPrEvidence, PausePrStatus, StateStore, task_records},
    supervisor::SupervisorError,
};
use std::time::{SystemTime, UNIX_EPOCH};
pub(crate) fn verified_retry_claim(
    store: &StateStore,
    task_id: &str,
    config: &Config,
    selection: &crate::model::SelectionEvidence,
) -> Result<String, DispatchError> {
    let claim =
        task_records::source_claim_intent(store, task_id)?.ok_or(DispatchError::ChangedClaim)?;
    let expected = serde_json::json!({"principal": config.assignment_login,
        "repository": selection.candidate.repository, "number": selection.candidate.issue_number});
    if serde_json::from_str::<serde_json::Value>(&claim).ok() != Some(expected) {
        return Err(DispatchError::ChangedClaim);
    }
    Ok(claim)
}

pub(crate) fn retry_pr_absence(
    prs: &mut impl PullRequestReader,
    selection: &crate::model::SelectionEvidence,
    actor: &str,
) -> Result<ExitPrEvidence, DispatchError> {
    if prs.authenticated_identity()? != actor {
        return Err(DispatchError::ChangedClaim);
    }
    let result = lookup(
        prs,
        &selection.candidate.mapping.code_repository,
        &selection.candidate.issue_url,
    );
    let status = match &result {
        Ok(LookupResult::Absent) => PausePrStatus::Absent,
        Ok(LookupResult::OpenPreexisting(_)) => PausePrStatus::Open,
        Ok(LookupResult::Ambiguous(_)) => PausePrStatus::Ambiguous,
        Err(error) => PausePrStatus::Error {
            category: error.category,
            code: error.code.into(),
            http_status: error.status,
        },
    };
    let proof = ExitPrEvidence {
        observed_at_unix_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SupervisorError::IdentityUnavailable)?
            .as_secs(),
        repository: selection.candidate.mapping.code_repository.clone(),
        status,
    };
    if result? != LookupResult::Absent {
        return Err(DispatchError::ExistingPr);
    }
    Ok(proof)
}
