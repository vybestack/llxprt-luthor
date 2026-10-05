fn verify_supervisor_gate(db: &Connection, plan: &LaunchPlan) -> Result<(), SupervisorError> {
    let (ready, release): (String, String) = db.query_row(
        "SELECT e.payload,i.detail FROM evidence e JOIN intents i ON i.task_id=e.task_id AND i.attempt_id=e.attempt_id
         WHERE e.task_id=?1 AND e.attempt_id=?2 AND e.kind='supervisor_ready' AND i.kind='gate_release'",
        rusqlite::params![plan.task_id, plan.attempt_id], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let process = super::recorded_process(&ready).ok_or(SupervisorError::Conflict)?;
    if process.pid != std::process::id()
        || super::identity(process.pid)?
            != (
                process.boot_identity.clone(),
                process.start_identity.clone(),
            )
        || super::recorded_process(&release).as_ref() != Some(&process)
    {
        return Err(SupervisorError::Conflict);
    }
    Ok(())
}

pub(crate) fn verify_gate_release(
    attempts: &Path,
    plan: &LaunchPlan,
    child: &super::ChildIdentity,
) -> Result<(), SupervisorError> {
    let root = attempts.parent().ok_or(SupervisorError::Conflict)?;
    let db =
        Connection::open_with_flags(root.join("state.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tx = db.unchecked_transaction()?;
    let (registered, released): (usize, usize) = tx.query_row(
        "SELECT (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2
            AND kind='child_registered' AND payload=?3),
            (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_release')",
        rusqlite::params![plan.task_id, plan.attempt_id, serde_json::to_string(child)?],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if registered != 1 || released != 1 {
        return Err(SupervisorError::Conflict);
    }
    super::binding::verify_in_snapshot(&tx, root, plan, true)?;
    if super::binding::has_amendment(&tx, plan)? {
        verify_supervisor_gate(&tx, plan)?;
    }
    verify_launch_worktree(&tx, plan)?;
    tx.commit()?;
    Ok(())
}

use super::{LaunchPlan, SessionEnvironment, SupervisorError};
use crate::{
    state::{SelectionEvidence, WorktreeIntent, WorktreeRecord},
    worktree,
};
use rusqlite::{Connection, OpenFlags};
use std::{fs, io::Read, os::unix::process::CommandExt, path::Path, process::Command};

fn verify_registered_worker(
    db: &Connection,
    path: &Path,
    plan: &LaunchPlan,
) -> Result<(), SupervisorError> {
    use super::{ChildIdentity, identity, private_bytes, recorded_process};
    let attempts = path.parent().ok_or(SupervisorError::Conflict)?;
    if path.file_name().and_then(|n| n.to_str()) != Some(&format!("{}.plan.json", plan.attempt_id))
        || private_bytes(path)
            .and_then(|bytes| serde_json::from_slice::<LaunchPlan>(&bytes).ok())
            .as_ref()
            != Some(plan)
    {
        return Err(SupervisorError::Conflict);
    }
    let (child, ready, release): (String, String, String) = db.query_row(
        "SELECT e.payload,s.payload,i.detail FROM evidence e JOIN evidence s
         ON s.task_id=e.task_id AND s.attempt_id=e.attempt_id AND s.kind='supervisor_ready'
         JOIN intents i ON i.task_id=e.task_id AND i.attempt_id=e.attempt_id AND i.kind='gate_release'
         WHERE e.task_id=?1 AND e.attempt_id=?2 AND e.kind='child_registered'
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='child_registered')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='supervisor_ready')=1
         AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_release')=1",
        rusqlite::params![plan.task_id, plan.attempt_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let registered: ChildIdentity = serde_json::from_str(&child)?;
    let supervisor = recorded_process(&ready).ok_or(SupervisorError::Conflict)?;
    let file: ChildIdentity =
        private_bytes(&attempts.join(format!("{}.child.json", plan.attempt_id)))
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(SupervisorError::Conflict)?;
    if registered != file
        || registered.pid != std::process::id()
        || registered.group_id != registered.pid
        || unsafe { libc::getpgrp() } != registered.pid as i32
        || identity(registered.pid)? != (registered.boot_identity, registered.start_identity)
        || recorded_process(&release).as_ref() != Some(&supervisor)
        || supervisor.pid == registered.pid
        || unsafe { libc::getppid() } != supervisor.pid as i32
        || identity(supervisor.pid)? != (supervisor.boot_identity, supervisor.start_identity)
    {
        return Err(SupervisorError::Conflict);
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn verify_launch_worktree(
    connection: &Connection,
    plan: &LaunchPlan,
) -> Result<(), SupervisorError> {
    let (selection, intent, identity): (String, String, String) = connection.query_row(
        "SELECT (SELECT payload FROM evidence WHERE task_id=?1 AND kind='selection'),
                (SELECT detail FROM intents WHERE task_id=?1 AND kind='worktree_create'),
                (SELECT payload FROM evidence WHERE task_id=?1 AND kind='worktree_created')",
        [plan.task_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let selection: SelectionEvidence = serde_json::from_str(&selection)?;
    let record = WorktreeRecord {
        intent: serde_json::from_str::<WorktreeIntent>(&intent)?,
        identity: Some(serde_json::from_str(&identity)?),
    };
    if crate::state::attempt_selection(connection, plan).is_err()
        || plan.worktree != plan.expected_worktree.path
    {
        return Err(SupervisorError::Conflict);
    }
    let first: bool = connection.query_row(
        "SELECT id=?2 FROM attempts WHERE task_id=?1 ORDER BY rowid LIMIT 1",
        rusqlite::params![plan.task_id, plan.attempt_id],
        |row| row.get(0),
    )?;
    if first {
        if record.identity.as_ref() != Some(&plan.expected_worktree) {
            return Err(SupervisorError::Conflict);
        }
        worktree::verify_never_dispatched(
            &record,
            &selection.candidate.mapping,
            &selection.effective_config.worktree_root,
            &plan.task_id,
        )?;
    } else if !worktree::matches_snapshot(
        &plan.expected_worktree,
        &worktree::verify_record(
            &record,
            &selection.candidate.mapping,
            &selection.effective_config.worktree_root,
            &plan.task_id,
        )?,
    )? {
        return Err(SupervisorError::Conflict);
    }
    Ok(())
}

#[cfg(unix)]
pub fn worker_gate(plan_path: &Path) -> Result<(), SupervisorError> {
    let plan: LaunchPlan = serde_json::from_slice(&fs::read(plan_path)?)?;
    let mut byte = [0];
    std::io::stdin()
        .read_exact(&mut byte)
        .map_err(|_| SupervisorError::GateClosed)?;
    if byte != *b"R" {
        return Err(SupervisorError::GateClosed);
    }
    worktree::verify_snapshot(&plan.expected_worktree)?;
    let db = plan_path
        .parent()
        .ok_or(SupervisorError::Conflict)?
        .parent()
        .ok_or(SupervisorError::Conflict)?
        .join("state.sqlite3");
    if plan_path
        .parent()
        .is_some_and(|dir| dir.file_name() == Some(std::ffi::OsStr::new("attempts")))
    {
        let connection = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let root = plan_path
            .parent()
            .and_then(Path::parent)
            .ok_or(SupervisorError::Conflict)?;
        let tx = connection.unchecked_transaction()?;
        let amended =
            super::binding_gate::verify(&tx, root, &plan).map_err(|_| SupervisorError::Conflict)?;
        if amended {
            verify_registered_worker(&tx, plan_path, &plan)?;
        }
        verify_launch_worktree(&tx, &plan)?;
        tx.commit()?;
    }
    let mut command = Command::new(&plan.executable);
    command.args(&plan.args).current_dir(&plan.worktree);
    configure_session(&mut command, &plan.session_environment);
    Err(command.exec().into())
}

#[cfg(unix)]
pub(crate) fn configure_session(command: &mut Command, session: &SessionEnvironment) {
    command.env("HOME", &session.home);
    for (name, value) in [
        ("XDG_CONFIG_HOME", &session.xdg_config_home),
        ("XDG_DATA_HOME", &session.xdg_data_home),
        ("XDG_STATE_HOME", &session.xdg_state_home),
        ("LLXPRT_CONFIG_HOME", &session.llxprt_config_home),
    ] {
        if let Some(value) = value {
            command.env(name, value);
        } else {
            command.env_remove(name);
        }
    }
}
