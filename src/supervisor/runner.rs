#[cfg(unix)]
#[cfg(unix)]
use super::{
    child_protocol::{release_registered_worker, spawn_gated_worker},
    error::SupervisorError,
    log_capture::LogCapture,
    observation::observe_worker,
    processes::valid_attempt,
    receipt::finish_receipt,
};
#[cfg(unix)]
use crate::model::{ExitReceipt, LaunchPlan};
#[cfg(unix)]
use std::{
    env,
    fs::File,
    io::{Read, Write},
    os::unix::{net::UnixListener, process::ExitStatusExt},
    path::Path,
    process::ExitStatus,
};
/// Waits for one explicit `R` byte before starting the worker. EOF keeps the
/// already-reserved attempt held and launches nothing. Unix process groups are
/// used so later reconciliation can independently prove group termination.
#[cfg(unix)]
pub fn run_gated_child<R: Read>(
    plan: &LaunchPlan,
    gate: R,
    store_root: &Path,
) -> Result<ExitStatus, SupervisorError> {
    run_gated_child_with_binary(plan, gate, store_root, &env::current_exe()?)
}

#[cfg(unix)]
pub fn run_gated_child_with_binary<R: Read>(
    plan: &LaunchPlan,
    gate: R,
    store_root: &Path,
    binary: &Path,
) -> Result<ExitStatus, SupervisorError> {
    run_gated_child_control(plan, gate, store_root, None, binary, |out, err| (out, err))
}

/// Allows a controlled log writer to be injected without changing filesystem-wide behavior.
#[cfg(unix)]
pub fn run_gated_child_with_log_writers<R, O, E>(
    plan: &LaunchPlan,
    gate: R,
    store_root: &Path,
    binary: &Path,
    writers: impl FnOnce(File, File) -> (O, E),
) -> Result<ExitStatus, SupervisorError>
where
    R: Read,
    O: Write + Send + 'static,
    E: Write + Send + 'static,
{
    run_gated_child_control(plan, gate, store_root, None, binary, writers)
}

#[cfg(unix)]
pub(crate) fn run_gated_child_control<R, O, E>(
    plan: &LaunchPlan,
    mut gate: R,
    store_root: &Path,
    control: Option<&UnixListener>,
    binary: &Path,
    writers: impl FnOnce(File, File) -> (O, E),
) -> Result<ExitStatus, SupervisorError>
where
    R: Read,
    O: Write + Send + 'static,
    E: Write + Send + 'static,
{
    if !valid_attempt(&plan.attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    let logs = LogCapture::open(store_root, plan)?;
    let mut child = spawn_gated_worker(plan, store_root, binary, control.is_some())?;
    let (registered, _shim_gate) =
        release_registered_worker(&mut child, &mut gate, plan, store_root, control.is_some())?;
    let (stdout_path, stderr_path, drains) = logs.start(&mut child, writers);
    let completed = observe_worker(&mut child, &registered, drains, plan, store_root, control)?;
    let receipt = ExitReceipt {
        attempt_id: plan.attempt_id.clone(),
        child_pid: registered.pid,
        boot_identity: registered.boot_identity,
        child_start_identity: registered.start_identity,
        exit_code: completed.status.code(),
        signal: completed.status.signal(),
        stdout_path,
        stdout_bytes: completed.stdout_bytes,
        stderr_path,
        stderr_bytes: completed.stderr_bytes,
        stop_signals: completed.stop_signals,
    };
    finish_receipt(store_root, plan, &receipt, control.is_some())?;
    Ok(completed.status)
}

#[cfg(not(unix))]
pub fn run_gated_child<R: std::io::Read>(
    _plan: &crate::model::LaunchPlan,
    _gate: R,
    _store_root: &std::path::Path,
) -> Result<std::process::ExitStatus, super::error::SupervisorError> {
    Err(super::error::SupervisorError::ExecutionUnavailable)
}
