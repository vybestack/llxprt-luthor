use super::{ExitReceipt, LaunchPlan};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalExitBasis {
    NativeMaxToolCallsPreflight,
}

/// A terminal startup rejection is a separate proof, not a reconstructed boot identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalExitProof {
    pub basis: TerminalExitBasis,
    pub receipt: ExitReceipt,
    pub stdout: String,
    pub child_registration: String,
    pub supervisor_registration: String,
    pub gate_release: String,
    pub gate_sent: String,
    pub current_boot_identity: String,
    pub observed_at_unix_secs: u64,
}

impl TerminalExitProof {
    pub(crate) fn matches_startup_rejection(&self, plan: &LaunchPlan) -> bool {
        let budgets: Vec<_> = plan
            .args
            .iter()
            .enumerate()
            .filter_map(|(index, arg)| {
                if arg == "--max-tool-calls" {
                    Some(
                        plan.args
                            .get(index + 1)
                            .map(String::as_str)
                            .unwrap_or_default(),
                    )
                } else {
                    arg.strip_prefix("--max-tool-calls=")
                }
            })
            .collect();
        let diagnostic = serde_json::json!({
            "error": {"code": "max-tool-calls", "message": "--max-tool-calls must be -1 or an integer from 1 through 512 (got 1024)"},
            "session_id": plan.session_id, "status": "error"
        }).to_string() + "\n";
        plan.executable
            .file_name()
            .is_some_and(|name| name == "llxprt-code-rs")
            && budgets == ["1024"]
            && plan.session_id == plan.task_id
            && self.receipt.attempt_id == plan.attempt_id
            && self.receipt.exit_code == Some(2)
            && self.receipt.signal.is_none()
            && self.receipt.stop_signals.is_empty()
            && self.receipt.boot_identity.starts_with("{ sec = ")
            && self.receipt.stdout_bytes == diagnostic.len() as u64
            && self.receipt.stderr_bytes == 0
            && self.stdout == diagnostic
            && !self.current_boot_identity.trim().is_empty()
            && self.current_boot_identity.len() <= 256
            && !self.current_boot_identity.starts_with("{ sec")
            && self.observed_at_unix_secs > 0
            && self.matches_registration()
    }

    #[cfg(unix)]
    fn matches_registration(&self) -> bool {
        let Ok(child) = serde_json::from_str::<super::ChildIdentity>(&self.child_registration)
        else {
            return false;
        };
        let Some(supervisor) = super::recorded_process(&self.supervisor_registration) else {
            return false;
        };
        super::validate_registered_identities(&child, &supervisor, &[]).is_ok()
            && child.pid == self.receipt.child_pid
            && child.boot_identity == self.receipt.boot_identity
            && child.start_identity == self.receipt.child_start_identity
            && super::recorded_process(&self.gate_release).as_ref() == Some(&supervisor)
            && super::recorded_process(&self.gate_sent).as_ref() == Some(&supervisor)
    }

    #[cfg(not(unix))]
    fn matches_registration(&self) -> bool {
        false
    }
}

#[cfg(unix)]
pub(super) fn prove(
    store: &crate::state::StateStore,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
    child: &super::ChildIdentity,
    supervisor: &super::ProcessIdentity,
    tracked: &[super::ProcessIdentity],
) -> Result<TerminalExitProof, &'static str> {
    super::validate_registered_identities(child, supervisor, tracked)?;
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
        stdout: super::private_bytes(&receipt.stdout_path)
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
        current_boot_identity: super::identity(std::process::id())
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
    super::probe_registered_absence(
        child,
        supervisor,
        tracked,
        |pid| {
            (unsafe { libc::kill(pid as libc::pid_t, 0) }) == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        },
        super::group_absent,
    )?;
    Ok(proof)
}
