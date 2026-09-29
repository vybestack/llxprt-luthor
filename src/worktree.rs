use crate::{
    config::Mapping,
    state::{StateError, StateStore, WorktreeIdentity, WorktreeIntent, WorktreeRecord},
};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("invalid task id")]
    InvalidTaskId,
    #[error("task has no verified claim")]
    NotClaimed,
    #[error("worktree conflict: {0}")]
    Conflict(&'static str),
    #[error("git operation failed: {0}")]
    Git(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    State(#[from] StateError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeResult {
    Created(WorktreeIdentity),
    Existing(WorktreeIdentity),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeInspection {
    UnverifiedPathPresent,
    UnverifiedPathAbsent,
    IdentityMatches,
    IdentityMismatch,
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
            Ok(actual) if &actual == expected => WorktreeInspection::IdentityMatches,
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

fn git(dir: &Path, args: &[&str]) -> Result<Output, WorktreeError> {
    Ok(Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()?)
}

fn read_git(dir: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let out = git(dir, args)?;
    if !out.status.success() {
        return Err(WorktreeError::Git("read failed"));
    }
    String::from_utf8(out.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|_| WorktreeError::Git("invalid UTF-8"))
}

fn repo_from_url(url: &str) -> Option<&str> {
    let path = if let Some((_, path)) = url.split_once("://") {
        let (_, path) = path.split_once('/')?;
        path
    } else if let Some((_, path)) = url.split_once(':') {
        path
    } else {
        return None;
    };
    Some(path.strip_suffix(".git").unwrap_or(path))
}

fn common_directory(dir: &Path) -> Result<PathBuf, WorktreeError> {
    let path = PathBuf::from(read_git(dir, &["rev-parse", "--git-common-dir"])?);
    Ok(fs::canonicalize(if path.is_absolute() {
        path
    } else {
        dir.join(path)
    })?)
}

fn validate_checkout(mapping: &Mapping) -> Result<(PathBuf, PathBuf, String), WorktreeError> {
    let checkout = fs::canonicalize(&mapping.checkout)?;
    if Path::new(&read_git(&checkout, &["rev-parse", "--show-toplevel"])?) != checkout {
        return Err(WorktreeError::Conflict(
            "checkout is not the repository root",
        ));
    }
    let git_dir = common_directory(&checkout)?;
    let origin = read_git(&checkout, &["remote", "get-url", "origin"])?;
    if repo_from_url(&origin) != Some(mapping.code_repository.as_str()) {
        return Err(WorktreeError::Conflict(
            "checkout origin differs from code repository",
        ));
    }
    let remote = if mapping.push_remote.contains(':') || mapping.push_remote.contains('/') {
        mapping.push_remote.clone()
    } else {
        read_git(&checkout, &["remote", "get-url", &mapping.push_remote])?
    };
    if ![
        mapping.code_repository.as_str(),
        mapping.allowed_pr_head_repository.as_str(),
    ]
    .contains(&repo_from_url(&remote).unwrap_or_default())
    {
        return Err(WorktreeError::Conflict(
            "push remote differs from mapped repositories",
        ));
    }
    let base_ref = format!("refs/heads/{}", mapping.base_branch);
    if !git(&checkout, &["show-ref", "--verify", "--quiet", &base_ref])?
        .status
        .success()
    {
        return Err(WorktreeError::Conflict("base branch is absent"));
    }
    Ok((checkout, git_dir, remote))
}

#[cfg(target_os = "macos")]
fn system_alias(path: &Path, metadata: &fs::Metadata) -> Result<bool, WorktreeError> {
    use std::os::unix::fs::MetadataExt;
    let target = match path.to_str() {
        Some("/var") => Path::new("private/var"),
        Some("/tmp") => Path::new("private/tmp"),
        Some("/etc") => Path::new("private/etc"),
        _ => return Ok(false),
    };
    Ok(metadata.uid() == 0 && fs::read_link(path)? == target)
}

fn validate_root_components(root: &Path) -> Result<(), WorktreeError> {
    if !root.is_absolute()
        || root
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(WorktreeError::Conflict("invalid worktree root path"));
    }
    for ancestor in root.ancestors().collect::<Vec<_>>().into_iter().rev() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                #[cfg(target_os = "macos")]
                if system_alias(ancestor, &metadata)? {
                    continue;
                }
                return Err(WorktreeError::Conflict("worktree root contains a symlink"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn normalized_root(root: &Path) -> Result<PathBuf, WorktreeError> {
    validate_root_components(root)?;
    let existing = root
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .ok_or(WorktreeError::Conflict("invalid worktree root path"))?;
    let canonical = fs::canonicalize(existing)?;
    Ok(canonical.join(
        root.strip_prefix(existing)
            .expect("ancestor must be a prefix of root"),
    ))
}

#[cfg(unix)]
fn file_identity(path: &Path) -> Result<(u64, u64), WorktreeError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn identity(
    path: &Path,
    intent: &WorktreeIntent,
    git_dir: &Path,
    remote: &str,
) -> Result<WorktreeIdentity, WorktreeError> {
    let canonical = fs::canonicalize(path)?;
    let (device, inode) = file_identity(path)?;
    let actual_git_dir = common_directory(path)?;
    let branch = read_git(path, &["symbolic-ref", "--quiet", "HEAD"])?;
    let head = read_git(path, &["rev-parse", "HEAD"])?;
    let top = read_git(path, &["rev-parse", "--show-toplevel"])?;
    if canonical != intent.path
        || top != canonical
        || actual_git_dir != git_dir
        || branch != format!("refs/heads/{}", intent.branch)
    {
        return Err(WorktreeError::Conflict(
            "worktree repository, path or branch changed",
        ));
    }
    Ok(WorktreeIdentity {
        path: canonical,
        device,
        inode,
        branch: intent.branch.clone(),
        base: intent.base.clone(),
        head,
        repository: intent.repository.clone(),
        git_directory: actual_git_dir,
        remote: remote.to_owned(),
    })
}

fn validate_task_id(task_id: &str) -> Result<(), WorktreeError> {
    if task_id.is_empty()
        || task_id.len() > 128
        || !task_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || task_id.starts_with('-')
    {
        return Err(WorktreeError::InvalidTaskId);
    }
    Ok(())
}

fn check_root(root: &Path) -> Result<PathBuf, WorktreeError> {
    let normalized = normalized_root(root)?;
    match fs::symlink_metadata(root) {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err(WorktreeError::Conflict(
                "worktree root is not a real directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(normalized)
}

fn check_new_worktree(checkout: &Path, intent: &WorktreeIntent) -> Result<(), WorktreeError> {
    if intent.path.starts_with(checkout) || checkout.starts_with(&intent.path) {
        return Err(WorktreeError::Conflict("worktree path overlaps checkout"));
    }
    match fs::symlink_metadata(&intent.path) {
        Ok(_) => return Err(WorktreeError::Conflict("worktree path already exists")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let branch_ref = format!("refs/heads/{}", intent.branch);
    let branch_check = git(checkout, &["show-ref", "--verify", "--quiet", &branch_ref])?;
    if branch_check.status.code() != Some(1) {
        return Err(WorktreeError::Conflict(
            "branch exists or cannot be checked",
        ));
    }
    Ok(())
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
            repository: mapping.code_repository.clone(),
        },
    )
}

/// Returns existing evidence only after comparing it to the current filesystem and Git identity.
/// An unfinished intent is never retried or inferred complete from Git state.
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
    let selection = store
        .claimed_worktree_context(task_id)
        .map_err(|error| match error {
            StateError::InvalidSelection => WorktreeError::NotClaimed,
            other => WorktreeError::State(other),
        })?;
    if &selection.candidate.mapping != mapping
        || selection.effective_config.worktree_root != worktree_root
        || !selection.effective_config.mappings.contains(mapping)
    {
        return Err(WorktreeError::Conflict(
            "mapping or root differs from selected task",
        ));
    }
    let root = check_root(worktree_root)?;
    let intent = WorktreeIntent {
        path: root.join(task_id),
        branch: format!("luthor/{task_id}"),
        base: mapping.base_branch.clone(),
        repository: mapping.code_repository.clone(),
    };
    let (checkout, git_dir, remote) = validate_checkout(mapping)?;
    if let Some(record) = store.worktree_record(task_id)? {
        if record.intent != intent {
            return Err(WorktreeError::Conflict(
                "worktree intent differs from mapping",
            ));
        }
        let Some(expected) = record.identity else {
            store.set_task_phase(task_id, "held")?;
            return Err(WorktreeError::Conflict(
                "unfinished worktree intent requires inspection",
            ));
        };
        let result = identity(&intent.path, &intent, &git_dir, &remote);
        return match result {
            Ok(actual) if actual == expected => Ok(WorktreeResult::Existing(expected)),
            _ => Err(WorktreeError::Conflict(
                "persisted worktree identity differs from disk",
            )),
        };
    }
    check_new_worktree(&checkout, &intent)?;
    before_intent(store);
    store.begin_worktree(task_id, &intent)?;
    let outcome = (|| {
        if check_root(worktree_root)? != root {
            return Err(WorktreeError::Conflict("worktree root changed"));
        }
        fs::create_dir_all(worktree_root)?;
        after_root_created();
        if check_root(worktree_root)? != root || fs::canonicalize(worktree_root)? != root {
            return Err(WorktreeError::Conflict("worktree root changed"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        check_new_worktree(&checkout, &intent)?;
        let output = git(
            &checkout,
            &[
                "worktree",
                "add",
                "-b",
                &intent.branch,
                intent
                    .path
                    .to_str()
                    .ok_or(WorktreeError::Conflict("non-UTF8 worktree path"))?,
                &intent.base,
            ],
        )?;
        if !output.status.success() {
            return Err(WorktreeError::Git("worktree add failed"));
        }
        let created = identity(&intent.path, &intent, &git_dir, &remote)?;
        store.finish_worktree(task_id, &created)?;
        Ok(WorktreeResult::Created(created))
    })();
    if outcome.is_err() {
        store.set_task_phase(task_id, "held")?;
    }
    outcome
}
