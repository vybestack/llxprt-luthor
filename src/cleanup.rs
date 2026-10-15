//! Opt-in cleanup of quiescent PR-complete tasks after fresh terminal PR proof.
pub use crate::cleanup_error::CleanupError;
mod removal;
pub(crate) mod terminal_pr;
use crate::{
    config::{CleanupPolicy, Config},
    github::pull_request::PullRequestReader,
    ownership::{OwnershipError, WorktreeOwner, WorktreeOwnerInternal},
    state::{
        StateError, StateStore, cleanup_records, journal, merge_followup, pr_merge, task_records,
    },
    supervisor, worktree,
};
use serde::Serialize;
use serde_json::json;
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Default, Serialize)]
pub struct CleanupReport {
    pub cleaned: Vec<String>,
    pub retained: Vec<String>,
    pub skipped: Vec<CleanupSkip>,
}

#[derive(Debug, Serialize)]
pub struct CleanupSkip {
    pub task_id: String,
    pub reason: String,
}

pub fn clean_terminal_tasks<Q: PullRequestReader>(
    store: &mut StateStore,
    config: &Config,
    prs: &mut Q,
) -> Result<CleanupReport, CleanupError> {
    config
        .cleanup
        .validate()
        .map_err(|_| StateError::InvalidConfig)?;
    let mut report = CleanupReport::default();
    if config.cleanup == CleanupPolicy::Disabled {
        return Ok(report);
    }
    for task in cleanup_records::candidates(store)? {
        let result = verify_configuration(store, config, &task).map(|_| ());
        if result.is_ok() && !crate::state::pr_poll::begin(store, &task, "cleanup", "complete")? {
            continue;
        }
        let result = result.and_then(|_| clean_one(store, config, prs, &task));
        record_result(store, &task, result, &mut report)?;
    }
    Ok(report)
}

/// Last optional cleanup opportunity, after delivery/closure proof and before
/// durable retirement. Cleanup remains opt-in, never a prerequisite for delivery.
pub(crate) fn clean_delivered_task<Q: PullRequestReader>(
    store: &mut StateStore,
    config: &Config,
    prs: &mut Q,
    task: &str,
) -> Result<CleanupReport, StateError> {
    let mut report = CleanupReport::default();
    if config.cleanup != CleanupPolicy::Disabled && crate::github::budget::optional_allowed() {
        let result = clean_one(store, config, prs, task);
        record_result(store, task, result, &mut report)?;
    }
    Ok(report)
}

fn record_result(
    store: &mut StateStore,
    task: &str,
    result: Result<bool, CleanupError>,
    report: &mut CleanupReport,
) -> Result<(), StateError> {
    match result {
        Ok(true) => report.cleaned.push(task.into()),
        Ok(false) => report.retained.push(task.into()),
        Err(error) => {
            let reason = error.operator_reason();
            journal::record_evidence(store, task, None, "cleanup_failed", &reason)?;
            report.skipped.push(CleanupSkip {
                task_id: task.into(),
                reason,
            });
        }
    }
    Ok(())
}

impl CleanupReport {
    pub(crate) fn append(&mut self, other: &mut Self) {
        self.cleaned.append(&mut other.cleaned);
        self.retained.append(&mut other.retained);
        self.skipped.append(&mut other.skipped);
    }
}

fn clean_one<Q: PullRequestReader>(
    store: &mut StateStore,
    config: &Config,
    prs: &mut Q,
    task: &str,
) -> Result<bool, CleanupError> {
    if cleanup_records::has_intent(store, task)? {
        return Err(CleanupError::Conflict(
            "unfinished cleanup intent requires operator inspection",
        ));
    }
    if tracker_closure_outstanding(store, config, task)? {
        return Ok(false);
    }
    let saved = cleanup_records::verified_pr(store, task)?;
    verify_configuration(store, config, task)?;
    let owner = match WorktreeOwner::acquire_existing(store.root(), task) {
        Ok(owner) => owner,
        Err(OwnershipError::Busy) => return Ok(false),
        Err(OwnershipError::Unavailable) => {
            return Err(CleanupError::Conflict("cleanup owner unavailable"));
        }
    };
    let mut terminal_proof = None;
    if !matches!(
        supervisor::recheck_retry_exit(
            store,
            task,
            &saved.attempt_id,
            &owner,
            false,
            &mut terminal_proof
        )?,
        supervisor::Reconciliation::Completed { .. }
    ) {
        return Err(CleanupError::Conflict(
            "cleanup process quiescence unproven",
        ));
    }
    let Some(pr) = terminal_pr::observe(prs, &saved)? else {
        return Ok(false);
    };
    let identity = worktree::verify_existing_worktree(store, task)?;
    verify_protected_paths(store, config, &identity)?;
    if matches!(config.cleanup, CleanupPolicy::Worktree)
        && pr["head_commit_sha"].as_str() != Some(identity.head.as_str())
    {
        return Err(CleanupError::Conflict(
            "cleanup worktree tip differs from verified PR tip",
        ));
    }
    let targets = removal::targets(&identity, &config.cleanup)?;
    owner
        .verify_protocol(&store.connection, store.root(), task, &saved.attempt_id)
        .map_err(|_| CleanupError::Conflict("cleanup owner protocol changed"))?;
    let observed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_secs();
    cleanup_records::begin(store, task, &json!({"policy":config.cleanup,"worktree":identity,"targets":targets,"pr":pr,"observed_at_unix_secs":observed_at}).to_string())?;
    remove_targets(store, task, &identity, targets, &config.cleanup)?;
    journal::record_evidence(
        store,
        task,
        None,
        "cleanup_complete",
        &json!({"policy":config.cleanup,"observed_at_unix_secs":observed_at}).to_string(),
    )?;
    Ok(true)
}

/// A merge Luthor made keeps its task, worktree and branch until the merge
/// sweep has recorded the tracker closure, even when that step failed in an
/// earlier cycle and was held rather than failing the cycle.
fn tracker_closure_outstanding(
    store: &StateStore,
    config: &Config,
    task: &str,
) -> Result<bool, CleanupError> {
    if !pr_merge::completed(store, task)? || merge_followup::closure(store, task)?.is_some() {
        return Ok(false);
    }
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    Ok(config.mappings.iter().any(|mapping| {
        mapping.tracker_repository == selection.candidate.repository && !mapping.merge.is_disabled()
    }))
}

fn verify_configuration(
    store: &StateStore,
    config: &Config,
    task: &str,
) -> Result<(), CleanupError> {
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    if selection.effective_config.worktree_root != config.worktree_root
        || selection.effective_config.state_root != config.state_root
        || selection.effective_config.assignment_login != config.assignment_login
        || selection.effective_config.sources != config.sources
        || !crate::config::same_worker_mappings(
            &selection.effective_config.mappings,
            &config.mappings,
        )
        || selection.effective_config.initial != config.initial
        || selection.effective_config.resume != config.resume
    {
        return Err(CleanupError::Conflict(
            "cleanup configuration differs from task ownership",
        ));
    }
    Ok(())
}

fn remove_targets(
    store: &mut StateStore,
    task: &str,
    identity: &crate::state::WorktreeIdentity,
    targets: Vec<PathBuf>,
    policy: &CleanupPolicy,
) -> Result<(), CleanupError> {
    for path in targets {
        worktree::verify_snapshot(identity)?;
        removal::remove(identity, &path, matches!(policy, CleanupPolicy::Worktree))?;
        journal::record_evidence(
            store,
            task,
            None,
            "cleanup_removed",
            &json!({"path":path}).to_string(),
        )?;
    }
    Ok(())
}

fn verify_protected_paths(
    store: &StateStore,
    config: &Config,
    identity: &crate::state::WorktreeIdentity,
) -> Result<(), CleanupError> {
    let mut protected = vec![
        std::fs::canonicalize(store.root())?,
        identity.git_directory.clone(),
    ];
    for mapping in &config.mappings {
        protected.push(std::fs::canonicalize(&mapping.checkout)?);
    }
    if protected.iter().any(|protected| {
        identity.path.starts_with(protected) || protected.starts_with(&identity.path)
    }) {
        return Err(CleanupError::Conflict(
            "cleanup worktree overlaps protected directory",
        ));
    }
    Ok(())
}
