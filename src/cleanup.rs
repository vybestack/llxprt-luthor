//! Opt-in cleanup of quiescent PR-complete tasks after fresh terminal PR proof.
pub use crate::cleanup_error::CleanupError;
mod removal;
mod terminal_pr;
use crate::{
    config::{CleanupPolicy, Config},
    github::pull_request::PullRequestReader,
    ownership::{OwnershipError, WorktreeOwner, WorktreeOwnerInternal},
    state::{StateError, StateStore, cleanup_records, journal, task_records},
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
        match clean_one(store, config, prs, &task) {
            Ok(true) => report.cleaned.push(task),
            Ok(false) => report.retained.push(task),
            Err(error) => {
                // Fixed reason only: upstream stderr/API data never enters operator evidence.
                journal::record_evidence(
                    store,
                    &task,
                    None,
                    "cleanup_failed",
                    "cleanup validation or removal failed; inspect cleanup intent",
                )?;
                return Err(error);
            }
        }
    }
    Ok(report)
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
    let saved = cleanup_records::verified_pr(store, task)?;
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    if selection.effective_config.worktree_root != config.worktree_root
        || selection.effective_config.state_root != config.state_root
        || !config.mappings.contains(&selection.candidate.mapping)
    {
        return Err(CleanupError::Conflict(
            "cleanup configuration differs from task ownership",
        ));
    }
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
