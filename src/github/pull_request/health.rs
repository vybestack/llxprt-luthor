use serde::{Deserialize, Serialize};

/// Fresh health of one immutable PR/head snapshot. UNKNOWN mergeability and
/// pending checks are not failures and never authorize a repair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrHealth {
    pub id: u64,
    pub head: String,
    pub conflicting: bool,
    pub failed_required_checks: Vec<String>,
    pub required_checks_pending: bool,
    pub failed_jobs: Vec<FailedJob>,
    pub required_jobs: Vec<FailedJob>,
}

impl PrHealth {
    pub fn needs_repair(&self) -> bool {
        self.conflicting || !self.failed_required_checks.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedJob {
    pub name: String,
    pub run_id: u64,
    pub run_attempt: u64,
    pub job_id: u64,
}
