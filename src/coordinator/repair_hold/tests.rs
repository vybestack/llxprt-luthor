use super::*;
use crate::{config::ConfigError, pr_repair::RepairHoldCause, worktree::WorktreeError};

#[test]
fn every_supervisor_error_class_has_a_distinct_bounded_hold_code() {
    let json = serde_json::from_str::<serde_json::Value>("secret-upstream-data").unwrap_err();
    let errors = [
        (
            SupervisorError::State(StateError::InvalidSelection),
            "supervisor-state-unverified",
        ),
        (
            SupervisorError::Worktree(WorktreeError::Conflict("secret-upstream-data")),
            "supervisor-worktree-unverified",
        ),
        (
            SupervisorError::Sql(rusqlite::Error::InvalidParameterName(
                "secret-upstream-data".into(),
            )),
            "supervisor-database-unavailable",
        ),
        (
            SupervisorError::Config(ConfigError::Invalid("secret-upstream-data".into())),
            "supervisor-config-unverified",
        ),
        (
            SupervisorError::Io(std::io::Error::other("secret-upstream-data")),
            "supervisor-io-unavailable",
        ),
        (SupervisorError::Json(json), "supervisor-json-invalid"),
        (
            SupervisorError::WorkerBuildHeld("secret-upstream-data".into()),
            "worker-build-held",
        ),
        (SupervisorError::Conflict, "launch-plan-conflict"),
        (
            SupervisorError::ReadyTimeout,
            "supervisor-ready-timeout; launch accounting required before restore can retry",
        ),
        (
            SupervisorError::ExecutionUnavailable,
            "supervisor-execution-unavailable",
        ),
        (
            SupervisorError::DispatchAborted,
            "supervisor-dispatch-aborted",
        ),
        (SupervisorError::GateClosed, "supervisor-gate-closed"),
        (
            SupervisorError::StopUnavailable,
            "supervisor-stop-unavailable",
        ),
        (
            SupervisorError::IdentityUnavailable,
            "supervisor-identity-unavailable",
        ),
        #[cfg(unix)]
        (
            SupervisorError::StopSocketPathTooLong,
            "stop-socket-path-too-long",
        ),
    ];
    let mut seen = std::collections::HashSet::new();
    for (error, code) in errors {
        let reason = reason(&DispatchError::Supervisor(error));
        assert_eq!(reason, format!("PR repair held: {code}"));
        assert!(seen.insert(reason));
    }
}

#[test]
fn typed_check_failures_keep_their_own_codes_and_only_transient_causes_suggest_waiting() {
    let cases = [
        (
            RepairHoldCause::PrIdentityUnverified,
            "pr-identity-unverified",
            false,
        ),
        (RepairHoldCause::OwnerLockBusy, "owner-lock-busy", true),
        (
            RepairHoldCause::OwnerUnavailable,
            "owner-unavailable",
            false,
        ),
        (
            RepairHoldCause::OwnerProtocolUnverified,
            "owner-protocol-unverified",
            false,
        ),
        (
            RepairHoldCause::AttemptEvidenceUnverified,
            "attempt-evidence-unverified",
            false,
        ),
        (
            RepairHoldCause::ProcessEvidenceUnverified,
            "process-evidence-unverified",
            false,
        ),
        (
            RepairHoldCause::ReceiptUnverified,
            "receipt-unverified",
            false,
        ),
        (RepairHoldCause::AttemptSettling, "attempt-settling", true),
        (
            RepairHoldCause::SessionUnverified,
            "session-unverified",
            false,
        ),
    ];
    let mut seen = std::collections::HashSet::new();
    for (cause, code, transient) in cases {
        let reason = reason(&DispatchError::Supervisor(SupervisorError::PrRepairHeld(
            cause,
        )));
        assert_eq!(
            reason.split(';').next().unwrap(),
            format!("PR repair held: {code}")
        );
        assert_eq!(reason.contains("restore will retry next cycle"), transient);
        assert_eq!(reason, cause.reason());
        assert!(seen.insert(reason));
    }
}
