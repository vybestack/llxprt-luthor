//! Single-candidate claim/worktree/launch protocol and scheduling ports.
use super::ports::{self, DispatchError, SupervisorLauncher};
use crate::{
    claim::{self, AssignmentWriter},
    config::Config,
    eligibility::Candidate,
    github::{
        project::ProjectReader,
        pull_request::{LookupResult, PullRequestReader, lookup},
    },
    state::{StateStore, scheduling, task_records},
    supervisor::{self, LaunchPlan, SupervisorError},
    worktree,
};

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
