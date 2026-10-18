//! Conservative OS absence checks shared by explicit quiescent operations.
#[cfg(unix)]
use crate::model::ProcessIdentity;
use crate::model::StateError;
use rusqlite::Connection;

#[cfg(all(test, unix))]
mod tests;

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
    require_absent_with(db, task, attempt, &OsProbe)
}

#[cfg(unix)]
fn require_absent_with(
    db: &Connection,
    task: &str,
    attempt: Option<&str>,
    probe: &impl Probe,
) -> Result<(), StateError> {
    use crate::model::{ChildIdentity, ProcessIdentity};
    let rows: Vec<(String, String)> = db.prepare(
        "SELECT kind,payload FROM evidence WHERE task_id=?1 AND (?2 IS NULL OR attempt_id IS NOT ?2) AND kind IN ('child_registered','supervisor_ready','tracked_descendant') ORDER BY sequence",
    )?.query_map(rusqlite::params![task, attempt], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
    for (kind, payload) in rows {
        let (process, group) = if kind == "child_registered" {
            let child: ChildIdentity = serde_json::from_str(&payload)?;
            (
                ProcessIdentity {
                    pid: child.pid,
                    boot_identity: child.boot_identity,
                    start_identity: child.start_identity,
                },
                child.group_id,
            )
        } else {
            let process: ProcessIdentity = serde_json::from_str(&payload)?;
            let group = process.pid;
            (process, group)
        };
        // Identity reuse proves absence only with independent group proof.
        if !registration_absent(&process, group, probe) {
            return Err(StateError::LaunchBlocked);
        }
    }
    Ok(())
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

#[cfg(unix)]
trait Probe {
    fn identity(&self, pid: u32) -> Option<(String, String)>;
    fn absent(&self, pid: u32, group: bool) -> bool;
    fn group_leader(&self, pid: u32) -> bool;
}

#[cfg(unix)]
struct OsProbe;

#[cfg(unix)]
impl Probe for OsProbe {
    fn identity(&self, pid: u32) -> Option<(String, String)> {
        // Permission/platform failures are uncertainty, never absence.
        crate::supervisor::identity(pid).ok()
    }

    fn absent(&self, pid: u32, group: bool) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 0 || unsafe { libc::kill(if group { -pid } else { pid }, 0) } == 0 {
            return false;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    fn group_leader(&self, pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        pid > 0 && unsafe { libc::getpgid(pid) } == pid
    }
}

/// Unknown/historical boot encodings cannot establish a different boot. In
/// particular Darwin's old `{ sec ... }` boottime is not a boot-session UUID.
#[cfg(unix)]
fn known_boot(boot: &str) -> bool {
    let uuid = boot.strip_prefix("darwin-bootsessionuuid:").unwrap_or(boot);
    uuid.len() == 36
        && uuid.bytes().enumerate().all(|(i, byte)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
        && !uuid.bytes().all(|byte| byte == b'0' || byte == b'-')
}

#[cfg(unix)]
fn registration_absent(process: &ProcessIdentity, group: u32, probe: &impl Probe) -> bool {
    if process.pid == 0
        || group == 0
        || i32::try_from(process.pid).is_err()
        || i32::try_from(group).is_err()
        || process.boot_identity.trim().is_empty()
        || process.start_identity.trim().is_empty()
    {
        return false;
    }
    let boot_known = known_boot(&process.boot_identity);
    if boot_known
        && probe.identity(std::process::id()).is_some_and(|(boot, _)| {
            known_boot(&boot) && !boot.eq_ignore_ascii_case(&process.boot_identity)
        })
    {
        // A reboot also eliminates leaderless groups and all their members.
        return true;
    }
    let current = probe.identity(process.pid);
    let unrelated = current.as_ref().is_some_and(|(boot, start)| {
        !start.trim().is_empty()
            && (start != &process.start_identity
                || (boot_known
                    && known_boot(boot)
                    && !boot.eq_ignore_ascii_case(&process.boot_identity)))
    });
    if !probe.absent(process.pid, false) && !unrelated {
        return false;
    }
    if probe.absent(group, true) {
        return true;
    }
    // A missing/reused PID alone says nothing about surviving group members.
    // Unix reserves an extant process-group ID against PID allocation. Only a
    // *new leader of that same group* proves the whole group was recycled. If
    // the leader is gone, outside the group, or unreadable, members remain
    // unproven and block. No such proof is available for a non-leader record.
    group == process.pid
        && unrelated
        && probe.group_leader(group)
        && probe.identity(group) == current
}
