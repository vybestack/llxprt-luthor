//! Launch planning and the Unix gated child runner.
mod binding;
#[cfg(unix)]
mod binding_gate;
mod command;
mod environment;
mod error;
mod evidence;
mod launch;
#[cfg(unix)]
mod log_failure;
mod planning;
mod processes;
#[cfg(unix)]
mod readiness;
#[cfg(unix)]
mod receipt;
mod reconciliation;
mod recovery;
mod runner;
mod service;
mod stop;
#[cfg(unix)]
mod stop_control;
mod storage;
mod terminal_exit;
#[cfg(unix)]
mod worker;

#[cfg(unix)]
pub(crate) use crate::model::{ChildIdentity, recorded_process};
pub use crate::model::{
    ExitReceipt, LaunchPlan, Reconciliation, RecoveryInspection, SessionEnvironment,
    TerminalExitProof,
};
pub use command::validate_saved_initial_plan;
pub use error::SupervisorError;
pub use launch::execute;
#[cfg(unix)]
pub use launch::{execute_amended, execute_amended_with_binary, execute_with_binary};
pub(crate) use planning::{RetryPlanAuthorization, prepare_retry};
pub use planning::{ensure_distinct_resume_prompt, prepare_initial, prepare_resume};
#[cfg(unix)]
pub(crate) use processes::{identity, private_bytes, verified_live_process};
pub(crate) use reconciliation::recheck_retry_exit;
pub use reconciliation::reconcile_attempt;
pub use recovery::inspect_recovery_quiescence;
pub use runner::run_gated_child;
#[cfg(unix)]
pub use runner::{run_gated_child_with_binary, run_gated_child_with_log_writers};
pub use service::supervise;
pub use stop::request_stop;
#[cfg(unix)]
pub use stop::{PendingStop, prepare_stop};
#[cfg(unix)]
pub use storage::validate_stop_socket_path;
pub use storage::{inspect_never_dispatched_storage, preflight_attempt_storage};
#[cfg(unix)]
pub use worker::worker_gate;

#[cfg(unix)]
mod child_protocol;
#[cfg(unix)]
mod log_capture;
#[cfg(unix)]
mod observation;
