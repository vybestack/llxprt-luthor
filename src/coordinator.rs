use crate::state::{launches, scheduling, task_records};
mod active;
mod auto_retry;
mod worker_outcome;
pub use auto_retry::{
    AutoRetryDependencies, AutoRetryReport, RetryObservation, retry_attention_tasks,
};
pub use worker_outcome::{OutcomeObservation, RetryDecision, WorkerOutcome};
mod failed_checks;
mod notes;
pub use notes::TaskNote;
mod pr_head;
pub(crate) mod pr_merge;
mod pr_repair;
mod repair_hold;
mod repair_retry;
pub use pr_merge::{MergeError, MergeReport, merge_verified_prs};
pub use pr_repair::{
    PrRepairReport, RepairOutcome, repair_verified_prs, repair_verified_prs_after_cleanup,
};
mod pr_restore;
pub use pr_restore::{RestoreReport, restore_open_held_prs, restore_wrongly_held_prs};
mod reconciliation;
mod recovery;
pub(crate) mod retirement;
pub use retirement::{RetiredTask, RetirementReport, abandon_task, retire_finished_tasks};
mod recovery_commit;
mod source_dispatch;
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
mod diagnostics;
mod ports;
pub mod requests;
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
pub use diagnostics::operator_error;
pub use ports::{DispatchError, SupervisorLauncher};
pub use retry::{RetryDependencies, retry_one};
use std::collections::HashSet;
mod ids;
pub use ids::{IdCreator, OsIdCreator};

mod reports;
pub use recovery_commit::RecoveryResult;
pub use reports::{AttemptReport, AttemptReview, ScheduleReport, SourceHold, StartupReport};

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
    let tasks = scheduling::unresolved_sources(store)?
        .into_iter()
        .map(|(task, _)| task)
        .collect::<std::collections::BTreeSet<_>>();
    for task in tasks {
        if task_records::latest_attempt(store, &task)?.is_none() {
            reconcile_source(store, &task, projects)?;
        }
    }
    report.source_holds = scheduling::unresolved_sources(store)?
        .into_iter()
        .map(|(task_id, kind)| SourceHold { task_id, kind })
        .collect();
    Ok(report)
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

pub use ports::ScheduleError;

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
    claim::ensure_dependencies(projects, candidate)?;
    scheduling::ensure_dispatch_capacity(store)?;
    crate::disk::inspect(&config.worktree_root, config.min_free_space_bytes)?.enforce()?;
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
        let plan = supervisor::prepare_initial_owned(store, task_id, attempt_id, &ownership)?;
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
    let mut d = dependencies;
    let mut report = ScheduleReport {
        startup,
        ..Default::default()
    };
    if report.startup.scheduling_blocked() {
        return Ok(report);
    }
    schedule_continuations(store, &mut report, &mut d)?;
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
        let dependencies =
            claim::dependency_waiting(d.projects, &candidate).map_err(DispatchError::Claim)?;
        if !dependencies.is_empty() {
            report.waiting.push(crate::eligibility::WaitingIssue {
                candidate,
                dependencies,
            });
            continue;
        }
        match scheduling::ensure_dispatch_capacity(store) {
            Ok(()) => {}
            Err(StateError::Capacity { .. }) => {
                report.capacity_full = true;
                break;
            }
            Err(error) => return Err(error.into()),
        }
        if record_dispatch_space(d.config, &mut report)? {
            break;
        }
        let task_id = format!("task-{}", d.ids.create()?);
        let attempt_id = format!("attempt-{}", d.ids.create()?);
        let plan = dispatch_one(
            store,
            &candidate,
            DispatchDependencies {
                config: d.config,
                config_revision: d.config_revision,
                task_id: &task_id,
                attempt_id: &attempt_id,
                projects: d.projects,
                prs: d.prs,
                assignments: d.assignments,
                launcher: d.launcher,
            },
        )?;
        report.launched.push(plan);
    }
    Ok(report)
}

fn schedule_continuations<P, Q, W, L, I>(
    store: &mut StateStore,
    report: &mut ScheduleReport,
    d: &mut ScheduleDependencies<'_, P, Q, W, L, I>,
) -> Result<(), ScheduleError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    W: AssignmentWriter,
    L: SupervisorLauncher,
    I: IdCreator,
{
    let retries = retry_attention_tasks(
        store,
        AutoRetryDependencies {
            config: d.config,
            config_revision: d.config_revision,
            projects: d.projects,
            prs: d.prs,
            launcher: d.launcher,
            ids: d.ids,
        },
    )?;
    report.automatic_retries = retries.observations;
    report.launched.extend(retries.launched);
    source_dispatch::dispatch_reconciled_sources(
        store,
        report,
        source_dispatch::SourceDispatchDependencies {
            config: d.config,
            projects: d.projects,
            prs: d.prs,
            launcher: d.launcher,
            ids: d.ids,
        },
    )?;
    Ok(())
}

fn record_dispatch_space(
    config: &Config,
    report: &mut ScheduleReport,
) -> Result<bool, DispatchError> {
    let space = crate::disk::inspect(&config.worktree_root, config.min_free_space_bytes)?;
    let held = space.dispatch_held;
    report.disk_space = Some(space);
    Ok(held)
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

pub fn resume_one<P: ProjectReader, Q: PullRequestReader, L: SupervisorLauncher>(
    store: &mut StateStore,
    dependencies: ResumeDependencies<'_, P, Q, L>,
) -> Result<LaunchPlan, DispatchError> {
    resume_with_policy(store, dependencies, false)
}

/// Only a durable queued decision may wait through read-only transient errors.
/// One-shot direct resumes retain their existing hold-on-read-failure contract.
pub(crate) fn resume_pending<P: ProjectReader, Q: PullRequestReader, L: SupervisorLauncher>(
    store: &mut StateStore,
    dependencies: ResumeDependencies<'_, P, Q, L>,
) -> Result<LaunchPlan, DispatchError> {
    resume_with_policy(store, dependencies, true)
}

fn resume_with_policy<P, Q, L>(
    store: &mut StateStore,
    dependencies: ResumeDependencies<'_, P, Q, L>,
    defer_transient_reads: bool,
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
        scheduling::ensure_dispatch_capacity(store)?;
        let plan = supervisor::prepare_resume(store, task_id, attempt_id)?;
        launcher.launch(store, &plan, &ownership)?;
        Ok(plan)
    })();
    if let Err(error) = &result {
        if ports::defer_resume_error(error, defer_transient_reads) {
            return result;
        }
        let reason = match error {
            DispatchError::Claim(_) | DispatchError::ChangedClaim => "resume claim changed",
            DispatchError::ExistingPr => "resume PR present",
            DispatchError::PullRequest(_) => "resume PR read failed",
            DispatchError::Supervisor(_) => "resume preparation or dispatch failed",
            DispatchError::RetryHeld { .. } => "retry held before launch",
            DispatchError::State(_) => "resume state transition failed",
            DispatchError::Worktree(_) => "resume worktree failed",
        };
        let reason = match error {
            DispatchError::Supervisor(SupervisorError::WorkerBuildHeld(detail)) => detail.as_str(),
            _ => reason,
        };
        task_records::hold_task(store, task_id, reason)?;
    }
    result
}
