use super::error::SupervisorError;
use crate::model::*;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::{fs, path::Path};

#[cfg(any(test, target_os = "macos"))]
pub(crate) fn darwin_boot_identity(bytes: &[u8]) -> Result<String, SupervisorError> {
    let uuid = std::str::from_utf8(bytes)
        .map_err(|_| SupervisorError::IdentityUnavailable)?
        .trim();
    if uuid.len() != 36
        || !uuid.bytes().enumerate().all(|(i, byte)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
        || uuid.bytes().all(|byte| byte == b'0' || byte == b'-')
    {
        return Err(SupervisorError::IdentityUnavailable);
    }
    Ok(format!(
        "darwin-bootsessionuuid:{}",
        uuid.to_ascii_lowercase()
    ))
}

#[cfg(target_os = "macos")]
pub(crate) fn identity(pid: u32) -> Result<(String, String), SupervisorError> {
    let output = Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.bootsessionuuid"])
        .output()?;
    if !output.status.success() {
        return Err(SupervisorError::IdentityUnavailable);
    }
    let boot = darwin_boot_identity(&output.stdout)?;
    let pid = i32::try_from(pid).map_err(|_| SupervisorError::IdentityUnavailable)?;
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    if unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) }
        != size
        || info.pbi_pid != pid as u32
        || info.pbi_start_tvsec == 0
    {
        return Err(SupervisorError::IdentityUnavailable);
    }
    Ok((
        boot,
        format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
    ))
}

#[cfg(target_os = "linux")]
pub(crate) fn identity(pid: u32) -> Result<(String, String), SupervisorError> {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let process = crate::platform::observe_linux_process(pid)?;
    Ok((boot.trim().to_owned(), process.start_time_ticks))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn identity(_pid: u32) -> Result<(String, String), SupervisorError> {
    Err(SupervisorError::IdentityUnavailable)
}

#[cfg(target_os = "macos")]
pub(crate) fn zombie(pid: u32) -> bool {
    let Ok(output) = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    output.status.success() && output.stdout.first() == Some(&b'Z')
}

#[cfg(target_os = "linux")]
pub(crate) fn zombie(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().next())
        == Some("Z")
}

#[cfg(unix)]
pub(crate) fn group_absent(pid: i32) -> bool {
    if unsafe { libc::kill(-pid, 0) } == 0 {
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
pub(crate) fn matching_child(child: &ChildIdentity) -> bool {
    let Ok(pid) = i32::try_from(child.pid) else {
        return false;
    };
    child.pid != 0
        && child.group_id == child.pid
        && !child.boot_identity.trim().is_empty()
        && !child.start_identity.trim().is_empty()
        && identity(child.pid).ok().as_ref()
            == Some(&(child.boot_identity.clone(), child.start_identity.clone()))
        && unsafe { libc::getpgid(pid) } == pid
}

/// Proves the registered direct processes and their process groups are absent.
/// Recorded known descendants are checked individually. This does not prove
/// that a deliberately untracked descendant escaped into another group or
/// session is absent; that remains an out-of-scope cooperative constraint.
#[cfg(unix)]
pub(crate) fn registered_processes_absent(
    child: &ChildIdentity,
    supervisor: &ProcessIdentity,
    tracked: &[ProcessIdentity],
) -> bool {
    registered_process_absence(child, supervisor, tracked).is_ok()
}

#[cfg(unix)]
pub(crate) fn registered_process_absence(
    child: &ChildIdentity,
    supervisor: &ProcessIdentity,
    tracked: &[ProcessIdentity],
) -> Result<(), &'static str> {
    registered_process_absence_with(
        child,
        supervisor,
        tracked,
        || identity(std::process::id()).map(|(boot, _)| boot),
        |pid| {
            if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
                return false;
            }
            std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        },
        group_absent,
    )
}

#[cfg(unix)]
pub(crate) fn registered_process_absence_with(
    child: &ChildIdentity,
    supervisor: &ProcessIdentity,
    tracked: &[ProcessIdentity],
    current_boot: impl FnOnce() -> Result<String, SupervisorError>,
    pid_absent: impl FnMut(u32) -> bool,
    group_absent: impl FnMut(i32) -> bool,
) -> Result<(), &'static str> {
    validate_registered_identities(child, supervisor, tracked)?;
    // Historical boottime contains no boot-session UUID to compare. Neither
    // equal seconds nor present-day ESRCH probes can supply that missing link.
    if child.boot_identity.starts_with("{ sec") {
        return Err("historical Darwin boot identity cannot prove boot continuity");
    }
    let current = current_boot().map_err(|_| "current boot identity is unavailable")?;
    if current != child.boot_identity {
        return Err("registered boot identity differs from current boot");
    }
    probe_registered_absence(child, supervisor, tracked, pid_absent, group_absent)
}

#[cfg(unix)]
pub(crate) fn probe_registered_absence(
    child: &ChildIdentity,
    supervisor: &ProcessIdentity,
    tracked: &[ProcessIdentity],
    mut pid_absent: impl FnMut(u32) -> bool,
    mut group_absent: impl FnMut(i32) -> bool,
) -> Result<(), &'static str> {
    if !pid_absent(child.pid)
        || !pid_absent(supervisor.pid)
        || !tracked.iter().all(|process| pid_absent(process.pid))
    {
        return Err("registered process is present or its absence is unproven");
    }
    if !group_absent(child.group_id as i32) || !group_absent(supervisor.pid as i32) {
        return Err("registered process group is present or its absence is unproven");
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn verified_live_process(child: &ChildIdentity, supervisor: &ProcessIdentity) -> bool {
    let bounded = |boot: &str, start: &str| {
        !boot.trim().is_empty()
            && boot.len() <= 256
            && !start.trim().is_empty()
            && start.len() <= 64
    };
    child.pid != supervisor.pid
        && bounded(&child.boot_identity, &child.start_identity)
        && bounded(&supervisor.boot_identity, &supervisor.start_identity)
        && matching_child(child)
        && !zombie(child.pid)
        && identity(supervisor.pid).ok().as_ref()
            == Some(&(
                supervisor.boot_identity.clone(),
                supervisor.start_identity.clone(),
            ))
        && !zombie(supervisor.pid)
        && unsafe { libc::getpgid(supervisor.pid as i32) } == supervisor.pid as i32
}

#[cfg(unix)]
pub(crate) fn private_bytes(path: &Path) -> Option<Vec<u8>> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return None;
    }
    fs::read(path).ok()
}

#[cfg(unix)]
pub(crate) fn private_log_size(path: &Path) -> Option<u64> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path).ok()?;
    (metadata.file_type().is_file() && metadata.permissions().mode() & 0o077 == 0)
        .then_some(metadata.len())
}

pub(crate) fn valid_attempt(attempt: &str) -> bool {
    !attempt.is_empty()
        && attempt.len() <= 128
        && attempt
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod darwin_boot_tests {
    use super::*;

    #[test]
    fn bootsession_uuid_is_validated_and_normalized() {
        assert_eq!(
            darwin_boot_identity(b"7379D9DB-543D-4D87-819E-086CEDBF1EF1\n").unwrap(),
            "darwin-bootsessionuuid:7379d9db-543d-4d87-819e-086cedbf1ef1"
        );
        for invalid in [
            &b""[..],
            b" \n",
            b"00000000-0000-0000-0000-000000000000",
            b"7379D9DB543D4D87819E086CEDBF1EF1",
            b"7379D9DB-543D-4D87-819E-086CEDBF1EFG",
            b"7379D9DB-543D-4D87-819E-086CEDBF1EF1\nsecond-line",
            b"7379D9DB-543D-4D87-819E-086CEDBF1EF1\0",
            b"\xff",
            b"{ sec = 1790533213, usec = 220969 } Sun Sep 27 15:20:13 2026",
        ] {
            assert!(matches!(
                darwin_boot_identity(invalid),
                Err(SupervisorError::IdentityUnavailable)
            ));
        }
    }
}

#[cfg(all(test, unix))]
mod registered_absence_tests {
    use super::*;
    use std::cell::RefCell;
    const BOOT: &str = "darwin-bootsessionuuid:7379d9db-543d-4d87-819e-086cedbf1ef1";

    fn records() -> (ChildIdentity, ProcessIdentity, Vec<ProcessIdentity>) {
        (
            ChildIdentity {
                pid: 101,
                group_id: 101,
                boot_identity: BOOT.into(),
                start_identity: "child".into(),
            },
            ProcessIdentity {
                pid: 102,
                boot_identity: BOOT.into(),
                start_identity: "supervisor".into(),
            },
            vec![ProcessIdentity {
                pid: 103,
                boot_identity: BOOT.into(),
                start_identity: "descendant".into(),
            }],
        )
    }

    #[test]
    fn unchanged_uuid_requires_independent_esrch_probes_for_all_registered_pids_and_groups() {
        let (child, supervisor, tracked) = records();
        let probes = RefCell::new(Vec::new());
        assert_eq!(
            registered_process_absence_with(
                &child,
                &supervisor,
                &tracked,
                || Ok(BOOT.into()),
                |pid| {
                    probes.borrow_mut().push(pid as i32);
                    true
                },
                |group| {
                    probes.borrow_mut().push(-group);
                    true
                }
            ),
            Ok(())
        );
        assert_eq!(*probes.borrow(), [101, 102, 103, -101, -102]);
    }

    #[test]
    fn changed_or_unavailable_boot_refuses_before_pid_or_group_probes() {
        let (child, supervisor, tracked) = records();
        for current in [
            Ok("darwin-bootsessionuuid:00000001-0000-4000-8000-000000000001".into()),
            Err(SupervisorError::IdentityUnavailable),
        ] {
            assert!(
                registered_process_absence_with(
                    &child,
                    &supervisor,
                    &tracked,
                    || current,
                    |_| panic!("boot unproven"),
                    |_| panic!("boot unproven")
                )
                .is_err()
            );
        }
    }

    #[test]
    fn historical_boottime_never_proves_continuity_even_with_equal_seconds_or_exact_value() {
        let (mut child, mut supervisor, mut tracked) = records();
        let old = "{ sec = 1790533213, usec = 116017 } Sun Sep 27 15:20:13 2026";
        child.boot_identity = old.into();
        supervisor.boot_identity = old.into();
        tracked[0].boot_identity = old.into();
        for current in [
            old,
            "{ sec = 1790533213, usec = 220969 } Sun Sep 27 15:20:13 2026",
            "{ sec = 1790539999, usec = 116017 } Sun Sep 27 17:13:19 2026",
            BOOT,
        ] {
            assert_eq!(
                registered_process_absence_with(
                    &child,
                    &supervisor,
                    &tracked,
                    || Ok(current.into()),
                    |_| panic!("historical boot unproven"),
                    |_| panic!("historical boot unproven")
                ),
                Err("historical Darwin boot identity cannot prove boot continuity")
            );
        }
    }

    #[test]
    fn live_reused_or_unobservable_pids_and_groups_all_refuse() {
        let (child, supervisor, tracked) = records();
        for present in [101, 102, 103, -101, -102] {
            assert!(
                registered_process_absence_with(
                    &child,
                    &supervisor,
                    &tracked,
                    || Ok(BOOT.into()),
                    |pid| pid as i32 != present,
                    |group| -group != present
                )
                .is_err()
            );
        }
        // Start identity cannot turn a reused PID into absence.
        let mut reused = child.clone();
        reused.start_identity = "old-process-start".into();
        assert!(
            registered_process_absence_with(
                &reused,
                &supervisor,
                &tracked,
                || Ok(BOOT.into()),
                |pid| pid != reused.pid,
                |_| true
            )
            .is_err()
        );
    }

    #[test]
    fn contradictory_or_malformed_records_refuse_before_process_probes() {
        for contradiction in [
            "child_boot",
            "supervisor_boot",
            "tracked_boot",
            "group",
            "pid",
            "start",
        ] {
            let (mut child, mut supervisor, mut tracked) = records();
            match contradiction {
                "child_boot" => child.boot_identity.clear(),
                "supervisor_boot" => supervisor.boot_identity = "another".into(),
                "tracked_boot" => tracked[0].boot_identity = "another".into(),
                "group" => child.group_id = supervisor.pid,
                "pid" => child.pid = supervisor.pid,
                "start" => tracked[0].start_identity.clear(),
                _ => unreachable!(),
            }
            assert!(
                registered_process_absence_with(
                    &child,
                    &supervisor,
                    &tracked,
                    || Ok(BOOT.into()),
                    |_| panic!("contradiction"),
                    |_| panic!("contradiction")
                )
                .is_err(),
                "{contradiction}"
            );
        }
    }
}
