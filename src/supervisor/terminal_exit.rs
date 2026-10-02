use super::processes::*;
use crate::model::*;

#[cfg(unix)]
pub(crate) fn prove(
    store: &crate::state::StateStore,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
    child: &ChildIdentity,
    supervisor: &ProcessIdentity,
    tracked: &[ProcessIdentity],
) -> Result<TerminalExitProof, &'static str> {
    validate_registered_identities(child, supervisor, tracked)?;
    // This diagnostic is emitted before the native CLI constructs its session,
    // backend or tools. Other exits cannot exclude untracked escaped workers.
    if !tracked.is_empty() {
        return Err("startup rejection contradicts tracked descendant evidence");
    }
    let evidence = |kind: &str| {
        store
            .evidence_payload(&plan.task_id, Some(&plan.attempt_id), kind)
            .ok()
            .flatten()
            .ok_or("terminal exit registration is missing or unreadable")
    };
    let proof = TerminalExitProof {
        basis: TerminalExitBasis::NativeMaxToolCallsPreflight,
        receipt: receipt.clone(),
        stdout: private_bytes(&receipt.stdout_path)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or("startup rejection log is missing or invalid")?,
        child_registration: evidence("child_registered")?,
        supervisor_registration: evidence("supervisor_ready")?,
        gate_sent: evidence("gate_sent")?,
        gate_release: store
            .intent_payload(&plan.task_id, &plan.attempt_id, "gate_release")
            .ok()
            .flatten()
            .ok_or("terminal exit gate release is missing or unreadable")?,
        current_boot_identity: identity(std::process::id())
            .map_err(|_| "current boot identity is unavailable")?
            .0,
        observed_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "terminal exit observation time is unavailable")?
            .as_secs(),
    };
    if !proof.matches_startup_rejection(plan) {
        return Err("historical exit does not prove native preflight rejection");
    }
    probe_registered_absence(
        child,
        supervisor,
        tracked,
        |pid| {
            (unsafe { libc::kill(pid as libc::pid_t, 0) }) == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        },
        group_absent,
    )?;
    Ok(proof)
}
