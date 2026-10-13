use super::ports::{observation_time, verify_completion_claim, verify_finishing_pr};
use crate::state::{journal, task_records};
use crate::{
    claim::ClaimError,
    github::{
        project::ProjectReader,
        pull_request::{LookupResult, PullRequestReader, lookup},
    },
    state::{SelectionEvidence, StateError, StateStore},
    supervisor::{self, SupervisorError},
};

#[derive(Clone, Copy)]
enum ActiveClaim {
    Owned,
    Lost,
}

enum ActivePr {
    Absent,
    Matching(Box<crate::model::VerifiedOpenPr>),
    Held(&'static str),
}

pub(crate) fn monitor_running_attempt<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    let selection =
        task_records::selection_evidence(store, task_id)?.ok_or(StateError::InvalidSelection)?;
    let claim = observe_claim(projects, &selection);
    let pr = if claim.is_ok() {
        Some(observe_pr(store, task_id, attempt_id, &selection, prs)?)
    } else {
        None
    };
    // GitHub reads can outlive the worker. Never apply an active-stage
    // observation to an exited attempt: account for its receipt/processes
    // first, then let reconciliation perform a fresh completion lookup.
    let result = supervisor::reconcile_attempt(store, task_id, attempt_id)?;
    if !matches!(result, supervisor::Reconciliation::Running) {
        return Ok(result);
    }
    let claim = match claim {
        Ok(claim) => claim,
        Err(reason) => return record_hold(store, task_id, attempt_id, reason),
    };
    let pr = pr.expect("owned or lost claim has a PR observation");
    let reason = match (claim, pr) {
        (_, ActivePr::Held(reason)) => return record_hold(store, task_id, attempt_id, reason),
        (ActiveClaim::Lost, ActivePr::Matching(_)) => "active claim lost and matching PR observed",
        (ActiveClaim::Lost, ActivePr::Absent) => "active claim lost",
        (ActiveClaim::Owned, ActivePr::Matching(proof)) => {
            // This is the worker's expected finish, not a competing PR. Keep
            // its reservation and let normal receipt reconciliation complete it.
            journal::record_evidence(
                store,
                task_id,
                Some(attempt_id),
                "active_open_pr",
                &serde_json::to_string(&proof)?,
            )?;
            return Ok(supervisor::Reconciliation::Running);
        }
        (ActiveClaim::Owned, ActivePr::Absent) => return Ok(supervisor::Reconciliation::Running),
    };
    journal::record_evidence(store, task_id, Some(attempt_id), "held_reason", reason)?;
    if journal::stop_intent(store, task_id, attempt_id)?.is_none()
        && supervisor::request_stop(store, task_id, attempt_id).is_err()
    {
        journal::record_evidence(
            store,
            task_id,
            Some(attempt_id),
            "held_reason",
            "active worker stop pending",
        )?;
    }
    Ok(supervisor::Reconciliation::Held {
        reason: reason.into(),
    })
}

fn observe_claim<P: ProjectReader>(
    projects: &mut P,
    selection: &SelectionEvidence,
) -> Result<ActiveClaim, &'static str> {
    match verify_completion_claim(projects, selection) {
        Ok(()) => Ok(ActiveClaim::Owned),
        Err(ClaimError::Changed) => Ok(ActiveClaim::Lost),
        Err(ClaimError::Source(_)) => Err("active claim read failed"),
        Err(_) => {
            unreachable!("completion claim verification only returns source or changed errors")
        }
    }
}

fn observe_pr<Q: PullRequestReader>(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    selection: &SelectionEvidence,
    prs: &mut Q,
) -> Result<ActivePr, SupervisorError> {
    match lookup(
        prs,
        &selection.candidate.mapping.code_repository,
        &selection.candidate.issue_url,
    ) {
        Ok(LookupResult::Absent) => {
            if crate::state::pr_repair::authorization(&store.connection, task_id, attempt_id)?
                .is_some()
            {
                Ok(ActivePr::Held("active repair PR disappeared"))
            } else {
                Ok(ActivePr::Absent)
            }
        }
        Ok(LookupResult::OpenPreexisting(pr)) => {
            let observed = observation_time()?;
            let verified = verify_finishing_pr(store, task_id, attempt_id, prs, (*pr, observed));
            Ok(match verified {
                Ok(verified) => {
                    if let Some(audit) = crate::state::pr_repair::authorization(
                        &store.connection,
                        task_id,
                        attempt_id,
                    )? && verified.id != audit.saved_pr.id
                    {
                        ActivePr::Held("active repair PR identity changed")
                    } else {
                        ActivePr::Matching(Box::new(verified))
                    }
                }
                Err(_) => ActivePr::Held("active PR verification failed"),
            })
        }
        Ok(LookupResult::Ambiguous(_)) => Ok(ActivePr::Held("active PR lookup ambiguous")),
        Err(_) => Ok(ActivePr::Held("active PR read failed")),
    }
}

fn record_hold(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    reason: &'static str,
) -> Result<supervisor::Reconciliation, SupervisorError> {
    journal::record_evidence(store, task_id, Some(attempt_id), "held_reason", reason)?;
    Ok(supervisor::Reconciliation::Held {
        reason: reason.into(),
    })
}
