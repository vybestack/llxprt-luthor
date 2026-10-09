use super::{error::SupervisorError, processes::*};
use crate::model::{ChildIdentity, LaunchPlan};
#[cfg(test)]
use crate::model::{ProcessIdentity, recorded_process};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{
    path::Path,
    process::Child,
    thread,
    time::{Duration, Instant},
};
#[cfg(unix)]
pub(crate) fn record_log_failure(
    plan: &LaunchPlan,
    store_root: &Path,
    stream: &str,
    error: &str,
) -> Result<(), SupervisorError> {
    let path = store_root
        .parent()
        .ok_or(SupervisorError::Conflict)?
        .join("state.sqlite3");
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let tx = connection.transaction()?;
    let detail =
        serde_json::json!({"task_id":plan.task_id,"attempt_id":plan.attempt_id}).to_string();
    let persisted: Option<String> = tx
        .query_row(
            "SELECT detail FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop'",
            params![plan.task_id, plan.attempt_id],
            |row| row.get(0),
        )
        .optional()?;
    if persisted.as_ref().is_some_and(|prior| prior != &detail) {
        return Err(SupervisorError::Conflict);
    }
    if persisted.is_none() {
        tx.execute(
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'stop',?4)",
            params![
                format!("stop-{}", plan.attempt_id),
                plan.task_id,
                plan.attempt_id,
                detail
            ],
        )?;
    }
    let payload = serde_json::json!({"stream":stream,"error":error}).to_string();
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'log_failure',?3)",
        params![plan.task_id, plan.attempt_id, payload],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn stop_failed_log_child(
    child: &mut Child,
    registered: &ChildIdentity,
) -> Result<(), SupervisorError> {
    let pid = i32::try_from(registered.pid).map_err(|_| SupervisorError::StopUnavailable)?;
    stop_failed_log_child_with(
        Duration::from_secs(2),
        || matching_child(registered),
        || unsafe { libc::kill(-pid, libc::SIGKILL) } == 0,
        || Ok(child.try_wait()?.is_some()),
        || group_absent(pid),
    )
}

#[cfg(unix)]
fn stop_failed_log_child_with(
    limit: Duration,
    mut matching: impl FnMut() -> bool,
    mut kill_group: impl FnMut() -> bool,
    mut try_wait: impl FnMut() -> Result<bool, SupervisorError>,
    mut absent: impl FnMut() -> bool,
) -> Result<(), SupervisorError> {
    let deadline = Instant::now() + limit;
    loop {
        // Send SIGKILL immediately while the recorded leader still proves group
        // ownership. After it exits, its identity cannot authorize another kill.
        let owned = matching();
        if owned {
            let _ = kill_group();
        }
        let exited = try_wait()?;
        if absent() && exited {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(SupervisorError::StopUnavailable);
        }
        thread::sleep(
            Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(all(test, unix))]
mod failed_log_stop_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn tracked_current_process_prevents_absence_proof() {
        let (boot_identity, start_identity) = identity(std::process::id()).unwrap();
        let child = ChildIdentity {
            pid: u32::MAX - 10,
            boot_identity: boot_identity.clone(),
            start_identity: "synthetic-child-start".to_owned(),
            group_id: u32::MAX - 10,
        };
        let supervisor = ProcessIdentity {
            pid: std::process::id(),
            boot_identity: boot_identity.clone(),
            start_identity,
        };
        let tracked = [ProcessIdentity {
            pid: std::process::id(),
            boot_identity,
            start_identity: "tracked-current-process".to_owned(),
        }];
        assert!(!registered_processes_absent(&child, &supervisor, &tracked));
    }

    #[test]
    fn invalid_tracked_boot_is_rejected_before_absence_checks() {
        let (boot_identity, start_identity) = identity(std::process::id()).unwrap();
        let child = ChildIdentity {
            pid: u32::MAX - 10,
            boot_identity: boot_identity.clone(),
            start_identity: "synthetic-child-start".to_owned(),
            group_id: u32::MAX - 10,
        };
        let supervisor = ProcessIdentity {
            pid: std::process::id(),
            boot_identity,
            start_identity,
        };
        let tracked = [ProcessIdentity {
            pid: u32::MAX - 11,
            boot_identity: "different-boot".to_owned(),
            start_identity: "tracked-start".to_owned(),
        }];
        assert!(!registered_processes_absent(&child, &supervisor, &tracked));
    }

    #[test]
    fn malformed_tracked_process_is_rejected() {
        assert!(recorded_process("not-json").is_none());
    }

    #[test]
    fn leader_exit_before_signal_keeps_group_held_without_signaling_reused_identity() {
        let signals = Cell::new(0);
        let result = stop_failed_log_child_with(
            Duration::ZERO,
            || false,
            || {
                signals.set(signals.get() + 1);
                true
            },
            || Ok(true),
            || false,
        );
        assert!(matches!(result, Err(SupervisorError::StopUnavailable)));
        assert_eq!(signals.get(), 0);
    }

    #[test]
    fn failed_first_signal_retries_only_with_proven_leader_and_requires_group_absence() {
        let signals = Cell::new(0);
        let result = stop_failed_log_child_with(
            Duration::from_millis(100),
            || true,
            || {
                signals.set(signals.get() + 1);
                signals.get() == 2
            },
            || Ok(signals.get() >= 2),
            || signals.get() >= 2,
        );
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(signals.get(), 2);
    }

    #[test]
    fn transient_identity_failure_does_not_abandon_signalable_child() {
        let probes = Cell::new(0);
        let signals = Cell::new(0);
        let result = stop_failed_log_child_with(
            Duration::from_millis(100),
            || {
                probes.set(probes.get() + 1);
                probes.get() > 1
            },
            || {
                signals.set(signals.get() + 1);
                true
            },
            || Ok(signals.get() == 1),
            || signals.get() == 1,
        );
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(signals.get(), 1);
    }

    #[test]
    fn reaped_leader_with_remaining_group_is_never_reported_stopped() {
        let signals = Cell::new(0);
        let result = stop_failed_log_child_with(
            Duration::from_millis(100),
            || signals.get() == 0,
            || {
                signals.set(signals.get() + 1);
                true
            },
            || Ok(true),
            || false,
        );
        assert!(matches!(result, Err(SupervisorError::StopUnavailable)));
        assert_eq!(signals.get(), 1);
    }
}
