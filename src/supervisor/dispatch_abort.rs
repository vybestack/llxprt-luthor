//! Terminal proof for cancellation before the worker gate is released.
use super::{
    SupervisorError,
    processes::{group_absent, identity, private_bytes, registered_processes_absent},
    storage::write_private_json_atomic,
};
use crate::model::{ChildIdentity, LaunchPlan, ProcessIdentity};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchAbort {
    pub(crate) plan: LaunchPlan,
    pub(crate) supervisor: ProcessIdentity,
    pub(crate) child: Option<ChildIdentity>,
}

pub(crate) fn record(
    attempts: &Path,
    plan: &LaunchPlan,
    child: Option<ChildIdentity>,
) -> Result<SupervisorError, SupervisorError> {
    let pid = std::process::id();
    let (boot_identity, start_identity) = identity(pid)?;
    if unsafe { libc::getpgid(pid as i32) } != pid as i32 {
        return Err(SupervisorError::IdentityUnavailable);
    }
    write_private_json_atomic(
        &attempts.join(format!("{}.dispatch-aborted.json", plan.attempt_id)),
        &DispatchAbort {
            plan: plan.clone(),
            supervisor: ProcessIdentity {
                pid,
                boot_identity,
                start_identity,
            },
            child,
        },
    )?;
    Ok(SupervisorError::DispatchAborted)
}

pub(crate) fn read(attempts: &Path, attempt: &str) -> Option<DispatchAbort> {
    let bytes = private_bytes(&attempts.join(format!("{attempt}.dispatch-aborted.json")))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let abort: DispatchAbort = serde_json::from_value(value.clone()).ok()?;
    (serde_json::to_value(&abort).ok()? == value).then_some(abort)
}

pub(crate) fn path_absent(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

pub(crate) fn processes_absent(abort: &DispatchAbort) -> bool {
    if let Some(child) = &abort.child {
        return registered_processes_absent(child, &abort.supervisor, &[]);
    }
    let supervisor = &abort.supervisor;
    supervisor.pid > 0
        && i32::try_from(supervisor.pid).is_ok()
        && !supervisor.start_identity.trim().is_empty()
        && !supervisor.boot_identity.starts_with("{ sec")
        && identity(std::process::id()).is_ok_and(|(boot, _)| boot == supervisor.boot_identity)
        && unsafe { libc::kill(supervisor.pid as i32, 0) } == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        && group_absent(supervisor.pid as i32)
}

/// An open pipe is the coordinator's cancellation token. It need not contain a
/// release byte yet. HUP/EOF means dispatch has been abandoned; never spawn.
pub(crate) fn require_coordinator() -> Result<(), SupervisorError> {
    let mut fd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut fd, 1, 0) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(SupervisorError::GateClosed);
    }
    Ok(())
}
