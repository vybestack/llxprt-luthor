use crate::state::{launches, scheduling, task_records};
mod active;
mod reconciliation;
mod recovery;
mod recovery_commit;
mod source_observation;
pub use reconciliation::reconcile_with_pr;
pub use recovery::operator_recover_missing_receipt;
pub use source_observation::{SourceReconciliation, reconcile_source};
mod continuation;
pub use continuation::{
    AmendedContinuationDependencies, AmendedSupervisorLauncher, ContinuationDependencies,
    ContinuationLocalInspector, ContinuationProcessInspector, ContinuationRefusal,
    ContinuationResult, NativeAmendedSupervisorLauncher, OsContinuationLocalInspector,
    OsContinuationProcessInspector, ProcessInspectionError, amend_never_dispatched_initial_branch,
    continue_never_dispatched,
};
mod ports;
mod retry;
use crate::{
    claim::{self, AssignmentWriter},
    config::Config,
    eligibility::Candidate,
    github::{
        project::ProjectReader,
        pull_request::{LookupResult, PullRequestReader, lookup},
    },
    state::{StateError, StateStore},
    supervisor::{self, LaunchPlan, SupervisorError},
    worktree,
};
pub use ports::{DispatchError, SupervisorLauncher};
pub use retry::{RetryDependencies, retry_one};
use std::{collections::HashSet, io::Read};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptReview {
    Running,
    Completed(supervisor::Reconciliation),
    Held(supervisor::Reconciliation),
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

pub use recovery_commit::RecoveryResult;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StartupReport {
    pub attempts: Vec<AttemptReport>,
    pub source_holds: Vec<SourceHold>,
}

impl StartupReport {
    pub fn scheduling_blocked(&self) -> bool {
        !self.source_holds.is_empty()
            || self
                .attempts
                .iter()
                .any(|attempt| matches!(attempt.review, AttemptReview::Error(_)))
    }
}

/// Reconcile every durable nonterminal attempt, even if a prior attempt cannot
/// be verified. An error never releases its reservation and cannot be ignored by
/// the scheduler. Source operations require proof before new selection.
pub fn startup_reconcile_all<P: ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    projects: &mut P,
    prs: &mut Q,
) -> Result<StartupReport, StateError> {
    let mut report = StartupReport::default();
    for (task_id, attempt_id) in scheduling::pending_attempts(store)? {
        let review = match reconcile_with_pr(store, &task_id, &attempt_id, projects, prs) {
            Ok(supervisor::Reconciliation::Running) => AttemptReview::Running,
            Ok(result @ supervisor::Reconciliation::Completed { .. }) => {
                AttemptReview::Completed(result)
            }
            Ok(result @ supervisor::Reconciliation::Held { .. }) => AttemptReview::Held(result),
            Err(error) => AttemptReview::Error(error.to_string()),
        };
        report.attempts.push(AttemptReport {
            task_id,
            attempt_id,
            review,
        });
    }
    report.source_holds = scheduling::unresolved_sources(store)?
        .into_iter()
        .map(|(task_id, kind)| SourceHold { task_id, kind })
        .collect();
    Ok(report)
}

pub trait IdCreator {
    fn create(&mut self) -> Result<String, std::io::Error>;
}

pub struct OsIdCreator;

impl IdCreator for OsIdCreator {
    fn create(&mut self) -> Result<String, std::io::Error> {
        let mut bytes = [0u8; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

pub struct ProductionLauncher;

impl SupervisorLauncher for ProductionLauncher {
    fn launch(
        &mut self,
        store: &mut StateStore,
        plan: &LaunchPlan,
        ownership: &crate::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        supervisor::execute(store, plan, ownership)
    }
}

#[derive(Debug, Error)]
pub enum ScheduleError {
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("ID generation failed: {0}")]
    Id(#[from] std::io::Error),
}

pub struct DispatchDependencies<'a, P, Q, W, L> {
    pub config: &'a Config,
    pub config_revision: &'a str,
    pub task_id: &'a str,
    pub attempt_id: &'a str,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub assignments: &'a mut W,
    pub launcher: &'a mut L,
}

/// Dispatches a single preselected issue. Callers must reconcile outstanding
/// intents before scheduling; this path never resumes or retries a prior task.
pub fn dispatch_one<P, Q, W, L>(
    store: &mut StateStore,
    candidate: &Candidate,
    dependencies: DispatchDependencies<'_, P, Q, W, L>,
) -> Result<LaunchPlan, DispatchError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    W: AssignmentWriter,
    L: SupervisorLauncher,
{
    let DispatchDependencies {
        config,
        config_revision,
        task_id,
        attempt_id,
        projects,
        prs,
        assignments,
        launcher,
    } = dependencies;
    scheduling::ensure_dispatch_capacity(store)?;
    let ownership = crate::ownership::WorktreeOwner::acquire(store.root(), task_id)
        .map_err(|_| SupervisorError::Conflict)?;
    task_records::create_task(store, task_id, candidate, config_revision, config)?;
    let mut launch_preflight_complete = false;
    let result = (|| {
        supervisor::validate_stop_socket_path(&config.state_root, attempt_id)?;
        supervisor::preflight_attempt_storage(&config.state_root)?;
        launch_preflight_complete = true;
        worktree::preflight(task_id, &config.worktree_root, &candidate.mapping)?;
        claim::claim(
            store,
            task_id,
            candidate,
            &config.assignment_login,
            projects,
            prs,
            assignments,
        )?;
        worktree::ensure_worktree(store, task_id, &config.worktree_root, &candidate.mapping)?;
        let (_, issue) = claim::fresh(projects, candidate)?;
        if issue.assignees != [config.assignment_login.as_str()] {
            return Err(DispatchError::ChangedClaim);
        }
        if lookup(
            prs,
            &candidate.mapping.code_repository,
            &candidate.issue_url,
        )? != LookupResult::Absent
        {
            return Err(DispatchError::ExistingPr);
        }
        let plan = supervisor::prepare_initial(store, task_id, attempt_id)?;
        launcher.launch(store, &plan, &ownership)?;
        Ok(plan)
    })();
    if let Err(error) = &result {
        task_records::hold_task(
            store,
            task_id,
            ports::dispatch_failure_reason(error, launch_preflight_complete),
        )?;
    }
    result
}

pub struct ScheduleDependencies<'a, P, Q, W, L, I> {
    pub config: &'a Config,
    pub config_revision: &'a str,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub assignments: &'a mut W,
    pub launcher: &'a mut L,
    pub ids: &'a mut I,
}

#[derive(Debug, Default)]
pub struct ScheduleReport {
    pub startup: StartupReport,
    pub launched: Vec<LaunchPlan>,
    pub skipped_existing: Vec<(String, String)>,
    pub capacity_full: bool,
}

/// Selected candidates are finite; no held or previously selected issue is
/// retried. A failed dispatch stops this pass without trying another worker.
pub fn schedule_candidates<P, Q, W, L, I>(
    store: &mut StateStore,
    candidates: Vec<Candidate>,
    dependencies: ScheduleDependencies<'_, P, Q, W, L, I>,
) -> Result<ScheduleReport, ScheduleError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    W: AssignmentWriter,
    L: SupervisorLauncher,
    I: IdCreator,
{
    let startup = startup_reconcile_all(store, dependencies.projects, dependencies.prs)?;
    schedule_candidates_after_startup(store, candidates, dependencies, startup)
}

/// Schedule candidates using startup reconciliation already performed by the caller.
pub fn schedule_candidates_after_startup<P, Q, W, L, I>(
    store: &mut StateStore,
    candidates: Vec<Candidate>,
    dependencies: ScheduleDependencies<'_, P, Q, W, L, I>,
    startup: StartupReport,
) -> Result<ScheduleReport, ScheduleError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    W: AssignmentWriter,
    L: SupervisorLauncher,
    I: IdCreator,
{
    let ScheduleDependencies {
        config,
        config_revision,
        projects,
        prs,
        assignments,
        launcher,
        ids,
    } = dependencies;
    let mut report = ScheduleReport {
        startup,
        ..Default::default()
    };
    if report.startup.scheduling_blocked() {
        return Ok(report);
    }
    let mut seen = HashSet::new();
    for candidate in candidates {
        let identity = (
            candidate.tracker_repo_id.clone(),
            candidate.issue_node_id.clone(),
        );
        if !seen.insert(identity.clone())
            || task_records::existing_issue(store, &identity.0, &identity.1)?
        {
            report.skipped_existing.push(identity);
            continue;
        }
        if !scheduling::unresolved_sources(store)?.is_empty() {
            report.startup.source_holds = scheduling::unresolved_sources(store)?
                .into_iter()
                .map(|(task_id, kind)| SourceHold { task_id, kind })
                .collect();
            break;
        }
        match scheduling::ensure_dispatch_capacity(store) {
            Ok(()) => {}
            Err(StateError::Capacity { .. }) => {
                report.capacity_full = true;
                break;
            }
            Err(error) => return Err(error.into()),
        }
        let task_id = format!("task-{}", ids.create()?);
        let attempt_id = format!("attempt-{}", ids.create()?);
        let plan = dispatch_one(
            store,
            &candidate,
            DispatchDependencies {
                config,
                config_revision,
                task_id: &task_id,
                attempt_id: &attempt_id,
                projects,
                prs,
                assignments,
                launcher,
            },
        )?;
        report.launched.push(plan);
    }
    Ok(report)
}

pub struct ResumeDependencies<'a, P, Q, L> {
    pub task_id: &'a str,
    pub attempt_id: &'a str,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub launcher: &'a mut L,
}

/// Continues a previously reconciled, paused task using only its stored selection.
/// Failed evidence checks hold the task without reserving another attempt.
fn acquire_resume_owner(
    store: &StateStore,
    task_id: &str,
) -> Result<crate::ownership::WorktreeOwner, SupervisorError> {
    let ownership = crate::ownership::WorktreeOwner::acquire_existing(store.root(), task_id)
        .map_err(|_| SupervisorError::Conflict)?;
    let prior_attempt: Option<String> = store
        .connection
        .query_row(
            "SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .map_err(|_| SupervisorError::Conflict)?;
    let prior_attempt = prior_attempt.ok_or(SupervisorError::Conflict)?;
    crate::ownership::WorktreeOwnerInternal::verify_protocol(
        &ownership,
        &store.connection,
        store.root(),
        task_id,
        &prior_attempt,
    )
    .map_err(|_| SupervisorError::Conflict)?;
    Ok(ownership)
}

pub fn resume_one<P, Q, L>(
    store: &mut StateStore,
    dependencies: ResumeDependencies<'_, P, Q, L>,
) -> Result<LaunchPlan, DispatchError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    L: SupervisorLauncher,
{
    let ResumeDependencies {
        task_id,
        attempt_id,
        projects,
        prs,
        launcher,
    } = dependencies;
    // Do not change the phase of an active or unverified task to held.
    launches::resume_context(store, task_id)?;
    let ownership = acquire_resume_owner(store, task_id)?;
    let result = (|| {
        supervisor::validate_stop_socket_path(store.root(), attempt_id)?;
        let selection = task_records::selection_evidence(store, task_id)?
            .ok_or(StateError::InvalidSelection)?;
        let candidate = &selection.candidate;
        let (_, issue) = claim::fresh(projects, candidate)?;
        if selection
            .effective_config
            .assignment_login
            .trim()
            .is_empty()
            || issue.assignees != [selection.effective_config.assignment_login.as_str()]
        {
            return Err(DispatchError::ChangedClaim);
        }
        if lookup(
            prs,
            &candidate.mapping.code_repository,
            &candidate.issue_url,
        )? != LookupResult::Absent
        {
            return Err(DispatchError::ExistingPr);
        }
        let plan = supervisor::prepare_resume(store, task_id, attempt_id)?;
        launcher.launch(store, &plan, &ownership)?;
        Ok(plan)
    })();
    if let Err(error) = &result {
        let reason = match error {
            DispatchError::Claim(_) | DispatchError::ChangedClaim => "resume claim changed",
            DispatchError::ExistingPr => "resume PR present",
            DispatchError::PullRequest(_) => "resume PR read failed",
            DispatchError::Supervisor(_) => "resume preparation or dispatch failed",
            DispatchError::RetryHeld { .. } => "retry held before launch",
            DispatchError::State(_) => "resume state transition failed",
            DispatchError::Worktree(_) => "resume worktree failed",
        };
        task_records::hold_task(store, task_id, reason)?;
    }
    result
}
