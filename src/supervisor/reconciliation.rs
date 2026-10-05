use super::{error::SupervisorError, evidence::*, processes::*, terminal_exit};
use crate::model::*;
use crate::state::StateStore;
use crate::state::exits::reconcile_verified_exit as commit_exit;
use crate::state::{exit_observation, journal, scheduling};
use std::fs;

/// Only an exact durable exit with a proven absent process group releases capacity.
#[cfg(unix)]
pub fn reconcile_attempt(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<Reconciliation, SupervisorError> {
    reconcile_attempt_inner(store, task_id, attempt_id, None, false)
}

#[cfg(unix)]
pub(crate) fn recheck_retry_exit(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    revalidate_terminal_exit: bool,
    proof: &mut Option<TerminalExitProof>,
) -> Result<Reconciliation, SupervisorError> {
    reconcile_attempt_inner(
        store,
        task_id,
        attempt_id,
        Some(proof),
        revalidate_terminal_exit,
    )
}

#[cfg(unix)]
fn reconcile_attempt_inner(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
    terminal_exit: Option<&mut Option<TerminalExitProof>>,
    revalidate_terminal_exit: bool,
) -> Result<Reconciliation, SupervisorError> {
    let recheck_processes = terminal_exit.is_some();
    if !valid_attempt(attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    let held = |reason: &str| Reconciliation::Held {
        reason: reason.into(),
    };
    let (attempts, plan) = match reconciliation_plan_evidence(store, task_id, attempt_id)? {
        Ok(context) => context,
        Err(reason) => return Ok(held(reason)),
    };
    let processes = match reconciliation_process_evidence(store, task_id, attempt_id, &attempts)? {
        Ok(processes) => processes,
        Err(reason) => return Ok(held(reason)),
    };
    let ReconciliationProcesses {
        child: child_file,
        supervisor,
        tracked,
        sent: _,
    } = &processes;
    let receipt_path = attempts.join(format!("{attempt_id}.receipt.json"));
    if !receipt_path.exists() {
        return live_reconciliation(store, task_id, attempt_id, &attempts, &processes);
    }
    if recheck_processes
        && !matches!(fs::symlink_metadata(attempts.join(format!("{attempt_id}.supervisor-error.json"))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(held("supervisor error receipt exists"));
    }
    let receipt = match validated_receipt(&attempts, attempt_id, child_file) {
        Ok(receipt) => receipt,
        Err(reason) => return Ok(held(reason)),
    };
    let (evidence, outcome, completed) = exit_record(&receipt)?;
    if exit_observation::reconciled_exit(store, task_id, attempt_id, &evidence, &outcome)?
        && !recheck_processes
    {
        return Ok(completed);
    }
    if recheck_processes
        && revalidate_terminal_exit
        && child_file.boot_identity.starts_with("{ sec")
    {
        if !exit_observation::reconciled_exit(store, task_id, attempt_id, &evidence, &outcome)? {
            return Ok(held("terminal exit is not durably reconciled"));
        }
        match terminal_exit::prove(store, &plan, &receipt, child_file, supervisor, tracked) {
            Ok(proof) => {
                *terminal_exit.expect("retry inspection owns proof output") = Some(proof);
                return Ok(completed);
            }
            Err(reason) => return Ok(held(reason)),
        }
    }
    if recheck_processes
        && let Err(reason) = registered_process_absence(child_file, supervisor, tracked)
    {
        return Ok(held(reason));
    }
    if let Err(reason) = receipt_process_quiescence(&receipt, supervisor, tracked)? {
        return Ok(held(reason));
    }
    commit_exit(
        &mut store.connection,
        task_id,
        attempt_id,
        &evidence,
        &outcome,
    )?;
    Ok(completed)
}

#[cfg(not(unix))]
pub(crate) fn recheck_retry_exit(
    _store: &mut StateStore,
    _task_id: &str,
    _attempt_id: &str,
    _revalidate_terminal_exit: bool,
    _proof: &mut Option<TerminalExitProof>,
) -> Result<Reconciliation, SupervisorError> {
    Err(SupervisorError::ExecutionUnavailable)
}

#[cfg(not(unix))]
pub fn reconcile_attempt(
    _store: &mut StateStore,
    _task_id: &str,
    _attempt_id: &str,
) -> Result<Reconciliation, SupervisorError> {
    Err(SupervisorError::ExecutionUnavailable)
}

#[cfg(unix)]
fn live_reconciliation(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    attempts: &std::path::Path,
    processes: &ReconciliationProcesses,
) -> Result<Reconciliation, SupervisorError> {
    let ReconciliationProcesses {
        child: child_file,
        supervisor,
        tracked,
        sent,
    } = processes;
    let held = |reason: &str| Reconciliation::Held {
        reason: reason.into(),
    };
    if sent.is_none()
        || !scheduling::active_attempt_reservation(store, task_id, attempt_id)?
        || journal::stop_intent(store, task_id, attempt_id)?.is_some()
        || attempts
            .join(format!("{attempt_id}.supervisor-error.json"))
            .exists()
    {
        return Ok(held("live worker identity or reservation unverified"));
    }
    if registered_processes_absent(child_file, supervisor, tracked) {
        return Ok(held(
            "receipt missing; registered processes absent; operator recovery required",
        ));
    }
    if !verified_live_process(child_file, supervisor) {
        return Ok(held("live worker identity or reservation unverified"));
    }
    Ok(Reconciliation::Running)
}

#[cfg(unix)]
fn validated_receipt(
    attempts: &std::path::Path,
    attempt_id: &str,
    child_file: &ChildIdentity,
) -> Result<ExitReceipt, &'static str> {
    let receipt: ExitReceipt =
        match private_bytes(&attempts.join(format!("{attempt_id}.receipt.json")))
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        {
            Some(receipt) => receipt,
            None => return Err("missing or invalid receipt"),
        };
    let stdout = attempts.join(format!("{attempt_id}.stdout.log"));
    let stderr = attempts.join(format!("{attempt_id}.stderr.log"));
    if receipt.attempt_id != attempt_id
        || receipt.child_pid != child_file.pid
        || receipt.boot_identity != child_file.boot_identity
        || receipt.child_start_identity != child_file.start_identity
        || receipt.child_pid == 0
        || i32::try_from(receipt.child_pid).is_err()
        || receipt.boot_identity.trim().is_empty()
        || receipt.child_start_identity.trim().is_empty()
        || receipt.exit_code.is_some() == receipt.signal.is_some()
        || receipt.stdout_path != stdout
        || receipt.stderr_path != stderr
    {
        return Err("receipt identity or shape mismatch");
    }
    if private_log_size(&stdout) != Some(receipt.stdout_bytes)
        || private_log_size(&stderr) != Some(receipt.stderr_bytes)
    {
        return Err("missing, unsafe or incomplete logs");
    }
    Ok(receipt)
}

#[cfg(unix)]
fn receipt_process_quiescence(
    receipt: &ExitReceipt,
    supervisor: &ProcessIdentity,
    tracked: &[ProcessIdentity],
) -> Result<Result<(), &'static str>, SupervisorError> {
    match identity(supervisor.pid) {
        Ok((boot, start))
            if boot != supervisor.boot_identity || start != supervisor.start_identity =>
        {
            return Ok(Err("supervisor identity mismatch"));
        }
        Err(_) => {
            let supervisor_group =
                i32::try_from(supervisor.pid).map_err(|_| SupervisorError::IdentityUnavailable)?;
            if !unavailable_supervisor_group_is_quiescent(supervisor_group) {
                return Ok(Err("supervisor identity unavailable"));
            }
        }
        Ok(_) => {}
    }
    if let Ok((boot, start)) = identity(receipt.child_pid)
        && (boot != receipt.boot_identity || start != receipt.child_start_identity)
    {
        return Ok(Err("child identity mismatch"));
    }
    let pgid =
        i32::try_from(receipt.child_pid).map_err(|_| SupervisorError::IdentityUnavailable)?;
    // The negative argument probes the whole child group, never a bare PID.
    if unsafe { libc::kill(-pgid, 0) } == 0 {
        return Ok(Err("child process group is alive"));
    }
    if std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Ok(Err("child process group absence is unproven"));
    }
    Ok(tracked_process_quiescence(receipt, tracked))
}

#[cfg(unix)]
fn tracked_process_quiescence(
    receipt: &ExitReceipt,
    tracked: &[ProcessIdentity],
) -> Result<(), &'static str> {
    for process in tracked {
        if process.boot_identity != receipt.boot_identity {
            return Err("tracked descendant boot identity mismatch");
        }
        match identity(process.pid) {
            Ok((boot, start))
                if boot == process.boot_identity && start == process.start_identity =>
            {
                return Err("tracked descendant is alive");
            }
            Ok(_) => return Err("tracked descendant identity mismatch"),
            Err(_) => {
                if unsafe { libc::kill(process.pid as libc::pid_t, 0) } == 0
                    || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                {
                    return Err("tracked descendant absence is unproven");
                }
            }
        }
    }
    Ok(())
}

fn exit_record(receipt: &ExitReceipt) -> Result<(String, String, Reconciliation), SupervisorError> {
    Ok((
        serde_json::to_string(receipt)?,
        format!(
            "exit_code={:?};signal={:?}",
            receipt.exit_code, receipt.signal
        ),
        Reconciliation::Completed {
            exit_code: receipt.exit_code,
            signal: receipt.signal,
        },
    ))
}
