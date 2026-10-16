//! Repinning never substitutes for exit reconciliation or process ownership.
use super::pin_adoption;
use crate::model::StateError;
use rusqlite::Connection;

pub(crate) fn require_idle(db: &Connection, task: &str) -> Result<String, StateError> {
    let phase = pin_adoption::require_quiescent(db, task).map_err(|error| match error {
        StateError::PinAdoptionRefused(reason) => StateError::PinReplacementRefused(reason),
        error => error,
    })?;
    if !matches!(
        phase.as_str(),
        "attention" | "paused" | "held" | "pr_complete"
    ) {
        return Err(StateError::PinReplacementRefused("task is not continuable"));
    }
    require_absent_processes(db, task)?;
    Ok(phase)
}

#[cfg(unix)]
fn require_absent_processes(db: &Connection, task: &str) -> Result<(), StateError> {
    use crate::model::{ChildIdentity, ProcessIdentity};
    let rows: Vec<(String, String)> = db.prepare(
        "SELECT kind,payload FROM evidence WHERE task_id=?1 AND kind IN ('child_registered','supervisor_ready','tracked_descendant') ORDER BY sequence",
    )?.query_map([task], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
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
            return Err(StateError::PinReplacementRefused(
                "registered process or process group is present or its absence is unproven",
            ));
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
fn require_absent_processes(_: &Connection, _: &str) -> Result<(), StateError> {
    Err(StateError::PinReplacementRefused(
        "process inspection unsupported",
    ))
}
