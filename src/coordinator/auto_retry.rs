//! Bounded automation for released attention tasks; all launch authority stays
//! in the existing audited operator-retry path.
use super::{
    IdCreator, RetryDependencies, SupervisorLauncher,
    ports::{DispatchError, ScheduleError, observation_time},
    retry::retry_with_owner,
    worker_outcome::{OutcomeObservation, RetryDecision, WorkerOutcome, decision, observe_outcome},
};
use crate::{
    config::{Config, RetryPolicy},
    github::{project::ProjectReader, pull_request::PullRequestReader},
    model::LaunchPlan,
    ownership::{OwnershipError, WorktreeOwner},
    state::{StateError, StateStore, journal, scheduling},
};
use serde::Serialize;
use serde_json::json;

#[derive(Debug, Clone, Serialize)]
pub struct RetryObservation {
    pub task_id: String,
    pub attempt_id: String,
    pub outcome: WorkerOutcome,
    pub decision: RetryDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<super::admission::TaskAdmissionFailure>,
}

#[derive(Debug, Default)]
pub struct AutoRetryReport {
    pub observations: Vec<RetryObservation>,
    pub launched: Vec<LaunchPlan>,
}

pub struct AutoRetryDependencies<'a, P, Q, L, I> {
    pub config: &'a Config,
    pub config_revision: &'a str,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub launcher: &'a mut L,
    pub ids: &'a mut I,
}

/// Called only by execute scheduling, never by preview, reconcile, or resume.
/// No assignment or new worktree is created. Current policy applies to existing
/// tasks; its exact value is audited separately from immutable selection config.
pub fn retry_attention_tasks<
    P: ProjectReader,
    Q: PullRequestReader,
    L: SupervisorLauncher,
    I: IdCreator,
>(
    store: &mut StateStore,
    dependencies: AutoRetryDependencies<'_, P, Q, L, I>,
) -> Result<AutoRetryReport, ScheduleError> {
    let mut d = dependencies;
    d.config
        .validate()
        .map_err(crate::supervisor::SupervisorError::from)
        .map_err(DispatchError::from)?;
    let mut report = AutoRetryReport::default();
    if matches!(d.config.retry_policy, RetryPolicy::Disabled { .. }) {
        return Ok(report);
    }
    let now = observation_time().map_err(DispatchError::from)?;
    for (task, attempt, count) in attention_tasks(store)? {
        let (plan, receipt) = match crate::state::retry_context_for_task(store, &task, &attempt) {
            Ok(context) => context,
            Err(StateError::LaunchBlocked) => {
                report.observations.push(RetryObservation {
                    task_id: task,
                    attempt_id: attempt,
                    outcome: WorkerOutcome::Unclassified,
                    decision: RetryDecision::Ineligible,
                    failure: None,
                });
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let observation = observe_outcome(store, &plan, &receipt, now)
            .map_err(|source| task_error(&plan, "worker_outcome", source))?;
        let mut failure = None;
        let decision = decision(&d.config.retry_policy, &observation, count, now)?;
        let decision = if let Some(decision) = decision {
            decision
        } else {
            match scheduling::ensure_dispatch_capacity(store) {
                Ok(()) => retry_decision(
                    store,
                    &plan,
                    &observation,
                    count,
                    &mut d,
                    &mut report,
                    &mut failure,
                )?,
                Err(StateError::Capacity { .. }) => RetryDecision::CapacityFull,
                Err(error) => return Err(error.into()),
            }
        };
        report.observations.push(RetryObservation {
            task_id: task,
            attempt_id: attempt,
            outcome: observation.outcome,
            decision,
            failure,
        });
    }
    Ok(report)
}

fn attention_tasks(store: &StateStore) -> Result<Vec<(String, String, u32)>, StateError> {
    Ok(store
        .connection
        .prepare(
            "SELECT t.id,a.id,(SELECT COUNT(*) FROM attempts WHERE task_id=t.id)
         FROM tasks t JOIN attempts a ON a.task_id=t.id
         WHERE t.state='attention' AND a.rowid=(SELECT MAX(rowid) FROM attempts WHERE task_id=t.id)
         ORDER BY t.rowid",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?)
}

fn launch_retry<P: ProjectReader, Q: PullRequestReader, L: SupervisorLauncher, I: IdCreator>(
    store: &mut StateStore,
    previous: &LaunchPlan,
    observation: &OutcomeObservation,
    count: u32,
    d: &mut AutoRetryDependencies<'_, P, Q, L, I>,
) -> Result<Option<LaunchPlan>, DispatchError> {
    let owner = match WorktreeOwner::acquire_existing(store.root(), &previous.task_id) {
        Ok(owner) => owner,
        Err(OwnershipError::Busy) => return Ok(None),
        Err(OwnershipError::Unavailable) => {
            return Err(DispatchError::RetryOwnershipUnavailable);
        }
    };
    let attempt = format!("attempt-{}", d.ids.create().map_err(StateError::from)?);
    let selection = crate::state::selection_for_attempt(store, previous)?;
    let actor = &selection.candidate.mapping.allowed_pr_author;
    let revision = format!("{}:auto:{attempt}", d.config_revision);
    let reason = match observation.outcome {
        WorkerOutcome::Transient {
            class: crate::config::TransientFailure::TurnBudget,
        } => "automatic retry of typed worker turn budget",
        _ => "automatic retry of typed transient worker failure",
    };
    journal::record_evidence(
        store,
        &previous.task_id,
        Some(&previous.attempt_id),
        "automatic_retry_requested",
        &json!({
            "attempt_id":attempt, "actor":actor, "reason":reason, "config_revision":revision,
            "policy":d.config.retry_policy, "outcome":observation, "previous_attempt_count":count,
        })
        .to_string(),
    )?;
    let result = retry_with_owner(
        store,
        RetryDependencies {
            task_id: &previous.task_id,
            previous_attempt_id: &previous.attempt_id,
            attempt_id: &attempt,
            config: d.config,
            config_revision: &revision,
            actor,
            reason,
            revalidate_terminal_exit: false,
            projects: d.projects,
            prs: d.prs,
            launcher: d.launcher,
        },
        &owner,
    );
    if let Err(error) = &result {
        journal::record_evidence(
            store,
            &previous.task_id,
            Some(&previous.attempt_id),
            "automatic_retry_failed",
            &json!({
                "attempt_id":attempt, "reason":super::diagnostics::scheduling_reason(error),
            })
            .to_string(),
        )?;
    }
    result.map(Some)
}

fn task_error(plan: &LaunchPlan, check: &'static str, source: DispatchError) -> ScheduleError {
    ScheduleError::Task {
        task_id: plan.task_id.clone(),
        attempt_id: plan.attempt_id.clone(),
        check,
        source: Box::new(source),
    }
}

fn retry_decision<P: ProjectReader, Q: PullRequestReader, L: SupervisorLauncher, I: IdCreator>(
    store: &mut StateStore,
    previous: &LaunchPlan,
    observation: &OutcomeObservation,
    count: u32,
    d: &mut AutoRetryDependencies<'_, P, Q, L, I>,
    report: &mut AutoRetryReport,
    failure: &mut Option<super::admission::TaskAdmissionFailure>,
) -> Result<RetryDecision, ScheduleError> {
    match launch_retry(store, previous, observation, count, d) {
        Ok(Some(plan)) => {
            report.launched.push(plan);
            Ok(RetryDecision::Launched)
        }
        Ok(None) => Ok(RetryDecision::WorktreeBusy),
        Err(error) => {
            let latest = crate::state::task_records::latest_attempt(store, &previous.task_id)?
                .ok_or(StateError::InvalidSelection)?;
            if !super::admission::task_local(&error) || latest != previous.attempt_id {
                return Err(ScheduleError::Task {
                    task_id: previous.task_id.clone(),
                    attempt_id: latest,
                    check: "automatic_retry",
                    source: Box::new(error),
                });
            }
            *failure = Some(super::admission::TaskAdmissionFailure::record(
                store,
                &previous.task_id,
                &previous.attempt_id,
                "retry_admission",
                &error,
            )?);
            Ok(if matches!(error, DispatchError::ConfigMismatch { .. }) {
                RetryDecision::ConfigMismatch
            } else {
                RetryDecision::AdmissionFailed
            })
        }
    }
}
