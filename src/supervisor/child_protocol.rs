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
            Err(error)
        }
    }
}

fn announce_ready() -> Result<(), SupervisorError> {
    println!("READY");
    let mut poll = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut poll, 1, 5000) } <= 0 {
        return Err(SupervisorError::GateClosed);
    }
    Ok(())
}
