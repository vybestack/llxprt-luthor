//! Validate all deletion targets before journaling intent. Never follow a
//! symlink or traverse another filesystem, nor delete tracked artifact paths.
use crate::cleanup_error::CleanupError;
use crate::{config::CleanupPolicy, state::WorktreeIdentity};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn targets(
    identity: &WorktreeIdentity,
    policy: &CleanupPolicy,
) -> Result<Vec<PathBuf>, CleanupError> {
    let paths = match policy {
        CleanupPolicy::Disabled => return Ok(vec![]),
        CleanupPolicy::Artifacts { paths } => {
            paths.iter().map(|path| identity.path.join(path)).collect()
        }
        CleanupPolicy::Worktree => {
            if !git_output(
                &identity.path,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            )?
            .is_empty()
            {
                return Err(CleanupError::Conflict(
                    "cleanup worktree contains unsaved files",
                ));
            }
            if git_output(&identity.path, &["ls-files", "--stage"])?
                .split(|byte| *byte == b'\n')
                .any(|entry| entry.starts_with(b"160000 "))
            {
                return Err(CleanupError::Conflict(
                    "cleanup worktree contains tracked submodule",
                ));
            }
            vec![identity.path.clone()]
        }
    };
    let mut present = Vec::new();
    for path in paths {
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                verify_parents(&identity.path, &path)?;
                continue;
            }
            Err(error) => return Err(error.into()),
        }
        verify_parents(&identity.path, &path)?;
        inspect_tree(
            &path,
            identity.device,
            &identity.path,
            matches!(policy, CleanupPolicy::Worktree),
        )?;
        if matches!(policy, CleanupPolicy::Artifacts { .. }) {
            let relative = path
                .strip_prefix(&identity.path)
                .map_err(|_| CleanupError::Conflict("artifact outside worktree"))?;
            let relative = relative
                .to_str()
                .ok_or(CleanupError::Conflict("non-UTF8 artifact path"))?;
            if !git_output(&identity.path, &["ls-files", "--", relative])?.is_empty()
                || !ignored(&identity.path, relative)?
            {
                return Err(CleanupError::Conflict(
                    "cleanup artifact must be ignored and untracked",
                ));
            }
        }
        present.push(path);
    }
    Ok(present)
}

fn verify_parents(root: &Path, path: &Path) -> Result<(), CleanupError> {
    for parent in path.ancestors().take_while(|parent| *parent != root) {
        match fs::symlink_metadata(parent) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(CleanupError::Conflict("cleanup path contains symlink"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn inspect_tree(
    path: &Path,
    device: u64,
    worktree: &Path,
    whole: bool,
) -> Result<(), CleanupError> {
    let meta = fs::symlink_metadata(path)?;
    if path.file_name().is_some_and(|name| name == ".git")
        && path != worktree.join(".git")
        && !removable_fixture_repository(path, worktree, whole)?
    {
        return Err(CleanupError::Conflict(
            "cleanup tree contains another repository",
        ));
    }
    if meta.file_type().is_symlink() || meta.dev() != device || !(meta.is_file() || meta.is_dir()) {
        return Err(CleanupError::Conflict(
            "cleanup tree contains symlink, mount or special file",
        ));
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            inspect_tree(&entry?.path(), device, worktree, whole)?;
        }
    }
    Ok(())
}

/// Only whole-worktree cleanup can remove fixture repositories, and only within
/// the worker's confined tmp/ output. Gitlinks and tracked fixture files refuse.
fn removable_fixture_repository(
    path: &Path,
    worktree: &Path,
    whole: bool,
) -> Result<bool, CleanupError> {
    let repository = path
        .parent()
        .ok_or(CleanupError::Conflict("nested repository has no parent"))?;
    if !whole || !repository.starts_with(worktree.join("tmp")) {
        return Ok(false);
    }
    let relative = repository
        .strip_prefix(worktree)
        .map_err(|_| CleanupError::Conflict("nested repository outside worktree"))?
        .to_str()
        .ok_or(CleanupError::Conflict("non-UTF8 nested repository path"))?;
    Ok(
        git_output(worktree, &["ls-files", "--", relative])?.is_empty()
            && ignored(worktree, relative)?,
    )
}

pub(crate) fn remove(
    identity: &WorktreeIdentity,
    path: &Path,
    whole: bool,
) -> Result<(), CleanupError> {
    verify_parents(&identity.path, path)?;
    inspect_tree(path, identity.device, &identity.path, whole)?;
    if whole {
        let path = path
            .to_str()
            .ok_or(CleanupError::Conflict("non-UTF8 worktree path"))?;
        if !git_status(
            &identity.path,
            &["worktree", "remove", "--force", "--", path],
        )?
        .success()
        {
            return Err(CleanupError::Conflict("git worktree removal failed"));
        }
    } else if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn git_command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .arg("-C")
        .arg(dir)
        .args(args);
    command
}

fn git_output(dir: &Path, args: &[&str]) -> Result<Vec<u8>, CleanupError> {
    let output = git_command(dir, args).output()?;
    if !output.status.success() {
        return Err(CleanupError::Conflict("cleanup git inspection failed"));
    }
    Ok(output.stdout)
}

fn git_status(dir: &Path, args: &[&str]) -> Result<std::process::ExitStatus, CleanupError> {
    Ok(git_command(dir, args).output()?.status)
}

fn ignored(root: &Path, relative: &str) -> Result<bool, CleanupError> {
    // check-ignore takes literal pathnames, not pathspecs; forcing literal
    // pathspec magic is an error for this command.
    let status = git_command(root, &["check-ignore", "--quiet", "--", relative])
        .env_remove("GIT_LITERAL_PATHSPECS")
        .output()?
        .status;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(CleanupError::Conflict("cleanup ignore inspection failed")),
    }
}
