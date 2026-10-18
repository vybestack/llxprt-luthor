//! Observe the exact persisted owner identity without changing its live lock.
use super::*;
use crate::model::StateError;

pub(crate) fn verify_protocol(
    db: &Connection,
    root: &Path,
    task: &str,
    attempt: &str,
) -> Result<(), StateError> {
    let path = lock_path(root, task).map_err(|_| StateError::LaunchBlocked)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let owner = WorktreeOwner { file };
    let observed = owner
        .protocol_evidence(root, task, attempt)
        .map_err(|_| StateError::LaunchBlocked)?;
    let payloads: Vec<(String, String)> = db
        .prepare(
            "SELECT task_id,payload FROM evidence WHERE kind='worktree_owner_protocol' AND attempt_id=?1",
        )?
        .query_map([attempt], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let [(saved_task, payload)] = payloads.as_slice() else {
        return Err(StateError::LaunchBlocked);
    };
    let saved: WorktreeOwnerProtocol = serde_json::from_str(payload)?;
    if saved_task != task || saved != observed || !saved.matches_attempt(task, attempt) {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
