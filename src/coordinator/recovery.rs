use super::{
    ports::{
        PrVerificationStage, completion_claim_failure_reason, observation_time,
        verify_completion_claim, verify_open_pr,
    },
    recovery_commit::{self, RecoveryPrEvidence, RecoveryResult},
};
use crate::state::{journal, task_records};
use crate::{
    github::{
        project::ProjectReader,
        pull_request::{LookupResult, PullRequestReader, lookup},
    },
    state::StateStore,
    supervisor::{self, SupervisorError},
    worktree,
};

enum RecoveryLookup {
    Verified(RecoveryPrEvidence),
    Held(&'static str),
}

pub fn operator_recover_missing_receipt<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    actor: &str,
    reason: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Result<RecoveryResult, SupervisorError> {
    let held = |reason: &str| RecoveryResult::Held(reason.to_owned());
    if actor.trim().is_empty() {
        return Ok(held("operator actor is blank"));
    }
    if reason.trim().is_empty() {
        return Ok(held("operator recovery reason is blank"));
    }
    let owner = match crate::ownership::WorktreeOwner::acquire_existing(store.root(), task_id) {
        Ok(owner) => owner,
        Err(crate::ownership::OwnershipError::Busy) => return Ok(held("worktree owner is live")),
        Err(crate::ownership::OwnershipError::Unavailable) => {
            return Ok(held("worktree ownership cannot be proved"));
        }
    };
    if !recovery_quiescent(store, task_id, attempt_id, &owner)? {
        return Ok(held("worker is not proven quiescent"));
    }
    let Some(selection) = task_records::selection_evidence(store, task_id)? else {
        return Ok(held("selection evidence is missing"));
    };
    if let Err(error) = verify_completion_claim(projects, &selection) {
        return Ok(held(completion_claim_failure_reason(&error)));
    }
    let repository = &selection.candidate.mapping.code_repository;
    let pr = match recovery_pr_lookup(
        store,
        task_id,
        attempt_id,
        repository,
        &selection.candidate.issue_url,
        prs,
    )? {
        RecoveryLookup::Verified(pr) => pr,
        RecoveryLookup::Held(reason) => return Ok(held(reason)),
    };
    if let Some(reason) = worktree_failure(store, task_id)? {
        return Ok(held(reason));
    }
    if let Err(error) = verify_completion_claim(projects, &selection) {
        return Ok(held(completion_claim_failure_reason(&error)));
    }
    if !recovery_quiescent(store, task_id, attempt_id, &owner)? {
        return Ok(held("worker quiescence changed during recovery"));
    }
    recovery_commit::audited_commit(store, task_id, attempt_id, actor, reason, repository, pr)
}

fn recovery_quiescent(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    owner: &crate::ownership::WorktreeOwner,
) -> Result<bool, SupervisorError> {
    Ok(matches!(
        supervisor::inspect_recovery_quiescence_with_owner(store, task_id, attempt_id, owner)?,
        supervisor::RecoveryInspection::Quiescent
    ))
}

fn worktree_failure(
    store: &StateStore,
    task_id: &str,
) -> Result<Option<&'static str>, SupervisorError> {
    let Some(payload) = journal::evidence_payload(store, task_id, None, "worktree_created")? else {
        return Ok(Some("worktree evidence is missing"));
    };
    if worktree::verify_snapshot(&serde_json::from_str(&payload)?).is_err() {
        return Ok(Some("worktree snapshot changed"));
    }
    Ok(None)
}

fn recovery_pr_lookup<Q: PullRequestReader>(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    repository: &str,
    issue_url: &str,
    prs: &mut Q,
) -> Result<RecoveryLookup, SupervisorError> {
    let result = match lookup(prs, repository, issue_url) {
        Ok(LookupResult::Absent) => RecoveryLookup::Verified(RecoveryPrEvidence::Absent),
        Ok(LookupResult::OpenPreexisting(pr)) => {
            let observed_at_unix_secs = observation_time()?;
            if observed_at_unix_secs == 0 {
                return Ok(RecoveryLookup::Held("invalid recovery timestamp"));
            }
            match verify_open_pr(
                store,
                task_id,
                attempt_id,
                prs,
                (*pr, observed_at_unix_secs),
                PrVerificationStage::Recovery,
            ) {
                Ok(proof) => RecoveryLookup::Verified(RecoveryPrEvidence::Open(Box::new(proof))),
                Err(reason) => RecoveryLookup::Held(reason),
            }
        }
        Ok(LookupResult::Ambiguous(_)) => RecoveryLookup::Held("pull request lookup is ambiguous"),
        Err(_) => RecoveryLookup::Held("pull request lookup failed"),
    };
    Ok(result)
}
