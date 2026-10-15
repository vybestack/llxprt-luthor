//! Only the worker's single terminal stdout object can identify a retry class.
use crate::{
    config::TransientFailure,
    model::{ExitReceipt, LaunchPlan},
    supervisor::SupervisorError,
};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerOutcome {
    Transient {
        class: TransientFailure,
    },
    /// A completed worker with no PR, or an explicitly declared terminal verdict,
    /// needs operator review; neither authorizes automatic continuation.
    NeedsHumanInput,
    NonTransient,
    Unclassified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeObservation {
    pub outcome: WorkerOutcome,
    pub observed_at_unix_secs: u64,
}

pub(crate) fn read(
    root: &Path,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
) -> Result<WorkerOutcome, SupervisorError> {
    if receipt.stdout_path
        != root
            .join("attempts")
            .join(format!("{}.stdout.log", plan.attempt_id))
    {
        return Err(SupervisorError::Conflict);
    }
    if receipt.signal.is_some() || !receipt.stop_signals.is_empty() {
        return Ok(WorkerOutcome::NonTransient);
    }
    // Do not materialize an arbitrarily large tool log as an envelope.
    if receipt.stdout_bytes > 1024 * 1024 {
        return Ok(WorkerOutcome::Unclassified);
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(&receipt.stdout_path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != receipt.stdout_bytes {
        return Err(SupervisorError::Conflict);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(SupervisorError::Conflict);
        }
    }
    let mut bytes = Vec::new();
    file.take(receipt.stdout_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != receipt.stdout_bytes {
        return Err(SupervisorError::Conflict);
    }
    Ok(classify(&bytes, &plan.session_id, receipt.exit_code))
}

#[derive(Deserialize)]
struct TerminalEnvelope {
    status: String,
    session_id: String,
    error: Option<TerminalError>,
}

#[derive(Deserialize)]
struct TerminalError {
    code: String,
    #[serde(rename = "message")]
    _message: String,
    terminal_outcome: Option<String>,
}

fn classify(bytes: &[u8], session: &str, exit: Option<i32>) -> WorkerOutcome {
    let Ok(value) = serde_json::from_slice::<TerminalEnvelope>(bytes) else {
        return WorkerOutcome::Unclassified;
    };
    if value.session_id != session {
        return WorkerOutcome::Unclassified;
    }
    match (value.status.as_str(), exit, value.error) {
        ("ok", Some(0), None) => WorkerOutcome::NeedsHumanInput,
        ("error", Some(code), Some(error)) if code != 0 => classify_error(&error, code),
        _ => WorkerOutcome::Unclassified,
    }
}

fn classify_error(error: &TerminalError, exit: i32) -> WorkerOutcome {
    if error.terminal_outcome.is_some() {
        return WorkerOutcome::NeedsHumanInput;
    }
    let class = match (error.code.as_str(), exit) {
        ("model-rate-limit", 5) => TransientFailure::RateLimit,
        ("model-transient-server", 5) => TransientFailure::TransientServer,
        ("model-connectivity", 5) => TransientFailure::Connectivity,
        ("turn-budget" | "turn-time-exhausted", 6) => TransientFailure::TurnBudget,
        ("model-quota-exhausted" | "model-permanent", 5) => return WorkerOutcome::NonTransient,
        _ => return WorkerOutcome::Unclassified,
    };
    WorkerOutcome::Transient { class }
}

#[cfg(test)]
mod tests {
    use crate::config::TransientFailure;
    use crate::coordinator::auto_retry::outcome::{WorkerOutcome, classify};
    use serde_json::json;

    #[test]
    fn only_typed_terminal_codes_with_matching_exit_family_authorize_retry() {
        for (code, class, exit, wrong_exit) in [
            ("model-rate-limit", TransientFailure::RateLimit, 5, 6),
            (
                "model-transient-server",
                TransientFailure::TransientServer,
                5,
                6,
            ),
            ("model-connectivity", TransientFailure::Connectivity, 5, 6),
            ("turn-budget", TransientFailure::TurnBudget, 6, 5),
            ("turn-time-exhausted", TransientFailure::TurnBudget, 6, 5),
        ] {
            let bytes = json!({"status":"error","session_id":"task","error":{"code":code,"message":"not parsed"}}).to_string();
            assert_eq!(
                classify(bytes.as_bytes(), "task", Some(exit)),
                WorkerOutcome::Transient { class }
            );
            assert_eq!(
                classify(bytes.as_bytes(), "other", Some(exit)),
                WorkerOutcome::Unclassified
            );
            assert_eq!(
                classify(bytes.as_bytes(), "task", Some(wrong_exit)),
                WorkerOutcome::Unclassified
            );
            assert_eq!(
                classify(bytes.as_bytes(), "task", Some(0)),
                WorkerOutcome::Unclassified
            );
            assert_eq!(
                classify(
                    format!("prompt echo\n{bytes}").as_bytes(),
                    "task",
                    Some(exit)
                ),
                WorkerOutcome::Unclassified
            );
            assert_eq!(
                classify(format!("{bytes}\n{bytes}").as_bytes(), "task", Some(exit)),
                WorkerOutcome::Unclassified
            );
        }
    }

    #[test]
    fn log_markers_quota_refusals_and_human_input_are_not_transient() {
        for code in ["model", "turn", "model-quota-exhausted", "model-permanent"] {
            let bytes = json!({"status":"error","session_id":"task","error":{"code":code,"message":"body read failed timeout 429 502 503 504 connection reset"}}).to_string();
            assert!(!matches!(
                classify(bytes.as_bytes(), "task", Some(5)),
                WorkerOutcome::Transient { .. }
            ));
        }
        let refusal = br#"{"status":"ok","session_id":"task","summary":"I need the missing handoff before proceeding"}"#;
        assert_eq!(
            classify(refusal, "task", Some(0)),
            WorkerOutcome::NeedsHumanInput
        );
        let terminal = br#"{"status":"error","session_id":"task","error":{"code":"model-connectivity","message":"no retry", "terminal_outcome":"output_truncated"}}"#;
        assert_eq!(
            classify(terminal, "task", Some(5)),
            WorkerOutcome::NeedsHumanInput
        );
        assert_eq!(
            classify(b"body read failed", "task", Some(5)),
            WorkerOutcome::Unclassified
        );
    }
}
