use super::{
    error::SupervisorError,
    processes::identity,
    storage::{write_private_json, write_private_json_atomic},
    worker::{self, configure_session},
};
use crate::{
    model::{ChildIdentity, LaunchPlan},
    ownership::WorktreeOwner,
};
use std::{
    fs,
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
};

pub(crate) fn spawn_gated_worker(
    plan: &LaunchPlan,
    root: &Path,
    binary: &Path,
    managed: bool,
    owner: &WorktreeOwner,
) -> Result<Child, SupervisorError> {
    let plan_path = root.join(format!("{}.plan.json", plan.attempt_id));
    if !managed {
        write_private_json(&plan_path, plan)?;
    }
    let mut command = Command::new(binary);
    command
        .arg("__worker_gate")
        // The worker gate changes cwd to the task worktree; preserve the plan's identity.
        .arg(fs::canonicalize(&plan_path)?)
        .current_dir(&plan.worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_session(&mut command, &plan.session_environment);
    owner.inherit_into(&mut command);
    command.process_group(0);
    Ok(command.spawn()?)
}

pub(crate) fn release_registered_worker(
    child: &mut Child,
    gate: &mut impl Read,
    plan: &LaunchPlan,
    root: &Path,
    managed: bool,
) -> Result<(ChildIdentity, ChildStdin), SupervisorError> {
    let mut shim_gate = child.stdin.take().expect("piped shim gate");
    let mut saved_identity = None;
    let registered = (|| {
        let (boot_identity, start_identity) = identity(child.id())?;
        let pid = i32::try_from(child.id()).map_err(|_| SupervisorError::IdentityUnavailable)?;
        if unsafe { libc::getpgid(pid) } != pid {
            return Err(SupervisorError::IdentityUnavailable);
        }
        let identity = ChildIdentity {
            pid: child.id(),
            boot_identity,
            start_identity,
            group_id: child.id(),
        };
        write_private_json_atomic(
            &root.join(format!("{}.child.json", plan.attempt_id)),
            &identity,
        )?;
        saved_identity = Some(identity.clone());
        if managed {
            announce_ready()?;
        }
        let mut release = [0];
        if gate.read(&mut release)? != 1 || release != *b"R" {
            return Err(SupervisorError::GateClosed);
        }
        if managed {
            worker::verify_gate_release(root, plan, &identity)?;
        }
        shim_gate.write_all(b"R")?;
        Ok(identity)
    })();
    match registered {
        Ok(identity) => Ok((identity, shim_gate)),
        Err(error) => {
            drop(shim_gate);
            child.wait()?;
            if managed
                && matches!(error, SupervisorError::GateClosed)
                && let Some(identity) = saved_identity
            {
                return Err(super::dispatch_abort::record(root, plan, Some(identity))?);
            }
            Err(error)
        }
    }
}

fn announce_ready() -> Result<(), SupervisorError> {
    let mut output = std::io::stdout().lock();
    match output.write_all(b"READY\n").and_then(|()| output.flush()) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
            return Err(SupervisorError::GateClosed);
        }
        result => result?,
    }
    wait_for_release(0)
}

fn wait_for_release(fd: std::os::fd::RawFd) -> Result<(), SupervisorError> {
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut poll, 1, 5000) };
    if ready < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // The coordinator writes R and then closes its pipe. POLLIN and POLLHUP
    // may arrive together: drain the byte before interpreting EOF as cancellation.
    // The caller still requires exactly R and verifies the durable release intent.
    if ready == 0
        || poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0
        || poll.revents & libc::POLLIN == 0
    {
        return Err(SupervisorError::GateClosed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::File,
        os::fd::{AsRawFd, FromRawFd},
    };

    #[test]
    fn buffered_release_survives_coordinator_pipe_close() {
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let mut reader = unsafe { File::from_raw_fd(fds[0]) };
        let mut writer = unsafe { File::from_raw_fd(fds[1]) };
        writer.write_all(b"R").unwrap();
        drop(writer);
        wait_for_release(reader.as_raw_fd()).unwrap();
        let mut byte = [0];
        assert_eq!(reader.read(&mut byte).unwrap(), 1);
        assert_eq!(byte, *b"R");
        assert_eq!(reader.read(&mut byte).unwrap(), 0);
    }
}
