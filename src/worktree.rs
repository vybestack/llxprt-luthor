use crate::state::{task_records, worktree_records};
use crate::{
    config::Mapping,
    state::{StateError, StateStore, WorktreeIdentity, WorktreeIntent, WorktreeRecord},
};
use std::{fs, path::Path};

mod types;
pub use types::{WorktreeError, WorktreeInspection, WorktreeResult};
mod git;
pub(crate) use git::check_root;
use git::*;
mod recovery;
/// A never-dispatched worker must start on its exact clean saved tip, rather
/// than the descendant tip accepted when reconciling an already-run worker.
pub fn verify_never_dispatched(
    record: &WorktreeRecord,
    mapping: &Mapping,
    root: &Path,
    task_id: &str,
) -> Result<(), WorktreeError> {
    let actual = verify_record(record, mapping, root, task_id)?;
    if record.identity.as_ref() != Some(&actual)
        || !read_git(
            &actual.path,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?
        .is_empty()
    {
        return Err(WorktreeError::Conflict(
            "saved worktree tip is changed or dirty",
        ));
    }
    Ok(())
}

/// Read-only inspection of the persisted selection and Git worktree before reservation.
pub fn verify_existing_worktree(
    store: &StateStore,
    task_id: &str,
) -> Result<WorktreeIdentity, WorktreeError> {
    let selection = task_records::selection_evidence(store, task_id)?
        .ok_or(WorktreeError::Conflict("missing selection"))?;
    let record = worktree_records::worktree_record(store, task_id)?
        .ok_or(WorktreeError::Conflict("missing worktree intent"))?;
    verify_record(
        &record,
        &selection.candidate.mapping,
        &selection.effective_config.worktree_root,
        task_id,
    )
}

/// Recheck the same durable snapshot from a read-only supervisor connection.
pub fn verify_record(
    record: &WorktreeRecord,
    mapping: &Mapping,
    root: &Path,
    task_id: &str,
) -> Result<WorktreeIdentity, WorktreeError> {
    let expected = record
        .identity
        .as_ref()
        .ok_or(WorktreeError::Conflict("missing worktree identity"))?;
    if record.intent.path != check_root(root)?.join(task_id)
        || record.intent.branch != format!("luthor/{task_id}")
        || record.intent.base != mapping.base_branch
        || record.intent.repository != mapping.code_repository
        || expected.path != record.intent.path
        || expected.branch != record.intent.branch
        || expected.base != record.intent.base
        || expected.base_commit != record.intent.base_commit
        || expected.repository != record.intent.repository
    {
        return Err(WorktreeError::Conflict(
            "persisted worktree identity differs from disk",
        ));
    }
    verify_path_identity(expected)?;
    let (_, git_dir, remote) = validate_checkout(mapping)?;
    let actual = identity(&record.intent.path, &record.intent, &git_dir, &remote)?;
    if !matches_snapshot(expected, &actual)? {
        return Err(WorktreeError::Conflict(
            "persisted worktree identity differs from disk",
        ));
    }
    Ok(actual)
}

/// Compare immutable ownership and require the live tip to extend the recorded tip.
pub fn matches_snapshot(
    expected: &WorktreeIdentity,
    actual: &WorktreeIdentity,
) -> Result<bool, WorktreeError> {
    let mut stable = actual.clone();
    stable.head = expected.head.clone();
    if stable != *expected {
        return Ok(false);
    }
    let status = git(
        &actual.path,
        &["merge-base", "--is-ancestor", &expected.head, &actual.head],
    )?
    .status;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(WorktreeError::Git("cannot compare worktree history")),
    }
}

/// Check the live Git identity against an immutable launch snapshot without writing to Git.
pub fn verify_snapshot(expected: &WorktreeIdentity) -> Result<(), WorktreeError> {
    verify_path_identity(expected)?;
    let actual = identity(
        &expected.path,
        &WorktreeIntent {
            path: expected.path.clone(),
            branch: expected.branch.clone(),
            base: expected.base.clone(),
            base_commit: expected.base_commit.clone(),
            repository: expected.repository.clone(),
        },
        &expected.git_directory,
        &expected.remote,
    )?;
    if !matches_snapshot(expected, &actual)? {
        return Err(WorktreeError::Conflict("live worktree identity changed"));
    }
    Ok(())
}

/// Read-only inspection never turns an unfinished intent into a verified worktree.
pub fn inspect_record(
    record: &WorktreeRecord,
    mapping: &Mapping,
) -> Result<WorktreeInspection, WorktreeError> {
    let intent = &record.intent;
    if intent.branch
        != format!(
            "luthor/{}",
            intent
                .path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
        )
        || intent.base != mapping.base_branch
        || intent.repository != mapping.code_repository
    {
        return Ok(WorktreeInspection::IdentityMismatch);
    }
    if let Some(expected) = &record.identity {
        let (_, git_dir, remote) = validate_checkout(mapping)?;
        return Ok(match identity(&intent.path, intent, &git_dir, &remote) {
            Ok(actual) if matches_snapshot(expected, &actual)? => {
                WorktreeInspection::IdentityMatches
            }
            _ => WorktreeInspection::IdentityMismatch,
        });
    }
    match fs::symlink_metadata(&intent.path) {
        Ok(_) => Ok(WorktreeInspection::UnverifiedPathPresent),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(WorktreeInspection::UnverifiedPathAbsent)
        }
        Err(error) => Err(error.into()),
    }
}

/// Checks worktree feasibility without creating files, branches, or Git worktrees.
pub fn preflight(
    task_id: &str,
    worktree_root: &Path,
    mapping: &Mapping,
) -> Result<(), WorktreeError> {
    validate_task_id(task_id)?;
    let root = check_root(worktree_root)?;
    let (checkout, _, _) = validate_checkout(mapping)?;
    check_new_worktree(
        &checkout,
        &WorktreeIntent {
            path: root.join(task_id),
            branch: format!("luthor/{task_id}"),
            base: mapping.base_branch.clone(),
            base_commit: None,
            repository: mapping.code_repository.clone(),
        },
    )
}

/// Returns existing evidence only after comparing it to the current filesystem and Git identity.
/// An unfinished intent is completed only at its exact clean pinned base.
pub fn ensure_worktree(
    store: &mut StateStore,
    task_id: &str,
    worktree_root: &Path,
    mapping: &Mapping,
) -> Result<WorktreeResult, WorktreeError> {
    ensure_worktree_with_hooks(store, task_id, worktree_root, mapping, |_| {}, || {})
}

/// Hooks for testing rejection before intent and interruption after root creation.
#[doc(hidden)]
pub fn ensure_worktree_with_hooks(
    store: &mut StateStore,
    task_id: &str,
    worktree_root: &Path,
    mapping: &Mapping,
    before_intent: impl FnOnce(&mut StateStore),
    after_root_created: impl FnOnce(),
) -> Result<WorktreeResult, WorktreeError> {
    validate_task_id(task_id)?;
    verify_selected_mapping(store, task_id, worktree_root, mapping)?;
    let root = check_root(worktree_root)?;
    let mut intent = WorktreeIntent {
        path: root.join(task_id),
        branch: format!("luthor/{task_id}"),
        base: mapping.base_branch.clone(),
        base_commit: None,
        repository: mapping.code_repository.clone(),
    };
    let (checkout, git_dir, remote) = validate_checkout(mapping)?;
    if let Some(record) = worktree_records::worktree_record(store, task_id)? {
        return reuse_worktree(
            store, task_id, record, &intent, &checkout, &git_dir, &remote,
        );
    }
    check_new_worktree(&checkout, &intent)?;
    intent.base_commit = Some(match fetch_base(&checkout, mapping, task_id) {
        Ok(commit) => commit,
        Err(error) => {
            task_records::hold_task(store, task_id, "remote base fetch failed")?;
            return Err(error);
        }
    });
    before_intent(store);
    worktree_records::begin_worktree(store, task_id, &intent)?;
    let outcome = (|| {
        create_worktree_root(worktree_root, &root, after_root_created)?;
        check_new_worktree(&checkout, &intent)?;
        add_worktree(&checkout, &intent)?;
        let created = identity(&intent.path, &intent, &git_dir, &remote)?;
        worktree_records::finish_worktree(store, task_id, &created)?;
        Ok(WorktreeResult::Created(created))
    })();
    if outcome.is_err() {
        task_records::set_task_phase(store, task_id, "held")?;
    }
    outcome
}

fn verify_selected_mapping(
    store: &StateStore,
    task_id: &str,
    worktree_root: &Path,
    mapping: &Mapping,
) -> Result<(), WorktreeError> {
    let selection = worktree_records::claimed_worktree_context(store, task_id).map_err(
        |error| match error {
            StateError::InvalidSelection => WorktreeError::NotClaimed,
            other => WorktreeError::State(other),
        },
    )?;
    if &selection.candidate.mapping != mapping
        || selection.effective_config.worktree_root != worktree_root
        || !selection.effective_config.mappings.contains(mapping)
    {
        return Err(WorktreeError::Conflict(
            "mapping or root differs from selected task",
        ));
    }
    Ok(())
}

fn reuse_worktree(
    store: &mut StateStore,
    task_id: &str,
    record: WorktreeRecord,
    intent: &WorktreeIntent,
    checkout: &Path,
    git_dir: &Path,
    remote: &str,
) -> Result<WorktreeResult, WorktreeError> {
    let mut saved_intent = record.intent.clone();
    saved_intent.base_commit = None;
    if saved_intent != *intent {
        return Err(WorktreeError::Conflict(
            "worktree intent differs from mapping",
        ));
    }
    let Some(expected) = record.identity else {
        return recovery::recover_intent(store, task_id, &record.intent, checkout, git_dir, remote);
    };
    let result = identity(&intent.path, &record.intent, git_dir, remote);
    match result {
        Ok(actual) if matches_snapshot(&expected, &actual)? => {
            Ok(WorktreeResult::Existing(expected))
        }
        _ => Err(WorktreeError::Conflict(
            "persisted worktree identity differs from disk",
        )),
    }
}
