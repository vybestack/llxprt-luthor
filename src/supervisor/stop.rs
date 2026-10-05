#[cfg(unix)]
use super::{binding, error::SupervisorError, processes::*, storage::stop_socket};
use crate::state::journal;
#[cfg(unix)]
use crate::{
    model::{ChildIdentity, ProcessIdentity, recorded_process},
    state::StateStore,
};
#[cfg(unix)]
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    thread,
    time::{Duration, Instant},
};
#[cfg(unix)]
fn peer_pid(stream: &UnixStream) -> Result<u32, SupervisorError> {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "macos")]
    {
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&raw mut pid).cast(),
                &raw mut len,
            )
        } != 0
            || len as usize != std::mem::size_of_val(&pid)
            || pid <= 0
        {
            return Err(SupervisorError::StopUnavailable);
        }
        Ok(pid as u32)
    }
    #[cfg(target_os = "linux")]
    {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut cred).cast(),
                &raw mut len,
            )
        } != 0
            || len as usize != std::mem::size_of_val(&cred)
            || cred.pid <= 0
        {
            return Err(SupervisorError::StopUnavailable);
        }
        Ok(cred.pid as u32)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err(SupervisorError::StopUnavailable)
}

/// The coordinator may signal only a registered, currently verified dedicated
/// child group after the recorded supervisor has ceased matching its identity.
#[cfg(unix)]
fn stop_without_supervisor(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    supervisor: &ProcessIdentity,
) -> Result<(), SupervisorError> {
    let child = independently_owned_child(store, task_id, attempt_id, supervisor)?;
    let pid = i32::try_from(child.pid).map_err(|_| SupervisorError::StopUnavailable)?;
    let target = serde_json::json!({"pid":child.pid,"group_id":child.group_id,
        "boot_identity":child.boot_identity,"start_identity":child.start_identity});
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGKILL] {
        // Revalidate the recorded boot, start, and dedicated process group before
        // every escalation. A PID alone is never a safe signal target.
        if !matching_child(&child) {
            return Err(SupervisorError::StopUnavailable);
        }
        let decision = serde_json::json!({"target":target,"signal":signal});
        journal::record_evidence(
            store,
            task_id,
            Some(attempt_id),
            "independent_stop_decision",
            &decision.to_string(),
        )?;
        if !matching_child(&child) {
            return Err(SupervisorError::StopUnavailable);
        }
        let sent = unsafe { libc::kill(-pid, signal) } == 0;
        let result = serde_json::json!({"target":target,"signal":signal,"sent":sent});
        journal::record_evidence(
            store,
            task_id,
            Some(attempt_id),
            "independent_stop_signal",
            &result.to_string(),
        )?;
        if !sent {
            return Err(SupervisorError::StopUnavailable);
        }
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            if group_absent(pid) {
                let absence = serde_json::json!({"target":target,"probe":"kill(-pgid, 0): ESRCH"});
                journal::record_evidence(
                    store,
                    task_id,
                    Some(attempt_id),
                    "independent_group_absent",
                    &absence.to_string(),
                )?;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    Err(SupervisorError::StopUnavailable)
}

/// A durable stop request awaiting the process-identity and signal decision.
#[cfg(unix)]
#[must_use]
pub struct PendingStop<'a> {
    store: &'a mut StateStore,
    task_id: &'a str,
    attempt_id: &'a str,
}

/// Commit the intent separately so exit between intent and signaling is testable.
#[cfg(unix)]
pub fn prepare_stop<'a>(
    store: &'a mut StateStore,
    task_id: &'a str,
    attempt_id: &'a str,
) -> Result<PendingStop<'a>, SupervisorError> {
    if !valid_attempt(attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    journal::record_stop_intent(store, task_id, attempt_id)?;
    Ok(PendingStop {
        store,
        task_id,
        attempt_id,
    })
}

/// Persist the request before contacting the supervisor.
#[cfg(unix)]
pub fn request_stop(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<(), SupervisorError> {
    prepare_stop(store, task_id, attempt_id)?.finish()
}

#[cfg(unix)]
impl PendingStop<'_> {
    pub fn finish(self) -> Result<(), SupervisorError> {
        let Self {
            store,
            task_id,
            attempt_id,
        } = self;
        let recorded =
            journal::evidence_payload(store, task_id, Some(attempt_id), "supervisor_ready")?
                .as_deref()
                .and_then(recorded_process)
                .ok_or(SupervisorError::StopUnavailable)?;
        match identity(recorded.pid) {
            Ok((boot, start))
                if boot == recorded.boot_identity && start == recorded.start_identity =>
            {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                if zombie(recorded.pid) {
                    return stop_without_supervisor(store, task_id, attempt_id, &recorded);
                }
            }
            Ok(_) => return stop_without_supervisor(store, task_id, attempt_id, &recorded),
            Err(_) => {
                let pid =
                    i32::try_from(recorded.pid).map_err(|_| SupervisorError::StopUnavailable)?;
                // A failed identity lookup alone is not proof of death.
                if unsafe { libc::kill(pid, 0) } == 0 {
                    #[cfg(any(target_os = "macos", target_os = "linux"))]
                    if zombie(recorded.pid) {
                        return stop_without_supervisor(store, task_id, attempt_id, &recorded);
                    }
                    return Err(SupervisorError::StopUnavailable);
                }
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                    return Err(SupervisorError::StopUnavailable);
                }
                return stop_without_supervisor(store, task_id, attempt_id, &recorded);
            }
        }
        let path = stop_socket(&store.root().join("attempts"), attempt_id);
        let mut stream = UnixStream::connect(path).map_err(|_| SupervisorError::StopUnavailable)?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        if peer_pid(&stream)? != recorded.pid {
            return Err(SupervisorError::StopUnavailable);
        }
        stream.write_all(format!("{task_id}\n{attempt_id}\n").as_bytes())?;
        let mut answer = [0];
        if stream.read_exact(&mut answer).is_err() || answer[0] != b'Y' {
            return Err(SupervisorError::StopUnavailable);
        }
        Ok(())
    }
}

#[cfg(unix)]
fn independently_owned_child(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    supervisor: &ProcessIdentity,
) -> Result<ChildIdentity, SupervisorError> {
    let release = journal::intent_payload(store, task_id, attempt_id, "gate_release")?
        .and_then(|payload| recorded_process(&payload))
        .ok_or(SupervisorError::StopUnavailable)?;
    if &release != supervisor
        || binding::verify_attempt_artifact(&store.connection, store.root(), task_id, attempt_id)
            .is_err()
    {
        return Err(SupervisorError::StopUnavailable);
    }
    let registered =
        journal::evidence_payload(store, task_id, Some(attempt_id), "child_registered")?
            .and_then(|payload| serde_json::from_str::<ChildIdentity>(&payload).ok())
            .ok_or(SupervisorError::StopUnavailable)?;
    let attempts = store.root().join("attempts");
    use std::os::unix::fs::PermissionsExt;
    let dir = fs::symlink_metadata(&attempts).map_err(|_| SupervisorError::StopUnavailable)?;
    if !dir.file_type().is_dir() || dir.permissions().mode() & 0o777 != 0o700 {
        return Err(SupervisorError::StopUnavailable);
    }
    let child = private_bytes(&attempts.join(format!("{attempt_id}.child.json")))
        .and_then(|bytes| serde_json::from_slice::<ChildIdentity>(&bytes).ok())
        .ok_or(SupervisorError::StopUnavailable)?;
    if child != registered || child.pid == supervisor.pid || !matching_child(&child) {
        return Err(SupervisorError::StopUnavailable);
    }
    Ok(child)
}

#[cfg(not(unix))]
pub fn request_stop(
    store: &mut crate::state::StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<(), super::error::SupervisorError> {
    journal::record_stop_intent(store, task_id, attempt_id)?;
    Err(super::error::SupervisorError::StopUnavailable)
}
