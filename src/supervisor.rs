//! Fail-closed launch planning. No child execution is permitted until a detached
//! supervisor, process gate, and independently verified termination are implemented.
use crate::{
    config::{ConfigError, RenderedCommand, TaskValues},
    state::{StateError, StateStore},
};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("launch plan does not match verified task or worktree")]
    Conflict,
    #[error("supervisor execution is unavailable; reservation held for reconciliation")]
    ExecutionUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub task_id: String,
    pub attempt_id: String,
    pub session_id: String,
    pub worktree: PathBuf,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub config_revision: String,
}

fn requires_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .filter(|pair| pair[0] == flag && pair[1] == value)
        .count()
        == 1
}

fn prompt(args: &[String]) -> Option<&str> {
    let mut matches = args
        .windows(2)
        .filter(|pair| matches!(pair[0].as_str(), "-p" | "--prompt"))
        .map(|pair| pair[1].as_str());
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Renders one initial attempt; the caller remains responsible for fresh claim
/// and absent-PR evidence. This function deliberately cannot start a worker.
pub fn prepare_initial(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<LaunchPlan, SupervisorError> {
    let selection = store.claimed_worktree_context(task_id)?;
    let record = store
        .worktree_record(task_id)?
        .ok_or(SupervisorError::Conflict)?;
    let identity = record.identity.ok_or(SupervisorError::Conflict)?;
    if identity.path != record.intent.path
        || identity.repository != selection.candidate.mapping.code_repository
        || identity.path != fs::canonicalize(&identity.path)?
    {
        return Err(SupervisorError::Conflict);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(&identity.path)?;
        if metadata.dev() != identity.device || metadata.ino() != identity.inode {
            return Err(SupervisorError::Conflict);
        }
    }
    let values = TaskValues {
        task_issue_number: selection.candidate.issue_number.to_string(),
        task_repository: selection.candidate.repository.clone(),
        task_issue_url: selection.candidate.issue_url.clone(),
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        worktree: identity.path.to_string_lossy().into_owned(),
    };
    let RenderedCommand { executable, args } =
        selection.effective_config.initial.render(&values)?;
    let worktree = identity.path;
    let cwd = worktree.to_str().ok_or(SupervisorError::Conflict)?;
    if !requires_pair(&args, "--session", task_id)
        || !requires_pair(&args, "--cwd", cwd)
        || prompt(&args).is_none_or(str::is_empty)
    {
        return Err(SupervisorError::Conflict);
    }
    let plan = LaunchPlan {
        task_id: task_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        session_id: task_id.to_owned(),
        worktree,
        executable,
        args,
        config_revision: selection.config_revision,
    };
    store.hold_launch_intent(task_id, attempt_id, &serde_json::to_string(&plan)?)?;
    Ok(plan)
}

/// Intentional hard stop: no caller can treat a prepared plan as launch permission.
pub fn execute(_plan: &LaunchPlan) -> Result<(), SupervisorError> {
    Err(SupervisorError::ExecutionUnavailable)
}
