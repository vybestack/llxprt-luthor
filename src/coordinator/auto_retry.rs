//! Bounded automation for released attention tasks; all launch authority stays
//! in the existing audited operator-retry path.
mod outcome;
use super::{
    IdCreator, RetryDependencies, SupervisorLauncher,
    ports::{DispatchError, observation_time},
    retry_one,
};
use crate::{
    config::{Config, RetryPolicy},
    github::{project::ProjectReader, pull_request::PullRequestReader},
    model::LaunchPlan,
    state::{StateError, StateStore, journal, scheduling},
};
pub use outcome::{OutcomeObservation, WorkerOutcome};
use serde::Serialize;
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDecision {
    Ineligible,
    NeedsHumanInput,
    NonTransient,
    Unclassified,
    ClassDisabled,
    Exhausted,
    Backoff,
    CapacityFull,
    Launched,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetryObservation {
    pub task_id: String,
    pub attempt_id: String,
    pub outcome: WorkerOutcome,
    pub decision: RetryDecision,
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
) -> Result<AutoRetryReport, DispatchError> {
    let mut d = dependencies;
    d.config
        .validate()
        .map_err(crate::supervisor::SupervisorError::from)?;
    let mut report = AutoRetryReport::default();
    if matches!(d.config.retry_policy, RetryPolicy::Disabled { .. }) {
        return Ok(report);
    }
    let now = observation_time()?;
    for (task, attempt, count) in attention_tasks(store)? {
        let (plan, receipt) = match crate::state::retry_context_for_task(store, &task, &attempt) {
            Ok(context) => context,
            Err(StateError::LaunchBlocked) => {
                report.observations.push(RetryObservation {
                    task_id: task,
                    attempt_id: attempt,
                    outcome: WorkerOutcome::Unclassified,
                    decision: RetryDecision::Ineligible,
                });
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let observation = observe_outcome(store, &plan, &receipt, now)?;
        let decision = decision(&d.config.retry_policy, &observation, count, now)?;
        let decision = if let Some(decision) = decision {
            decision
        } else {
            match scheduling::ensure_dispatch_capacity(store) {
                Ok(()) => {
                    report
                        .launched
                        .push(launch_retry(store, &plan, &observation, count, &mut d)?);
                    RetryDecision::Launched
                }
                Err(StateError::Capacity { .. }) => RetryDecision::CapacityFull,
                Err(error) => return Err(error.into()),
            }
        };
        report.observations.push(RetryObservation {
            task_id: task,
            attempt_id: attempt,
            outcome: observation.outcome,
            decision,
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

fn observe_outcome(
    store: &mut StateStore,
    plan: &LaunchPlan,
    receipt: &crate::model::ExitReceipt,
    now: u64,
) -> Result<OutcomeObservation, DispatchError> {
    let outcome = outcome::read(store.root(), plan, receipt)?;
    if let Some(saved) = journal::evidence_payload(
        store,
        &plan.task_id,
        Some(&plan.attempt_id),
        "worker_outcome",
    )? {
        let saved: OutcomeObservation = serde_json::from_str(&saved).map_err(StateError::from)?;
        if saved.outcome != outcome || saved.observed_at_unix_secs == 0 {
            return Err(StateError::LaunchBlocked.into());
        }
        return Ok(saved);
    }
    let observation = OutcomeObservation {
        outcome,
        observed_at_unix_secs: now,
    };
    journal::record_evidence(
        store,
        &plan.task_id,
        Some(&plan.attempt_id),
        "worker_outcome",
        &serde_json::to_string(&observation).map_err(StateError::from)?,
    )?;
    Ok(observation)
}

fn decision(
    policy: &RetryPolicy,
    observation: &OutcomeObservation,
    count: u32,
    now: u64,
) -> Result<Option<RetryDecision>, StateError> {
    let RetryPolicy::Bounded {
        max_attempts,
        backoff_secs,
        classes,
    } = policy
    else {
        return Err(StateError::InvalidConfig);
    };
    let class = match observation.outcome {
        WorkerOutcome::Transient { class } => class,
        WorkerOutcome::NeedsHumanInput => return Ok(Some(RetryDecision::NeedsHumanInput)),
        WorkerOutcome::NonTransient => return Ok(Some(RetryDecision::NonTransient)),
        WorkerOutcome::Unclassified => return Ok(Some(RetryDecision::Unclassified)),
    };
    let elapsed = now
        .checked_sub(observation.observed_at_unix_secs)
        .ok_or(StateError::LaunchBlocked)?;
    Ok(if !classes.contains(&class) {
        Some(RetryDecision::ClassDisabled)
    } else if count >= *max_attempts {
        Some(RetryDecision::Exhausted)
    } else if elapsed < u64::from(*backoff_secs) {
        Some(RetryDecision::Backoff)
    } else {
        None
    })
}

fn launch_retry<P: ProjectReader, Q: PullRequestReader, L: SupervisorLauncher, I: IdCreator>(
    store: &mut StateStore,
    previous: &LaunchPlan,
    observation: &OutcomeObservation,
    count: u32,
    d: &mut AutoRetryDependencies<'_, P, Q, L, I>,
) -> Result<LaunchPlan, DispatchError> {
    let attempt = format!("attempt-{}", d.ids.create().map_err(StateError::from)?);
    let selection = crate::state::selection_for_attempt(store, previous)?;
    let actor = &selection.candidate.mapping.allowed_pr_author;
    let revision = format!("{}:auto:{attempt}", d.config_revision);
    let reason = "automatic retry of typed transient worker failure";
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
    let result = retry_one(
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
    );
    if let Err(error) = &result {
        journal::record_evidence(
            store,
            &previous.task_id,
            Some(&previous.attempt_id),
            "automatic_retry_failed",
            &json!({
                "attempt_id":attempt, "reason":super::ports::dispatch_failure_reason(error, true),
            })
            .to_string(),
        )?;
    }
    result
}
