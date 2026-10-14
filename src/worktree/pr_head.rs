//! Adopt only first-parent extensions made entirely of configured-base merges.
use super::{WorktreeError, git::*, verify_existing_worktree};
use crate::state::{StateStore, task_records};
use std::{fs, path::Path};

/// The caller holds the task owner and has freshly verified PR, claim and exit
/// receipts. Fetch into task-private refs, prove lineage, then fast-forward only
/// a clean, still-identical worktree. No operator checkout branch is changed.
pub(crate) fn synchronize(
    store: &StateStore,
    task: &str,
    recorded: &str,
    head: &str,
) -> Result<serde_json::Value, WorktreeError> {
    require_commit_ids(&[recorded, head])?;
    let before = verify_existing_worktree(store, task)?;
    if !fs::symlink_metadata(before.path.join(".git"))?.is_file() {
        return Err(WorktreeError::Conflict("PR worktree has no .git file"));
    }
    if recorded == head {
        if before.head != head {
            return Err(WorktreeError::Conflict("PR head differs from local tip"));
        }
        return Ok(serde_json::json!({"head":head,"local_before":before.head}));
    }
    require_clean(&before.path)?;
    let selection = task_records::selection_evidence(store, task)?
        .ok_or(WorktreeError::Conflict("missing selection"))?;
    let mapping = &selection.candidate.mapping;
    let (_, _, _) = validate_checkout(mapping)?;
    let origin = read_git(&mapping.checkout, &["remote", "get-url", "origin"])?;
    let base = fetch_lineage(
        &before,
        task,
        (&format!("refs/heads/{}", before.branch), &before.remote),
        &origin,
        head,
    )?;
    let merges = base_merges(&before.path, recorded, head, &base)?;
    if before.head != recorded && !merges.contains(&before.head) {
        return Err(WorktreeError::Conflict(
            "local tip is not in verified PR lineage",
        ));
    }
    fast_forward(store, task, &before, head)?;
    Ok(
        serde_json::json!({"recorded_head":recorded,"head":head,"local_before":before.head,"base":base,"merges":merges}),
    )
}

/// Read-only lineage proof for a PR that GitHub already merged. The head branch
/// may be deleted by then, so the head comes from the PR's own ref on the code
/// repository. Nothing in the task worktree is changed.
pub(crate) fn verify_merged_lineage(
    store: &StateStore,
    task: &str,
    recorded: &str,
    head: &str,
    pull_number: u64,
) -> Result<serde_json::Value, WorktreeError> {
    require_commit_ids(&[recorded, head])?;
    let worktree = verify_existing_worktree(store, task)?;
    let selection = task_records::selection_evidence(store, task)?
        .ok_or(WorktreeError::Conflict("missing selection"))?;
    let mapping = &selection.candidate.mapping;
    let (_, _, _) = validate_checkout(mapping)?;
    let origin = read_git(&mapping.checkout, &["remote", "get-url", "origin"])?;
    let base = fetch_lineage(
        &worktree,
        task,
        (&format!("refs/pull/{pull_number}/head"), &origin),
        &origin,
        head,
    )?;
    let merges = base_merges(&worktree.path, recorded, head, &base)?;
    Ok(serde_json::json!({"recorded_head":recorded,"head":head,"base":base,"merges":merges}))
}

/// Fetches the PR head and the configured base into task-private refs and
/// returns the base tip. The base belongs to the code repository, not a
/// permitted fork, so it always comes from the checkout's `origin`.
fn fetch_lineage(
    worktree: &crate::state::WorktreeIdentity,
    task: &str,
    (head_source, head_remote): (&str, &str),
    origin: &str,
    head: &str,
) -> Result<String, WorktreeError> {
    let head_ref = format!("refs/luthor/pr-heads/{task}");
    let base_ref = format!("refs/luthor/pr-bases/{task}");
    fetch(&worktree.path, head_remote, head_source, &head_ref)?;
    fetch(
        &worktree.path,
        origin,
        &format!("refs/heads/{}", worktree.base),
        &base_ref,
    )?;
    if read_git(&worktree.path, &["rev-parse", &head_ref])? != head {
        return Err(WorktreeError::Conflict("fetched PR head changed"));
    }
    read_git(&worktree.path, &["rev-parse", &base_ref])
}

fn require_commit_ids(ids: &[&str]) -> Result<(), WorktreeError> {
    if ids
        .iter()
        .all(|sha| matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Ok(());
    }
    Err(WorktreeError::Conflict("invalid PR commit SHA"))
}

fn require_clean(path: &Path) -> Result<(), WorktreeError> {
    if !fs::symlink_metadata(path.join(".git"))?.is_file()
        || !read_git(path, &["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty()
    {
        return Err(WorktreeError::Conflict(
            "PR worktree is dirty or has no .git file",
        ));
    }
    Ok(())
}

fn fetch(path: &Path, remote: &str, source: &str, target: &str) -> Result<(), WorktreeError> {
    let refspec = format!("+{source}:{target}");
    if !git(
        path,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--",
            remote,
            &refspec,
        ],
    )?
    .status
    .success()
    {
        return Err(WorktreeError::Git("PR lineage fetch failed"));
    }
    Ok(())
}

fn base_merges(
    path: &Path,
    old: &str,
    head: &str,
    base: &str,
) -> Result<Vec<String>, WorktreeError> {
    let mut cursor = head.to_owned();
    let mut merges = Vec::new();
    while cursor != old {
        let parents = read_git(path, &["rev-list", "--parents", "-n", "1", &cursor])?;
        let parts = parents.split_whitespace().collect::<Vec<_>>();
        let [commit, first, second] = parts.as_slice() else {
            return Err(WorktreeError::Conflict(
                "PR advanced by a non-base-merge commit",
            ));
        };
        let status = git(path, &["merge-base", "--is-ancestor", second, base])?.status;
        match status.code() {
            Some(0) => {}
            Some(1) => {
                return Err(WorktreeError::Conflict(
                    "PR merge parent is not on configured base",
                ));
            }
            _ => return Err(WorktreeError::Git("cannot verify PR merge parent")),
        }
        merges.push((*commit).into());
        cursor = (*first).into();
    }
    Ok(merges)
}

fn fast_forward(
    store: &StateStore,
    task: &str,
    before: &crate::state::WorktreeIdentity,
    head: &str,
) -> Result<(), WorktreeError> {
    if verify_existing_worktree(store, task)? != *before {
        return Err(WorktreeError::Conflict("worktree changed during PR fetch"));
    }
    require_clean(&before.path)?;
    if before.head != head
        && !git(&before.path, &["merge", "--ff-only", "--", head])?
            .status
            .success()
    {
        return Err(WorktreeError::Git("PR fast-forward failed"));
    }
    let after = verify_existing_worktree(store, task)?;
    let mut expected = before.clone();
    expected.head = head.into();
    if after != expected {
        return Err(WorktreeError::Conflict(
            "worktree changed during PR fast-forward",
        ));
    }
    require_clean(&after.path)?;
    Ok(())
}
