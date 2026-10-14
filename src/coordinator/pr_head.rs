//! Fresh PR-head synchronization shared by monitoring and held-task restoration.
use super::{DispatchError, ports::verify_completion_claim};
use crate::{
    github::project::ProjectReader,
    model::VerifiedOpenPr,
    ownership::{WorktreeOwner, WorktreeOwnerInternal},
    state::{StateError, StateStore, journal, pr_repair, task_records},
    supervisor::{self, SupervisorError},
};

pub(crate) fn synchronize<P: ProjectReader>(
    store: &mut StateStore,
    task: &str,
    saved: &VerifiedOpenPr,
    observed: &VerifiedOpenPr,
    projects: &mut P,
) -> Result<WorktreeOwner, DispatchError> {
    // Draft/check summaries may change without changing PR ownership. Everything
    // else except observation time and the proved head must retain its identity.
    let mut comparable = observed.clone();
    comparable.head_commit_sha = saved.head_commit_sha.clone();
    comparable.observed_at = saved.observed_at;
    comparable.draft = saved.draft;
    comparable.checks = saved.checks.clone();
    if comparable != *saved {
        return Err(SupervisorError::Conflict.into());
    }
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    verify_completion_claim(projects, &selection)?;
    let owner = accounted_owner(store, task, &saved.attempt_id)?;
    let lineage = crate::worktree::pr_head::synchronize(
        store,
        task,
        &saved.head_commit_sha,
        &observed.head_commit_sha,
    )?;
    if observed.head_commit_sha != saved.head_commit_sha {
        pr_repair::record_head(store, task, saved, observed, &lineage)?;
    }
    Ok(owner)
}

pub(crate) fn accounted_owner(
    store: &mut StateStore,
    task: &str,
    attempt: &str,
) -> Result<WorktreeOwner, DispatchError> {
    if task_records::latest_attempt(store, task)?.as_deref() != Some(attempt)
        || journal::evidence_payload(store, task, Some(attempt), "pr_repair_blocked")?.is_some()
    {
        return Err(StateError::LaunchBlocked.into());
    }
    let owner = WorktreeOwner::acquire_existing(store.root(), task)
        .map_err(|_| SupervisorError::Conflict)?;
    owner
        .verify_protocol(&store.connection, store.root(), task, attempt)
        .map_err(|_| SupervisorError::Conflict)?;
    if !matches!(
        supervisor::recheck_retry_exit(store, task, attempt, &owner, false, &mut None)?,
        supervisor::Reconciliation::Completed { .. }
    ) {
        return Err(SupervisorError::Conflict.into());
    }
    Ok(owner)
}
