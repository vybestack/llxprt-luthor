use super::{
    git::*,
    types::{WorktreeError, WorktreeResult},
};
use crate::state::{StateStore, WorktreeIntent, task_records, worktree_records};
use std::{fs, path::Path};

/// Recovery never fetches a new base, resets a branch, or discards local changes.
pub(crate) fn recover_intent(
    store: &mut StateStore,
    task_id: &str,
    intent: &WorktreeIntent,
    checkout: &Path,
    git_dir: &Path,
    remote: &str,
) -> Result<WorktreeResult, WorktreeError> {
    let result = (|| {
        let base = intent
            .base_commit
            .as_deref()
            .ok_or(WorktreeError::Conflict(
                "unfinished worktree has no pinned base",
            ))?;
        match fs::symlink_metadata(&intent.path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(WorktreeError::Conflict(
                    "worktree path is not a real directory",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let root = intent.path.parent().ok_or(WorktreeError::InvalidTaskId)?;
                create_worktree_root(root, root, || {})?;
                check_new_worktree(checkout, intent)?;
                add_worktree(checkout, intent)?;
            }
            Err(error) => return Err(error.into()),
        }
        let actual = identity(&intent.path, intent, git_dir, remote).map_err(unverifiable)?;
        let status = read_git(
            &intent.path,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )
        .map_err(unverifiable)?;
        if actual.head != base || !status.is_empty() {
            return Err(WorktreeError::Conflict(
                "unfinished worktree is changed or dirty",
            ));
        }
        verify_registration(checkout, &actual)?;
        worktree_records::finish_worktree(store, task_id, &actual)?;
        Ok(WorktreeResult::Created(actual))
    })();
    if result.is_err() {
        task_records::hold_task(store, task_id, "unfinished worktree could not be verified")?;
    }
    result
}

/// A path that git cannot read as a worktree (empty or partial directory) is
/// an unverifiable path, not a git failure, so every environment reports it
/// the same way regardless of whether a parent repository encloses the path.
fn unverifiable(error: WorktreeError) -> WorktreeError {
    match error {
        WorktreeError::Git(_) => {
            WorktreeError::Conflict("worktree path is not a verifiable git worktree")
        }
        other => other,
    }
}
