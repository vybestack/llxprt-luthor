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
use thiserror::Error;

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
