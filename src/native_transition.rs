//! Audited native continuation domain records.
use crate::model::{EffectiveConfigSnapshot, ExitPrEvidence, LaunchPlan, RetrySourceEvidence};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    StoppedResume,
    NaturalExitRetry,
}
impl Mode {
    pub(crate) fn stopped(self) -> bool {
        self == Self::StoppedResume
    }
    pub(crate) fn phase(self) -> &'static str {
        if self.stopped() {
            "paused"
        } else {
            "attention"
        }
    }
}

/// Exact scoped journal rows include their sequences and immutable payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Predecessor {
    pub original_plan: LaunchPlan,
    pub effective_plan: LaunchPlan,
    pub amendment_sequence: i64,
    pub transition_sequence: Option<i64>,
    pub intents: Vec<(i64, String, String)>,
    pub evidence: Vec<(i64, String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Transition {
    pub mode: Mode,
    pub actor: String,
    pub predecessor: Predecessor,
    pub config: EffectiveConfigSnapshot,
    pub plan: LaunchPlan,
    pub pr: ExitPrEvidence,
    pub source: RetrySourceEvidence,
    pub observed_at_unix_secs: u64,
}

pub(crate) const KIND: &str = "native_continuation";
pub(crate) const SEAL: &str = "native_continuation_seal";

pub(crate) struct Context {
    pub initial: crate::state::NeverDispatchedContext,
    pub predecessor: Predecessor,
}
