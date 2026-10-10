#[cfg(not(unix))]
use super::runner::run_gated_child;
use super::{
    binding, error::SupervisorError, processes::valid_attempt, storage::write_private_json,
};
#[cfg(unix)]
use super::{
    runner::run_gated_child_control, storage::stop_socket, worker::verify_launch_worktree,
};
#[cfg(unix)]
use crate::ownership::{WorktreeOwner, WorktreeOwnerInternal};
use crate::{model::LaunchPlan, state::WorktreeIdentity, worktree};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
#[cfg(unix)]
use std::{env, os::unix::net::UnixListener};
use std::{fs, path::Path};
/// The child opens SQLite read-only, without taking the coordinator's process lock.
/// It verifies the exact launch plan, reservation and verified claim/worktree before READY.
pub fn supervise(root: &Path, attempt: &str) -> Result<(), SupervisorError> {
    if !valid_attempt(attempt) {
        return Err(SupervisorError::Conflict);
    }
    let attempts = root.join("attempts");
    let result = (|| {
        let plan: LaunchPlan =
            serde_json::from_slice(&fs::read(attempts.join(format!("{attempt}.plan.json")))?)?;
        if plan.attempt_id != attempt
            || plan.session_id != plan.task_id
            || plan.config_revision.is_empty()
        {
            return Err(SupervisorError::Conflict);
        }
        verify_supervision_plan(root, attempt, &plan)?;
        #[cfg(unix)]
        let _ownership =
            WorktreeOwner::inherited(root, &plan.task_id).map_err(|_| SupervisorError::Conflict)?;
        #[cfg(unix)]
        {
            let db = Connection::open_with_flags(
                root.join("state.sqlite3"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            _ownership
                .verify_protocol(&db, root, &plan.task_id, attempt)
                .map_err(|_| SupervisorError::Conflict)?;
        }
        #[cfg(unix)]
        let listener = {
            let socket = stop_socket(&attempts, attempt);
            let listener = UnixListener::bind(socket)?;
            listener.set_nonblocking(true)?;
            listener
        };
        #[cfg(unix)]
        run_gated_child_control(
            &plan,
            std::io::stdin(),
            &attempts,
            Some(&listener),
            &env::current_exe()?,
            |out, err| (out, err),
            &_ownership,
        )?;
        #[cfg(not(unix))]
        run_gated_child(&plan, std::io::stdin(), &attempts)?;
        Ok(())
    })();
    if let Err(error) = &result {
        // The coordinator retains the reservation whether the gate closed or the worker failed.
        let _ = write_private_json(
            &attempts.join(format!("{attempt}.supervisor-error.json")),
            &serde_json::json!({"attempt_id":attempt,"error":error.to_string()}),
        );
    }
    result
}

fn verify_supervision_plan(
    root: &Path,
    attempt: &str,
    plan: &LaunchPlan,
) -> Result<(), SupervisorError> {
    let connection =
        Connection::open_with_flags(root.join("state.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let row: Option<(String, String)> = connection.query_row(
            "SELECT i.detail, (SELECT payload FROM evidence WHERE task_id=?2 AND kind='worktree_created')
             FROM intents i JOIN attempts a ON a.id=i.attempt_id
             JOIN reservations r ON r.attempt_id=a.id JOIN tasks t ON t.id=a.task_id
             WHERE i.kind='launch' AND i.attempt_id=?1 AND i.task_id=?2
               AND r.task_id=?2 AND r.status='reserved' AND t.state='held'
               AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND kind='supervisor_dispatch')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND kind='claim_verified')=1
               AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND kind='worktree_created')=1",
            params![attempt, plan.task_id], |row| Ok((row.get(0)?, row.get(1)?))
        ).optional()?;
    let (_, worktree) = row.ok_or(SupervisorError::Conflict)?;
    binding::verify_worker_plan(&connection, root, plan)?;
    let identity: WorktreeIdentity = serde_json::from_str(&worktree)?;
    #[cfg(unix)]
    verify_launch_worktree(&connection, plan)?;
    if !worktree::matches_snapshot(&identity, &plan.expected_worktree)?
        || identity.path != plan.worktree
        || fs::canonicalize(&plan.worktree)? != plan.worktree
    {
        return Err(SupervisorError::Conflict);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(&plan.worktree)?;
        if metadata.dev() != identity.device || metadata.ino() != identity.inode {
            return Err(SupervisorError::Conflict);
        }
    }
    Ok(())
}
