use crate::{
    claim::{self, AssignmentWriter, ClaimError},
    config::Config,
    eligibility::Candidate,
    github::{
        project::ProjectReader,
        pull_request::{LookupError, LookupResult, PullRequestReader, lookup},
    },
    state::{StateError, StateStore},
    supervisor::{self, LaunchPlan, SupervisorError},
    worktree::{self, WorktreeError},
};
use std::{collections::HashSet, io::Read};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptReview {
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
pub fn startup_reconcile_all(store: &mut StateStore) -> Result<StartupReport, StateError> {
    let mut report = StartupReport::default();
    for (task_id, attempt_id) in store.pending_attempts()? {
        let review = match supervisor::reconcile_attempt(store, &task_id, &attempt_id) {
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
    report.source_holds = store
        .unresolved_sources()?
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

pub trait SupervisorLauncher {
    fn launch(&mut self, store: &mut StateStore, plan: &LaunchPlan) -> Result<(), SupervisorError>;
}

pub struct ProductionLauncher;

impl SupervisorLauncher for ProductionLauncher {
    fn launch(&mut self, store: &mut StateStore, plan: &LaunchPlan) -> Result<(), SupervisorError> {
        supervisor::execute(store, plan)
    }
}

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Claim(#[from] ClaimError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    PullRequest(#[from] LookupError),
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error("claim changed before launch")]
    ChangedClaim,
    #[error("pull request is no longer absent")]
    ExistingPr,
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
    store.ensure_dispatch_capacity()?;
    store.create_task(task_id, candidate, config_revision, config)?;
    let result = (|| {
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
        launcher.launch(store, &plan)?;
        Ok(plan)
    })();
    if let Err(error) = &result {
        // Keep the reason bounded to a stage/type: external error strings may carry credentials.
        let reason = match error {
            DispatchError::Claim(_) => "claim failed",
            DispatchError::Worktree(_) => "worktree failed",
            DispatchError::ChangedClaim => "prelaunch claim changed",
            DispatchError::ExistingPr => "prelaunch PR present",
            DispatchError::PullRequest(_) => "prelaunch PR read failed",
            DispatchError::Supervisor(_) => "launch preparation or dispatch failed",
            DispatchError::State(_) => "state transition failed",
        };
        store.hold_task(task_id, reason)?;
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
        startup: startup_reconcile_all(store)?,
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
        if !seen.insert(identity.clone()) || store.existing_issue(&identity.0, &identity.1)? {
            report.skipped_existing.push(identity);
            continue;
        }
        if !store.unresolved_sources()?.is_empty() {
            report.startup.source_holds = store
                .unresolved_sources()?
                .into_iter()
                .map(|(task_id, kind)| SourceHold { task_id, kind })
                .collect();
            break;
        }
        match store.ensure_dispatch_capacity() {
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
    store.resume_context(task_id)?;
    let result = (|| {
        let selection = store
            .selection_evidence(task_id)?
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
        launcher.launch(store, &plan)?;
        Ok(plan)
    })();
    if let Err(error) = &result {
        let reason = match error {
            DispatchError::Claim(_) | DispatchError::ChangedClaim => "resume claim changed",
            DispatchError::ExistingPr => "resume PR present",
            DispatchError::PullRequest(_) => "resume PR read failed",
            DispatchError::Supervisor(_) => "resume preparation or dispatch failed",
            DispatchError::State(_) => "resume state transition failed",
            DispatchError::Worktree(_) => "resume worktree failed",
        };
        store.hold_task(task_id, reason)?;
    }
    result
}
