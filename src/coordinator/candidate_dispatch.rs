use super::{
    admission::{TaskAdmissionFailure, task_local},
    dispatch::{DispatchDependencies, ScheduleDependencies, dispatch_one},
    ports::{ScheduleError, SupervisorLauncher},
    reports::ScheduleReport,
};
use crate::{
    claim::AssignmentWriter,
    eligibility::Candidate,
    github::{project::ProjectReader, pull_request::PullRequestReader},
    state::{StateStore, task_records},
};

/// Continue only after an expected refusal with no launch intent. An uncertain
/// assignment remains held by dispatch_one; never replay it in this pass.
pub(crate) fn dispatch_candidate<P, Q, W, L, I>(
    store: &mut StateStore,
    candidate: &Candidate,
    task_id: &str,
    attempt_id: &str,
    report: &mut ScheduleReport,
    d: &mut ScheduleDependencies<'_, P, Q, W, L, I>,
) -> Result<(), ScheduleError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    W: AssignmentWriter,
    L: SupervisorLauncher,
{
    let result = dispatch_one(
        store,
        candidate,
        DispatchDependencies {
            config: d.config,
            config_revision: d.config_revision,
            task_id,
            attempt_id,
            projects: d.projects,
            prs: d.prs,
            assignments: d.assignments,
            launcher: d.launcher,
        },
    );
    match result {
        Ok(plan) => report.launched.push(plan),
        Err(error) => {
            if !task_local(&error)
                || task_records::task_phase(store, task_id)?.is_none()
                || task_records::latest_attempt(store, task_id)?.is_some()
            {
                return Err(ScheduleError::Task {
                    task_id: task_id.into(),
                    attempt_id: attempt_id.into(),
                    check: "candidate_dispatch",
                    source: Box::new(error),
                });
            }
            report.admission_failures.push(TaskAdmissionFailure::record(
                store,
                task_id,
                attempt_id,
                "candidate_admission",
                &error,
            )?);
        }
    }
    Ok(())
}
