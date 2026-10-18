//! Conservative OS absence checks shared by explicit quiescent operations.
use crate::model::StateError;
use rusqlite::Connection;

#[cfg(unix)]
pub(crate) fn require_absent_processes(db: &Connection, task: &str) -> Result<(), StateError> {
    require_absent_except(db, task, None)
}

/// A resize may observe one confirmed live attempt, never ignore registrations
/// from another attempt (including task-scoped or malformed registrations).
#[cfg(unix)]
pub(crate) fn require_absent_except(
    db: &Connection,
    task: &str,
    attempt: Option<&str>,
) -> Result<(), StateError> {
    use crate::model::{ChildIdentity, ProcessIdentity};
    let rows: Vec<(String, String)> = db.prepare(
        "SELECT kind,payload FROM evidence WHERE task_id=?1 AND (?2 IS NULL OR attempt_id IS NOT ?2) AND kind IN ('child_registered','supervisor_ready','tracked_descendant') ORDER BY sequence",
    )?.query_map(rusqlite::params![task, attempt], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
    for (kind, payload) in rows {
        let (pid, group) = if kind == "child_registered" {
            let child: ChildIdentity = serde_json::from_str(&payload)?;
            (child.pid, child.group_id)
        } else {
            let process: ProcessIdentity = serde_json::from_str(&payload)?;
            (process.pid, process.pid)
        };
        // Even a reused PID is not authorization to ignore a registration.
        if !absent(pid, false) || !absent(group, true) {
            return Err(StateError::LaunchBlocked);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn absent(pid: u32, group: bool) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 || unsafe { libc::kill(if group { -pid } else { pid }, 0) } == 0 {
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
pub(crate) fn require_absent_processes(_: &Connection, _: &str) -> Result<(), StateError> {
    Err(StateError::LaunchBlocked)
}

#[cfg(not(unix))]
pub(crate) fn require_absent_except(
    _: &Connection,
    _: &str,
    _: Option<&str>,
) -> Result<(), StateError> {
    Err(StateError::LaunchBlocked)
}
