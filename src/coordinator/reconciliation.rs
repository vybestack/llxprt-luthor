use super::{
    active::monitor_running_attempt,
    ports::{
        PrVerificationStage, completion_claim_failure_reason, observation_time,
        verify_completion_claim, verify_open_pr,
    },
};
use crate::state::{exit_observation, journal, pr_completion, task_records};
use crate::{
    github::{
        project::ProjectReader,
        pull_request::{LookupError, LookupResult, PullRequestReader, lookup},
    },
    state::{ExitPrEvidence, PausePrEvidence, PausePrStatus, StateError, StateStore},
    supervisor::{self, SupervisorError},
};

/// Process and receipt proof precede the PR read. Only exhaustive absence
/// releases a stopped task for resume or a natural exit for attention.
pub fn reconcile_with_pr<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let result = supervisor::reconcile_attempt(store, task_id, attempt_id)?;
    if matches!(result, supervisor::Reconciliation::Running) {
        return monitor_running_attempt(store, task_id, attempt_id, projects, prs);
    }
    if !matches!(result, supervisor::Reconciliation::Completed { .. }) {
        return Ok(result);
    }
    if exit_observation::stopped_exit_for_pause(store, task_id, attempt_id)? {
        return finish_verified_stopped_attempt_with_pr(
            store, task_id, attempt_id, projects, prs, result,
        );
    }
    if exit_observation::natural_exit_for_attention(store, task_id, attempt_id)? {
        return finish_verified_natural_exit_with_pr(
            store, task_id, attempt_id, projects, prs, result,
        );
    }
    Ok(result)
}

fn finish_verified_stopped_attempt_with_pr<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    projects: &mut P,
    prs: &mut Q,
    result: supervisor::Reconciliation,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let selection =
        task_records::selection_evidence(store, task_id)?.ok_or(StateError::InvalidSelection)?;
    let repository = &selection.candidate.mapping.code_repository;
    let lookup_result = lookup(prs, repository, &selection.candidate.issue_url);
    let status = lookup_status(&lookup_result);
    let observed_at_unix_secs = observation_time()?;
    let proof = PausePrEvidence {
        observed_at_unix_secs,
        repository: repository.clone(),
        status,
    };
    exit_observation::record_pause_pr_lookup(store, task_id, attempt_id, &proof)?;
    match lookup_result {
        Ok(LookupResult::Absent) => Ok(result),
        Ok(LookupResult::OpenPreexisting(pr)) => verify_and_record_open_pr(
            store,
            task_id,
            attempt_id,
            projects,
            prs,
            (*pr, observed_at_unix_secs),
            result,
        ),
        Ok(LookupResult::Ambiguous(_)) => Ok(supervisor::Reconciliation::Held {
            reason: "pause PR ambiguous".into(),
        }),
        Err(_) => Ok(supervisor::Reconciliation::Held {
            reason: "pause PR read failed".into(),
        }),
    }
}

fn finish_verified_natural_exit_with_pr<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    projects: &mut P,
    prs: &mut Q,
    result: supervisor::Reconciliation,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let selection =
        task_records::selection_evidence(store, task_id)?.ok_or(StateError::InvalidSelection)?;
    let repository = &selection.candidate.mapping.code_repository;
    let lookup_result = lookup(prs, repository, &selection.candidate.issue_url);
    let observed_at_unix_secs = observation_time()?;
    let proof = ExitPrEvidence {
        observed_at_unix_secs,
        repository: repository.clone(),
        status: lookup_status(&lookup_result),
    };
    exit_observation::record_exit_pr_lookup(store, task_id, attempt_id, &proof)?;
    match lookup_result {
        Ok(LookupResult::Absent) => Ok(result),
        Ok(LookupResult::OpenPreexisting(pr)) => {
            let verified = verify_open_pr(
                store,
                task_id,
                attempt_id,
                prs,
                (*pr, observed_at_unix_secs),
                PrVerificationStage::Exit,
            );
            match verified {
                Ok(verified) => {
                    if let Err(error) = verify_completion_claim(projects, &selection) {
                        let reason = completion_claim_failure_reason(&error);
                        journal::record_evidence(
                            store,
                            task_id,
                            Some(attempt_id),
                            "held_reason",
                            reason,
                        )?;
                        return Ok(supervisor::Reconciliation::Held {
                            reason: reason.into(),
                        });
                    }
                    pr_completion::record_verified_open_pr(store, task_id, attempt_id, &verified)?;
                    Ok(result)
                }
                Err(reason) => {
                    journal::record_evidence(
                        store,
                        task_id,
                        Some(attempt_id),
                        "held_reason",
                        reason,
                    )?;
                    Ok(supervisor::Reconciliation::Held {
                        reason: reason.into(),
                    })
                }
            }
        }
        Ok(LookupResult::Ambiguous(_)) => Ok(supervisor::Reconciliation::Held {
            reason: "exit PR ambiguous".into(),
        }),
        Err(_) => Ok(supervisor::Reconciliation::Held {
            reason: "exit PR read failed".into(),
        }),
    }
}

fn lookup_status(result: &Result<LookupResult, LookupError>) -> PausePrStatus {
    match result {
        Ok(LookupResult::Absent) => PausePrStatus::Absent,
        Ok(LookupResult::OpenPreexisting(_)) => PausePrStatus::Open,
        Ok(LookupResult::Ambiguous(_)) => PausePrStatus::Ambiguous,
        Err(error) => PausePrStatus::Error {
            category: error.category,
            code: error.code.to_owned(),
            http_status: error.status,
        },
    }
}

fn verify_and_record_open_pr<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    projects: &mut P,
    prs: &mut Q,
    evidence: (crate::github::pull_request::PullRequestEvidence, u64),
    result: supervisor::Reconciliation,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let verified = verify_open_pr(
        store,
        task_id,
        attempt_id,
        prs,
        evidence,
        PrVerificationStage::Exit,
    );
    match verified {
        Ok(verified) => {
            let selection = task_records::selection_evidence(store, task_id)?
                .ok_or(StateError::InvalidSelection)?;
            if let Err(error) = verify_completion_claim(projects, &selection) {
                let reason = completion_claim_failure_reason(&error);
                journal::record_evidence(store, task_id, Some(attempt_id), "held_reason", reason)?;
                return Ok(supervisor::Reconciliation::Held {
                    reason: reason.into(),
                });
            }
            pr_completion::record_verified_open_pr(store, task_id, attempt_id, &verified)?;
            Ok(result)
        }
        Err(reason) => {
            journal::record_evidence(store, task_id, Some(attempt_id), "held_reason", reason)?;
            Ok(supervisor::Reconciliation::Held {
                reason: reason.to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests;
