//! Monitor verified PRs and dispatch only bounded, freshly authorized repairs.
use super::{
    failed_checks::{self, FailedChecks},
    ids::IdCreator,
    notes::TaskNote,
    ports::SupervisorLauncher,
    ports::{
        DispatchError, PrVerificationStage, observation_time, verify_completion_claim,
        verify_open_pr,
    },
    retirement::{self, Observation, RetiredTask, Settlement},
};
use crate::{
    claim,
    github::{
        project::ProjectReader,
        pull_request::{LookupResult, PrHealthReader, PullRequestEvidence, lookup},
    },
    ownership::{WorktreeOwner, WorktreeOwnerInternal},
    pr_repair::MAX_PR_REPAIRS,
    state::{
        StateError, StateStore, journal, pr_repair, retirement::RetiredPhase, scheduling,
        task_records,
    },
    supervisor,
};
use serde::Serialize;

/// What one repair pass did. A skipped task is untouched and is looked at again
/// next cycle; a held task was recorded as held with its reason.
#[derive(Debug, Default)]
pub struct RepairOutcome {
    pub launched: Vec<supervisor::LaunchPlan>,
    pub skipped: Vec<TaskNote>,
    pub held: Vec<TaskNote>,
    pub retired: Vec<RetiredTask>,
}

/// The cycle's account of verified-PR housekeeping, reported alongside launches.
#[derive(Debug, Default, Serialize)]
pub struct PrRepairReport {
    pub restored: Vec<String>,
    pub restore_retained: Vec<TaskNote>,
    pub skipped: Vec<TaskNote>,
    pub held: Vec<TaskNote>,
    pub retired: Vec<RetiredTask>,
    pub retire_retained: Vec<TaskNote>,
}

enum Inspection {
    Idle,
    Launched(Box<supervisor::LaunchPlan>),
    Skipped(String),
}

/// Visits every `pr_complete` task once. One task's problem is recorded against
/// that task and never stops the others; only a failure to record state aborts.
pub fn repair_verified_prs<
    P: ProjectReader,
    Q: PrHealthReader,
    L: SupervisorLauncher,
    I: IdCreator,
>(
    store: &mut StateStore,
    config: &crate::config::Config,
    projects: &mut P,
    prs: &mut Q,
    launcher: &mut L,
    ids: &mut I,
) -> Result<RepairOutcome, DispatchError> {
    repair_verified_prs_after_cleanup(
        store,
        config,
        projects,
        prs,
        launcher,
        ids,
        &Default::default(),
    )
}

/// Do not retry a failed cleanup task in PR repair during the same cycle: its
/// ownership, historical configuration or upstream proof may be unavailable.
pub fn repair_verified_prs_after_cleanup<
    P: ProjectReader,
    Q: PrHealthReader,
    L: SupervisorLauncher,
    I: IdCreator,
>(
    store: &mut StateStore,
    config: &crate::config::Config,
    projects: &mut P,
    prs: &mut Q,
    launcher: &mut L,
    ids: &mut I,
    cleanup: &crate::cleanup::CleanupReport,
) -> Result<RepairOutcome, DispatchError> {
    let mut outcome = RepairOutcome::default();
    for task in pr_repair::candidates(store)? {
        if let Some(skip) = cleanup.skipped.iter().find(|skip| skip.task_id == task) {
            outcome.skipped.push(TaskNote {
                task_id: task,
                reason: skip.reason.clone(),
            });
            continue;
        }
        match inspect_task(store, config, &task, projects, prs, launcher, ids) {
            Ok(Inspection::Launched(plan)) => outcome.launched.push(*plan),
            Ok(Inspection::Idle) => {}
            Ok(Inspection::Skipped(reason)) => outcome.skipped.push(TaskNote {
                task_id: task,
                reason,
            }),
            Err(error) => match finished_instead_of_held(store, &task, projects, prs)? {
                Resolution::Retired(retired) => outcome.retired.push(retired),
                Resolution::LeftToCleanup(reason) => outcome.skipped.push(TaskNote {
                    task_id: task,
                    reason,
                }),
                Resolution::Active => {
                    let reason = hold_reason(&error);
                    task_records::hold_task(store, &task, &reason)?;
                    outcome.held.push(TaskNote {
                        task_id: task,
                        reason,
                    });
                }
            },
        }
    }
    Ok(outcome)
}

enum Resolution {
    Retired(RetiredTask),
    /// The saved PR is finished; terminal cleanup, which removes the worktree
    /// and branch, retires the task once the open-PR search agrees.
    LeftToCleanup(String),
    Active,
}

/// A failed repair check on a task whose work is already over is not a safety
/// conflict: the closed issue or merged PR is the cause. Anything unproven
/// stays a hold.
fn finished_instead_of_held<P: ProjectReader, Q: PrHealthReader>(
    store: &mut StateStore,
    task: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Result<Resolution, StateError> {
    let Observation::Finished(finished) = retirement::observe_finished(store, task, projects, prs)?
    else {
        return Ok(Resolution::Active);
    };
    if matches!(
        finished.phase,
        RetiredPhase::PrMerged | RetiredPhase::PrClosed
    ) {
        return Ok(Resolution::LeftToCleanup(format!(
            "{}; terminal cleanup retires the task",
            finished.reason
        )));
    }
    match retirement::settle(store, task, &finished)? {
        Settlement::Retired(retired) => Ok(Resolution::Retired(retired)),
        Settlement::Blocked(_) | Settlement::Unfinished | Settlement::Unavailable(_) => {
            Ok(Resolution::Active)
        }
    }
}

/// Bounded to the failing stage: upstream error text never enters task evidence.
fn hold_reason(error: &DispatchError) -> String {
    let cause = match error {
        DispatchError::PullRequest(lookup) => {
            format!("PR or check state could not be verified ({lookup})")
        }
        DispatchError::Claim(_) | DispatchError::ChangedClaim => {
            "issue claim changed or could not be re-read".into()
        }
        DispatchError::Worktree(_) => "worktree identity or tip did not verify".into(),
        DispatchError::Supervisor(supervisor::SupervisorError::WorkerBuildHeld(reason)) => {
            reason.clone()
        }
        DispatchError::Supervisor(_) => {
            "PR, process, receipt or ownership evidence did not verify".into()
        }
        DispatchError::State(StateError::LaunchBlocked) => {
            "recorded context does not allow a repair launch (ambiguous PR or unverified history)"
                .into()
        }
        DispatchError::State(_) => "task state could not be validated".into(),
        DispatchError::RetryHeld { .. } | DispatchError::ExistingPr => {
            "repair launch was refused".into()
        }
    };
    format!("PR repair held: {cause}")
}

/// The PR is looked up before anything else: a task whose PR is no longer open
/// is finished and must not be judged against today's configuration.
fn inspect_task<P: ProjectReader, Q: PrHealthReader, L: SupervisorLauncher, I: IdCreator>(
    store: &mut StateStore,
    config: &crate::config::Config,
    task: &str,
    projects: &mut P,
    prs: &mut Q,
    launcher: &mut L,
    ids: &mut I,
) -> Result<Inspection, DispatchError> {
    let saved = pr_repair::saved_pr(&store.connection, task)?;
    // Exhaustive absence covers a merged/closed PR and leaves cleanup to its
    // separate exact terminal-PR proof. It must never trigger a new PR.
    let pr = match lookup(prs, &saved.repository, &saved.tracker_issue_url) {
        Ok(LookupResult::Absent) => return Ok(Inspection::Idle),
        Ok(LookupResult::OpenPreexisting(pr)) => *pr,
        Ok(LookupResult::Ambiguous(_)) => return Err(StateError::LaunchBlocked.into()),
        // Unknown is not a conflict; nothing changes and the next cycle asks again.
        Err(error) => {
            return Ok(Inspection::Skipped(format!(
                "PR lookup unavailable ({error}); will retry next cycle"
            )));
        }
    };
    let (previous, _) = pr_repair::context(store, task)?;
    let selection = crate::state::selection_for_attempt(store, &previous)?;
    let mut comparable = crate::model::EffectiveConfigSnapshot::from(config);
    comparable.initial.executable = selection.effective_config.initial.executable.clone();
    comparable.resume.executable = selection.effective_config.resume.executable.clone();
    let drift = selection.effective_config.differing_fields(&comparable);
    if !drift.is_empty() {
        return Ok(Inspection::Skipped(format!(
            "selection config differs: {}",
            drift.join(", ")
        )));
    }
    inspect_open_pr(store, task, (saved, pr), (projects, prs), (launcher, ids))
}

fn inspect_open_pr<P: ProjectReader, Q: PrHealthReader, L: SupervisorLauncher, I: IdCreator>(
    store: &mut StateStore,
    task: &str,
    (saved, pr): (crate::model::VerifiedOpenPr, PullRequestEvidence),
    (projects, prs): (&mut P, &mut Q),
    (launcher, ids): (&mut L, &mut I),
) -> Result<Inspection, DispatchError> {
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    let observed = verify_open_pr(
        store,
        task,
        &saved.attempt_id,
        prs,
        (pr, observation_time()?),
        PrVerificationStage::Exit,
    )
    .map_err(|_| supervisor::SupervisorError::Conflict)?;
    if observed.id != saved.id
        || observed.number != saved.number
        || observed.head_commit_sha != saved.head_commit_sha
    {
        return Err(supervisor::SupervisorError::Conflict.into());
    }
    verify_completion_claim(projects, &selection)?;
    let health = prs.health(&saved.repository, saved.number)?;
    if health.id != observed.id || health.head != observed.head_commit_sha {
        return Err(supervisor::SupervisorError::Conflict.into());
    }
    journal::record_evidence(
        store,
        task,
        Some(&saved.attempt_id),
        "pr_health",
        &serde_json::to_string(&health).map_err(StateError::from)?,
    )?;
    if limit_reached(store, task, &health)? {
        return Ok(Inspection::Idle);
    }
    let excerpts = match failed_checks::inspect(store, task, &saved, &health, prs)? {
        FailedChecks::Waiting => return Ok(Inspection::Idle),
        FailedChecks::Ready(excerpts) => excerpts,
    };
    if !health.needs_repair() {
        return Ok(Inspection::Idle);
    }
    match scheduling::ensure_dispatch_capacity(store) {
        Ok(()) => {}
        Err(StateError::Capacity { .. }) => return Ok(Inspection::Idle),
        Err(error) => return Err(error.into()),
    }
    launch_repair(
        store,
        task,
        projects,
        launcher,
        ids,
        (observed, health, excerpts),
    )
    .map(|plan| Inspection::Launched(Box::new(plan)))
}

fn launch_repair<P: ProjectReader, L: SupervisorLauncher, I: IdCreator>(
    store: &mut StateStore,
    task: &str,
    projects: &mut P,
    launcher: &mut L,
    ids: &mut I,
    evidence: (
        crate::model::VerifiedOpenPr,
        crate::github::pull_request::PrHealth,
        Vec<String>,
    ),
) -> Result<supervisor::LaunchPlan, DispatchError> {
    let (observed, health, excerpts) = evidence;
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    let (previous, _) = pr_repair::context(store, task)?;
    let owner = WorktreeOwner::acquire_existing(store.root(), task)
        .map_err(|_| supervisor::SupervisorError::Conflict)?;
    owner
        .verify_protocol(&store.connection, store.root(), task, &previous.attempt_id)
        .map_err(|_| supervisor::SupervisorError::Conflict)?;
    let mut terminal = None;
    if !matches!(
        supervisor::recheck_retry_exit(
            store,
            task,
            &previous.attempt_id,
            &owner,
            false,
            &mut terminal
        )?,
        supervisor::Reconciliation::Completed { .. }
    ) {
        return Err(supervisor::SupervisorError::Conflict.into());
    }
    let (item, issue) = claim::fresh(projects, &selection.candidate)?;
    if issue.assignees != [selection.effective_config.assignment_login.as_str()] {
        return Err(DispatchError::ChangedClaim);
    }
    let source = crate::model::RetrySourceEvidence {
        item,
        issue,
        claim: task_records::source_claim_intent(store, task)?
            .ok_or(DispatchError::ChangedClaim)?,
        observed_at_unix_secs: observation_time()?,
    };
    let attempt = format!(
        "attempt-{}",
        ids.create().map_err(supervisor::SupervisorError::from)?
    );
    let plan = supervisor::prepare_pr_repair(store, &attempt, observed, health, source, excerpts)?;
    launcher.launch(store, &plan, &owner)?;
    Ok(plan)
}

fn limit_reached(
    store: &mut StateStore,
    task: &str,
    health: &crate::github::pull_request::PrHealth,
) -> Result<bool, DispatchError> {
    if health.needs_repair() && pr_repair::count(&store.connection, task)? >= MAX_PR_REPAIRS {
        task_records::hold_task(
            store,
            task,
            &format!(
                "PR repair attempt limit reached; requires a person; failing checks: {:?}",
                health.failed_required_checks
            ),
        )?;
        return Ok(true);
    }
    Ok(false)
}
