use super::{
    active::monitor_running_attempt,
    ports::{
        PrVerificationStage, completion_claim_failure_reason, observation_time,
        verify_completion_claim, verify_finishing_pr, verify_open_pr,
    },
    retirement::{Settlement, retire_if_finished},
};
use crate::state::{exit_observation, journal, pr_completion, retirement, task_records};
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
    // A retired task is finished; its attempts are not reconciled again.
    if let Some(phase) = task_records::task_phase(store, task_id)?
        && retirement::RetiredPhase::parse(&phase).is_some()
    {
        return Ok(supervisor::Reconciliation::Held {
            reason: format!("task is retired ({phase})"),
        });
    }
    let mut result = supervisor::reconcile_attempt(store, task_id, attempt_id)?;
    if matches!(result, supervisor::Reconciliation::Running) {
        result = monitor_running_attempt(store, task_id, attempt_id, projects, prs)?;
    }
    if !matches!(result, supervisor::Reconciliation::Completed { .. }) {
        return Ok(result);
    }
    // A completed PR proof is already committed atomically with the lookup.
    // Reconciliation rechecks process accounting, not a second completion write.
    if task_records::task_phase(store, task_id)?.as_deref() == Some("pr_complete") {
        return Ok(result);
    }
    if journal::evidence_payload(store, task_id, Some(attempt_id), "pr_repair_blocked")?.is_some() {
        // A repair held for a person may have been overtaken: the PR merged or
        // the issue closed while it waited.
        if let Settlement::Retired(_) = retire_if_finished(store, task_id, projects, prs)? {
            return Ok(result);
        }
        return Ok(supervisor::Reconciliation::Held {
            reason: "PR repair requires a person".into(),
        });
    }
    if let Some(held) = inspect_repair_result(store, task_id, attempt_id)? {
        return Ok(held);
    }
    if crate::state::pr_repair::authorization(&store.connection, task_id, attempt_id)?.is_some() {
        return finish_pr_repair(store, task_id, attempt_id, projects, prs, result);
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

fn inspect_repair_result(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<Option<supervisor::Reconciliation>, SupervisorError> {
    match supervisor::repair_result(store, task_id, attempt_id) {
        Ok(Some(crate::pr_repair::PrRepairStatus::SemanticConflict)) => {
            return Ok(Some(supervisor::Reconciliation::Held {
                reason: "semantic PR conflict requires a person".into(),
            }));
        }
        Ok(_) => {}
        Err(error) => {
            // A typed error envelope has no repair result. The execute repair
            // pass applies current policy, rechecks proofs and schedules it.
            if super::repair_retry::defer_typed_failure(store, task_id, attempt_id)? {
                return Ok(Some(supervisor::Reconciliation::Held {
                    reason: "typed PR repair failure awaits retry policy".into(),
                }));
            }
            journal::record_evidence(
                store,
                task_id,
                Some(attempt_id),
                "pr_repair_blocked",
                "PR repair result unavailable or invalid",
            )?;
            task_records::hold_task(store, task_id, "PR repair result unavailable or invalid")?;
            return Err(error);
        }
    }
    Ok(None)
}

fn finish_pr_repair<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task: &str,
    attempt: &str,
    projects: &mut P,
    prs: &mut Q,
    result: supervisor::Reconciliation,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    let found = lookup(
        prs,
        &selection.candidate.mapping.code_repository,
        &selection.candidate.issue_url,
    );
    let proof = ExitPrEvidence {
        observed_at_unix_secs: observation_time()?,
        repository: selection.candidate.mapping.code_repository.clone(),
        status: lookup_status(&found),
    };
    // Never use ordinary absent-PR phase transitions for an existing-PR repair.
    journal::record_evidence(
        store,
        task,
        Some(attempt),
        "pr_repair_pr_lookup",
        &serde_json::to_string(&proof)?,
    )?;
    let outcome = match found {
        Ok(LookupResult::OpenPreexisting(pr)) => verify_and_record_open_pr(
            store,
            task,
            attempt,
            projects,
            prs,
            (*pr, proof.observed_at_unix_secs),
            result.clone(),
        ),
        _ => Ok(supervisor::Reconciliation::Held {
            reason: "repair PR missing, ambiguous or unreadable".into(),
        }),
    };
    match outcome {
        Ok(done @ supervisor::Reconciliation::Completed { .. }) => Ok(done),
        other => {
            // The saved PR is read by its identity and the issue directly: a
            // merged PR or closed issue explains the failure, so the task is
            // finished rather than held for a person.
            if let Settlement::Retired(_) = retire_if_finished(store, task, projects, prs)? {
                return Ok(result);
            }
            journal::record_evidence(
                store,
                task,
                Some(attempt),
                "pr_repair_blocked",
                "post-repair PR or claim verification failed",
            )?;
            task_records::hold_task(
                store,
                task,
                "post-repair PR or claim verification failed; requires a person",
            )?;
            other
        }
    }
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
    // Repair completion already verifies its saved PR identity and exact local
    // tip in the state commit. Retain that fail-closed error path.
    let verified = if crate::state::pr_repair::authorization(
        &store.connection,
        task_id,
        attempt_id,
    )?
    .is_some()
    {
        verify_open_pr(
            store,
            task_id,
            attempt_id,
            prs,
            evidence,
            PrVerificationStage::Exit,
        )
    } else {
        verify_finishing_pr(store, task_id, attempt_id, prs, evidence)
    };
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
