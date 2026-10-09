use crate::{
    config::{CommandTemplate, Config, Mapping, Source},
    eligibility::Candidate,
    github::pull_request::ErrorCategory,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error("task identity already exists: {0}/{1}")]
    DuplicateTask(String, String),
    #[error("capacity exhausted: {reserved} reservations for capacity {capacity}")]
    Capacity { reserved: usize, capacity: usize },
    #[error("capacity must be positive")]
    InvalidCapacity,
    #[error("invalid configuration")]
    InvalidConfig,
    #[error("invalid explicit target")]
    InvalidTarget,
    #[error("multiple tasks match explicit target {0}/{1}")]
    AmbiguousTarget(String, u64),
    #[error("candidate selection does not match effective configuration")]
    InvalidSelection,
    #[error("launch requires a verified worktree and an unused, accounted task")]
    LaunchBlocked,
    #[error("configured capacity {configured} does not match persisted capacity {persisted}")]
    CapacityMismatch { configured: usize, persisted: usize },
    #[error("unsupported database version {0}")]
    UnsupportedDatabaseVersion(i32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeIntent {
    pub path: PathBuf,
    pub branch: String,
    pub base: String,
    pub repository: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeIdentity {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub branch: String,
    pub base: String,
    pub head: String,
    pub repository: String,
    pub git_directory: PathBuf,
    pub remote: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub intent: WorktreeIntent,
    pub identity: Option<WorktreeIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct EffectiveConfigSnapshot {
    pub state_root: PathBuf,
    pub worktree_root: PathBuf,
    pub capacity: usize,
    pub assignment_login: String,
    pub sources: Vec<Source>,
    pub mappings: Vec<Mapping>,
    pub initial: CommandTemplate,
    pub resume: CommandTemplate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PausePrStatus {
    Absent,
    Open,
    Ambiguous,
    Error {
        category: ErrorCategory,
        code: String,
        http_status: Option<u16>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PausePrEvidence {
    pub observed_at_unix_secs: u64,
    pub repository: String,
    pub status: PausePrStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitPrEvidence {
    pub observed_at_unix_secs: u64,
    pub repository: String,
    pub status: PausePrStatus,
}

#[derive(Serialize)]
pub(crate) struct OperatorRecoveryAudit<'a> {
    pub(crate) actor: &'a str,
    pub(crate) reason: &'a str,
    pub(crate) observed_at_unix_secs: u64,
    pub(crate) os_ids: Vec<serde_json::Value>,
    pub(crate) supervisor: serde_json::Value,
    pub(crate) child: serde_json::Value,
    pub(crate) tracked_os_identities: Vec<serde_json::Value>,
}

impl From<&Config> for EffectiveConfigSnapshot {
    fn from(config: &Config) -> Self {
        Self {
            state_root: config.state_root.clone(),
            worktree_root: config.worktree_root.clone(),
            capacity: config.capacity,
            assignment_login: config.assignment_login.clone(),
            sources: config.sources.clone(),
            mappings: config.mappings.clone(),
            initial: config.initial.clone(),
            resume: config.resume.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct SelectionEvidence {
    pub candidate: Candidate,
    pub config_revision: String,
    pub effective_config: EffectiveConfigSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrySourceEvidence {
    pub item: crate::github::project::ProjectItem,
    pub issue: crate::github::project::Issue,
    pub claim: String,
    pub observed_at_unix_secs: u64,
}

/// Private, append-only authorization for a single natural-exit continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryAuthorization {
    pub actor: String,
    pub reason: String,
    pub previous_plan: LaunchPlan,
    pub previous_config: EffectiveConfigSnapshot,
    pub config: EffectiveConfigSnapshot,
    pub plan: LaunchPlan,
    pub reservation: String,
    pub pr: ExitPrEvidence,
    pub source: RetrySourceEvidence,
    pub terminal_exit: Option<TerminalExitProof>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryInspection {
    Held(&'static str),
    Quiescent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnvironment {
    pub home: PathBuf,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub llxprt_config_home: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub task_id: String,
    pub attempt_id: String,
    pub session_id: String,
    pub worktree: PathBuf,
    pub expected_worktree: WorktreeIdentity,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub config_revision: String,
    pub session_environment: SessionEnvironment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    Running,
    Completed {
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    Held {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitReceipt {
    pub attempt_id: String,
    pub child_pid: u32,
    pub boot_identity: String,
    pub child_start_identity: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout_path: PathBuf,
    pub stdout_bytes: u64,
    pub stderr_path: PathBuf,
    pub stderr_bytes: u64,
    #[serde(default)]
    pub stop_signals: Vec<i32>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ChildIdentity {
    pub(crate) pid: u32,
    pub(crate) boot_identity: String,
    pub(crate) start_identity: String,
    pub(crate) group_id: u32,
}

#[cfg(unix)]
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) boot_identity: String,
    pub(crate) start_identity: String,
}

#[cfg(unix)]
pub(crate) fn recorded_process(payload: &str) -> Option<ProcessIdentity> {
    let value: ProcessIdentity = serde_json::from_str(payload).ok()?;
    (value.pid > 0
        && i32::try_from(value.pid).is_ok()
        && !value.boot_identity.trim().is_empty()
        && !value.start_identity.trim().is_empty())
    .then_some(value)
}

#[cfg(unix)]
pub(crate) fn validate_registered_identities(
    child: &ChildIdentity,
    supervisor: &ProcessIdentity,
    tracked: &[ProcessIdentity],
) -> Result<(), &'static str> {
    let bounded = |boot: &str, start: &str| {
        !boot.trim().is_empty()
            && boot.len() <= 256
            && !start.trim().is_empty()
            && start.len() <= 64
    };
    if child.pid == 0
        || supervisor.pid == 0
        || i32::try_from(child.pid).is_err()
        || i32::try_from(supervisor.pid).is_err()
        || child.pid == supervisor.pid
        || child.group_id != child.pid
        || !bounded(&child.boot_identity, &child.start_identity)
        || !bounded(&supervisor.boot_identity, &supervisor.start_identity)
        || supervisor.boot_identity != child.boot_identity
        || tracked.iter().any(|process| {
            process.pid == 0
                || i32::try_from(process.pid).is_err()
                || !bounded(&process.boot_identity, &process.start_identity)
                || process.boot_identity != child.boot_identity
        })
    {
        return Err("registered process identity is invalid or contradictory");
    }
    Ok(())
}

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
        let Ok(child) = serde_json::from_str::<ChildIdentity>(&self.child_registration) else {
            return false;
        };
        let Some(supervisor) = recorded_process(&self.supervisor_registration) else {
            return false;
        };
        validate_registered_identities(&child, &supervisor, &[]).is_ok()
            && child.pid == self.receipt.child_pid
            && child.boot_identity == self.receipt.boot_identity
            && child.start_identity == self.receipt.child_start_identity
            && recorded_process(&self.gate_release).as_ref() == Some(&supervisor)
            && recorded_process(&self.gate_sent).as_ref() == Some(&supervisor)
    }

    #[cfg(not(unix))]
    fn matches_registration(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedPromptVersion {
    TrackerOnlyV1,
    TrackerAndClosingV2,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerifiedOpenPr {
    pub(crate) id: u64,
    pub(crate) number: u64,
    pub(crate) url: String,
    pub(crate) repository_id: u64,
    pub(crate) repository: String,
    pub(crate) head_repository_id: u64,
    pub(crate) head_repository: String,
    pub(crate) base_branch: String,
    pub(crate) head_branch: String,
    pub(crate) author: String,
    pub(crate) active_login: String,
    pub(crate) tracker_issue_url: String,
    pub(crate) draft: bool,
    pub(crate) checks: Option<Vec<String>>,
    pub(crate) created_at: String,
    pub(crate) head_commit_sha: String,
    pub(crate) observed_at: u64,
    pub(crate) attempt_id: String,
}
