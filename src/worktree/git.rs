use super::types::WorktreeError;
use crate::{
    config::Mapping,
    state::{WorktreeIdentity, WorktreeIntent},
};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<Output, WorktreeError> {
    Ok(Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()?)
}

pub(crate) fn read_git(dir: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let out = git(dir, args)?;
    if !out.status.success() {
        return Err(WorktreeError::Git("read failed"));
    }
    String::from_utf8(out.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|_| WorktreeError::Git("invalid UTF-8"))
}

pub(crate) fn repo_from_url(url: &str) -> Option<&str> {
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

pub(crate) fn common_directory(dir: &Path) -> Result<PathBuf, WorktreeError> {
    let path = PathBuf::from(read_git(dir, &["rev-parse", "--git-common-dir"])?);
    Ok(fs::canonicalize(if path.is_absolute() {
        path
    } else {
        dir.join(path)
    })?)
}

pub(crate) fn validate_checkout(
    mapping: &Mapping,
) -> Result<(PathBuf, PathBuf, String), WorktreeError> {
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
    if !git(
        &checkout,
        &[
            "check-ref-format",
            &format!("refs/heads/{}", mapping.base_branch),
        ],
    )?
    .status
    .success()
    {
        return Err(WorktreeError::Conflict("invalid base branch ref"));
    }
    Ok((checkout, git_dir, remote))
}

#[cfg(target_os = "macos")]
pub(crate) fn system_alias(path: &Path, metadata: &fs::Metadata) -> Result<bool, WorktreeError> {
    use std::os::unix::fs::MetadataExt;
    let target = match path.to_str() {
        Some("/var") => Path::new("private/var"),
        Some("/tmp") => Path::new("private/tmp"),
        Some("/etc") => Path::new("private/etc"),
        _ => return Ok(false),
    };
    Ok(metadata.uid() == 0 && fs::read_link(path)? == target)
}

pub(crate) fn validate_root_components(root: &Path) -> Result<(), WorktreeError> {
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

pub(crate) fn normalized_root(root: &Path) -> Result<PathBuf, WorktreeError> {
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
pub(crate) fn file_identity(path: &Path) -> Result<(u64, u64), WorktreeError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

/// Reject a replaced directory before asking Git to discover its repository.
/// Otherwise Git may fail generically or discover an unrelated parent checkout.
#[cfg(unix)]
pub(crate) fn verify_path_identity(expected: &WorktreeIdentity) -> Result<(), WorktreeError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(&expected.path)?;
    if !metadata.is_dir() || metadata.dev() != expected.device || metadata.ino() != expected.inode {
        return Err(WorktreeError::Conflict(
            "worktree repository, path or branch changed",
        ));
    }
    Ok(())
}

pub(crate) fn identity(
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
        base_commit: intent.base_commit.clone(),
        head,
        repository: intent.repository.clone(),
        git_directory: actual_git_dir,
        remote: remote.to_owned(),
    })
}

pub(crate) fn validate_task_id(task_id: &str) -> Result<(), WorktreeError> {
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

pub(crate) fn check_root(root: &Path) -> Result<PathBuf, WorktreeError> {
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

pub(crate) fn check_new_worktree(
    checkout: &Path,
    intent: &WorktreeIntent,
) -> Result<(), WorktreeError> {
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

pub(crate) fn create_worktree_root(
    worktree_root: &Path,
    root: &Path,
    after_root_created: impl FnOnce(),
) -> Result<(), WorktreeError> {
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
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(crate) fn add_worktree(checkout: &Path, intent: &WorktreeIntent) -> Result<(), WorktreeError> {
    let output = git(
        checkout,
        &[
            "worktree",
            "add",
            "-b",
            &intent.branch,
            intent
                .path
                .to_str()
                .ok_or(WorktreeError::Conflict("non-UTF8 worktree path"))?,
            intent
                .base_commit
                .as_deref()
                .ok_or(WorktreeError::Conflict(
                    "new worktree has no fetched base commit",
                ))?,
        ],
    )?;
    if !output.status.success() {
        return Err(WorktreeError::Git("worktree add failed"));
    }
    Ok(())
}

/// Pin each task's fetch in its own ref: FETCH_HEAD and remote-tracking refs can
/// be changed by another dispatch. Never update the operator's local base branch.
pub(crate) fn fetch_base(
    checkout: &Path,
    mapping: &Mapping,
    task_id: &str,
) -> Result<String, WorktreeError> {
    let source = format!("refs/heads/{}", mapping.base_branch);
    let target = format!("refs/luthor/task-bases/{task_id}");
    let refspec = format!("+{source}:{target}");
    let output = git(
        checkout,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--",
            &mapping.push_remote,
            &refspec,
        ],
    )
    .map_err(|_| WorktreeError::BaseFetch)?;
    if !output.status.success() {
        return Err(WorktreeError::BaseFetch);
    }
    read_git(
        checkout,
        &["rev-parse", "--verify", &format!("{target}^{{commit}}")],
    )
    .map_err(|_| WorktreeError::BaseFetch)
}

/// A Git directory pointer alone is not proof that this path is registered.
pub(crate) fn verify_registration(
    checkout: &Path,
    identity: &WorktreeIdentity,
) -> Result<(), WorktreeError> {
    let entries = read_git(checkout, &["worktree", "list", "--porcelain"])?;
    let path = format!("worktree {}", identity.path.display());
    let head = format!("HEAD {}", identity.head);
    let branch = format!("branch refs/heads/{}", identity.branch);
    let matches = entries
        .split("\n\n")
        .filter(|entry| {
            let lines = entry.lines().collect::<Vec<_>>();
            lines.contains(&path.as_str())
                && lines.contains(&head.as_str())
                && lines.contains(&branch.as_str())
        })
        .count();
    if matches != 1 {
        return Err(WorktreeError::Conflict(
            "worktree is not registered at the intended path",
        ));
    }
    Ok(())
}
