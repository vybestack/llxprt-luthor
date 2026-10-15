//! A typed failed repair remains authorized for its verified PR, not for an
//! absent-PR retry. Reuse ordinary outcome classification and policy decisions.
use super::{
    notes::TaskNote,
    ports::{
        DispatchError, PrVerificationStage, observation_time, verify_completion_claim,
        verify_open_pr,
    },
    worker_outcome::{self, RetryDecision, WorkerOutcome},
};
use crate::{
    config::{Config, RetryPolicy},
    github::{
        project::ProjectReader,
        pull_request::{LookupResult, PullRequestReader, lookup},
    },
    pr_repair::{MAX_PR_REPAIRS, RESULT_UNAVAILABLE, RepairRetry},
    state::{StateError, StateStore, journal, pr_repair, repair_retry, task_records},
    supervisor::SupervisorError,
};

/// Reconciliation already verified the receipt. Do not demand a result file
/// from an error envelope, and do not block the entire queue with that IO error.
pub(crate) fn defer_typed_failure(
    store: &mut StateStore,
    task: &str,
    attempt: &str,
) -> Result<bool, SupervisorError> {
    let Some((plan, receipt)) = repair_retry::failure_context(store, task, attempt)? else {
        return Ok(false);
    };
    if !matches!(
        worker_outcome::read(store.root(), &plan, &receipt)?,
        WorkerOutcome::Transient { .. }
    ) {
        return Ok(false);
    }
    if journal::evidence_payload(store, task, Some(attempt), "pr_repair_blocked")?.is_none() {
        journal::record_evidence(
            store,
            task,
            Some(attempt),
            "pr_repair_blocked",
            RESULT_UNAVAILABLE,
        )?;
        task_records::hold_task(store, task, RESULT_UNAVAILABLE)?;
    }
    Ok(true)
}

/// Also visits pre-upgrade result-unavailable holds. Every proof is reread;
/// missing ownership/receipt/claim/PR evidence is retained and reported.
pub(crate) fn restore<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    config: &Config,
    projects: &mut P,
    prs: &mut Q,
) -> Result<Vec<TaskNote>, DispatchError> {
    let mut retained = Vec::new();
    if matches!(config.retry_policy, RetryPolicy::Disabled { .. }) {
        return Ok(retained);
    }
    for task in repair_retry::candidates(store)? {
        if !crate::state::pr_poll::begin(store, &task, "repair_retry_restore", "held")? {
            continue;
        }
        match restore_task(store, config, &task, projects, prs) {
            Ok(()) => {}
            Err(DispatchError::State(error)) if !matches!(error, StateError::LaunchBlocked) => {
                return Err(error.into());
            }
            Err(error) => retained.push(TaskNote {
                task_id: task,
                reason: super::repair_hold::reason(&error),
            }),
        }
    }
    Ok(retained)
}

fn restore_task<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    config: &Config,
    task: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Result<(), DispatchError> {
    let attempt = task_records::latest_attempt(store, task)?.ok_or(StateError::LaunchBlocked)?;
    let Some((plan, receipt)) = repair_retry::failure_context(store, task, &attempt)? else {
        return Ok(());
    };
    let _owner = super::pr_head::rechecked_owner(store, task, &attempt)?;
    let now = observation_time()?;
    let observation = worker_outcome::observe_outcome(store, &plan, &receipt, now)?;
    let decision = worker_outcome::decision(
        &config.retry_policy,
        &observation,
        repair_retry::attempt_count(store, task)?,
        now,
    )?;
    if !matches!(decision, None | Some(RetryDecision::Backoff)) {
        return Ok(());
    }
    if pr_repair::count(&store.connection, task)? >= MAX_PR_REPAIRS {
        return Ok(());
    }
    let observed = observe_pr(store, task, &attempt, projects, prs)?;
    let RetryPolicy::Bounded { backoff_secs, .. } = config.retry_policy else {
        return Err(StateError::InvalidConfig.into());
    };
    let retry = RepairRetry {
        task_id: task.into(),
        attempt_id: attempt,
        backoff_until_unix_secs: observation
            .observed_at_unix_secs
            .checked_add(u64::from(backoff_secs))
            .ok_or(StateError::LaunchBlocked)?,
    };
    repair_retry::schedule(store, &retry, &observed)?;
    Ok(())
}

fn observe_pr<P: ProjectReader, Q: PullRequestReader>(
    store: &StateStore,
    task: &str,
    attempt: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Result<crate::model::VerifiedOpenPr, DispatchError> {
    let saved = pr_repair::saved_pr(&store.connection, task)?;
    let LookupResult::OpenPreexisting(pr) =
        lookup(prs, &saved.repository, &saved.tracker_issue_url)?
    else {
        return Err(StateError::LaunchBlocked.into());
    };
    let observed = verify_open_pr(
        store,
        task,
        attempt,
        prs,
        (*pr, observation_time()?),
        PrVerificationStage::Exit,
    )
    .map_err(|_| {
        SupervisorError::PrRepairHeld(crate::pr_repair::RepairHoldCause::PrIdentityUnverified)
    })?;
    let mut comparable = observed.clone();
    comparable.attempt_id = saved.attempt_id.clone();
    comparable.head_commit_sha = saved.head_commit_sha.clone();
    comparable.observed_at = saved.observed_at;
    comparable.draft = saved.draft;
    comparable.checks = saved.checks.clone();
    let identity = crate::worktree::verify_existing_worktree(store, task)?;
    if comparable != saved || identity.head != observed.head_commit_sha {
        return Err(StateError::LaunchBlocked.into());
    }
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    verify_completion_claim(projects, &selection)?;
    Ok(observed)
}

/// Recheck current policy/total attempts after restart and before launch. The
/// original deadline is never moved forward by another polling cycle.
pub(crate) fn pending(
    store: &mut StateStore,
    config: &Config,
    task: &str,
) -> Result<Option<RepairRetry>, DispatchError> {
    let attempt = task_records::latest_attempt(store, task)?.ok_or(StateError::LaunchBlocked)?;
    let Some(mut retry) = repair_retry::scheduled(store, task, &attempt)? else {
        return Ok(None);
    };
    if matches!(config.retry_policy, RetryPolicy::Disabled { .. }) {
        return Err(StateError::LaunchBlocked.into());
    }
    let (plan, receipt) = repair_retry::scheduled_context(store, task, &attempt)?;
    let now = observation_time()?;
    let observation = worker_outcome::observe_outcome(store, &plan, &receipt, now)?;
    let decision = worker_outcome::decision(
        &config.retry_policy,
        &observation,
        repair_retry::attempt_count(store, task)?,
        now,
    )?;
    if !matches!(decision, None | Some(RetryDecision::Backoff)) {
        return Err(StateError::LaunchBlocked.into());
    }
    let RetryPolicy::Bounded { backoff_secs, .. } = config.retry_policy else {
        return Err(StateError::InvalidConfig.into());
    };
    retry.backoff_until_unix_secs = retry.backoff_until_unix_secs.max(
        observation
            .observed_at_unix_secs
            .checked_add(u64::from(backoff_secs))
            .ok_or(StateError::LaunchBlocked)?,
    );
    if now < retry.backoff_until_unix_secs {
        Ok(Some(retry))
    } else {
        Ok(None)
    }
}
