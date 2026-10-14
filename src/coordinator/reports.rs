use super::{pr_repair::PrRepairReport, retirement::RetiredTask};
use crate::{
    state::{StateError, StateStore, scheduling},
    supervisor::LaunchPlan,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptReview {
    Running,
    Completed(crate::supervisor::Reconciliation),
    Held(crate::supervisor::Reconciliation),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptReport {
    pub task_id: String,
    pub attempt_id: String,
    pub review: AttemptReview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceHold {
    pub task_id: String,
    pub kind: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StartupReport {
    pub attempts: Vec<AttemptReport>,
    pub source_holds: Vec<SourceHold>,
}

impl StartupReport {
    /// Forget what reconciliation reported for tasks retired afterwards, and
    /// recompute the source holds their retirement may have cleared.
    pub fn settle_retired(
        &mut self,
        store: &StateStore,
        retired: &[RetiredTask],
    ) -> Result<(), StateError> {
        self.attempts
            .retain(|attempt| !retired.iter().any(|task| task.task_id == attempt.task_id));
        self.source_holds = scheduling::unresolved_sources(store)?
            .into_iter()
            .map(|(task_id, kind)| SourceHold { task_id, kind })
            .collect();
        Ok(())
    }

    pub fn scheduling_blocked(&self) -> bool {
        self.source_holds.iter().any(|hold| hold.kind == "stop")
            || self
                .attempts
                .iter()
                .any(|attempt| matches!(attempt.review, AttemptReview::Error(_)))
    }
}

#[derive(Debug, Default)]
pub struct ScheduleReport {
    pub startup: StartupReport,
    pub launched: Vec<LaunchPlan>,
    pub skipped_existing: Vec<(String, String)>,
    pub capacity_full: bool,
    pub disk_space: Option<crate::disk::DiskSpace>,
    pub pr_repair: PrRepairReport,
    pub automatic_retries: Vec<super::auto_retry::RetryObservation>,
    pub candidate_read_failures: Vec<crate::github::project::CandidateReadFailure>,
    pub source_read_failures: Vec<crate::github::project::SourceReadFailure>,
}
