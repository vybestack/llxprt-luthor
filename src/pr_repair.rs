//! Auditable authorization of a bounded continuation of an existing PR.
use crate::{
    github::pull_request::PrHealth,
    model::{LaunchPlan, RetrySourceEvidence, VerifiedOpenPr},
};
use serde::{Deserialize, Serialize};

mod hold;
pub use hold::RepairHoldCause;
pub(crate) use hold::open_hold_reasons;

pub const MAX_PR_REPAIRS: usize = 3;
pub const REPAIR_RESULT_FILE: &str = ".luthor-pr-repair-result.json";
/// The reason an earlier release wrote for any repair failure, including a
/// finished task whose PR had merged. Only restoration reads it; new holds carry
/// the specific cause instead.
pub const UNSPECIFIC_REPAIR_HOLD_REASON: &str =
    "PR repair validation or launch failed; inspect task evidence";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrRepairAuthorization {
    pub previous_plan: LaunchPlan,
    pub plan: LaunchPlan,
    pub saved_pr: VerifiedOpenPr,
    pub observed_pr: VerifiedOpenPr,
    pub health: PrHealth,
    pub source: RetrySourceEvidence,
    pub log_excerpts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrRepairResult {
    pub attempt_id: String,
    pub status: PrRepairStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrRepairStatus {
    Repaired,
    SemanticConflict,
}

/// An observed typed worker failure, not a fabricated repair result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairRetry {
    pub task_id: String,
    pub attempt_id: String,
    pub backoff_until_unix_secs: u64,
}

pub(crate) const RESULT_UNAVAILABLE: &str = "PR repair result unavailable or invalid";
