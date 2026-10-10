use super::ports::{
    PrVerificationStage, observation_time, verify_completion_claim, verify_open_pr,
};
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
    Matching,
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
    let claim = match observe_claim(projects, &selection) {
        Ok(claim) => claim,
        Err(reason) => return record_hold(store, task_id, attempt_id, reason),
    };
    let pr = observe_pr(store, task_id, attempt_id, &selection, prs)?;
    let reason = match (claim, pr) {
        (_, ActivePr::Held(reason)) => return record_hold(store, task_id, attempt_id, reason),
        (ActiveClaim::Lost, ActivePr::Matching) => "active claim lost and matching PR observed",
        (ActiveClaim::Lost, ActivePr::Absent) => "active claim lost",
        (ActiveClaim::Owned, ActivePr::Matching) => "matching PR observed while worker active",
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
        Ok(LookupResult::Absent) => Ok(ActivePr::Absent),
        Ok(LookupResult::OpenPreexisting(pr)) => {
            let observed = observation_time()?;
            let verified = verify_open_pr(
                store,
                task_id,
                attempt_id,
                prs,
                (*pr, observed),
                PrVerificationStage::Exit,
            );
            Ok(match verified {
                Ok(_) => ActivePr::Matching,
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
