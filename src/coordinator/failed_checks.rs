//! Rerun each failed Actions run once for this verified head, before coding.
use super::ports::DispatchError;
use crate::{
    github::pull_request::{PrHealth, PrHealthReader},
    model::VerifiedOpenPr,
    state::{StateStore, journal, task_records},
};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Rerun {
    head: String,
    run_id: u64,
    run_attempt: u64,
    checks: Vec<String>,
}

pub(crate) enum FailedChecks {
    Waiting,
    Ready(Vec<String>),
}

pub(crate) fn inspect(
    store: &mut StateStore,
    task: &str,
    saved: &VerifiedOpenPr,
    health: &PrHealth,
    prs: &mut impl PrHealthReader,
) -> Result<FailedChecks, DispatchError> {
    let reruns = recorded(store, task, &health.head)?;
    if !health.needs_repair()
        && !health.required_checks_pending
        && !reruns.is_empty()
        && reruns.iter().all(|run| {
            health
                .required_jobs
                .iter()
                .any(|job| job.run_id == run.run_id && job.run_attempt > run.run_attempt)
        })
    {
        journal::record_evidence(
            store,
            task,
            Some(&saved.attempt_id),
            "pr_checks_flaky_looking",
            "required checks passed after one failed-job rerun",
        )?;
        return Ok(FailedChecks::Waiting);
    }
    let mut waiting = false;
    let mut logs = Vec::new();
    let mut handled = Vec::new();
    for job in &health.failed_jobs {
        match reruns.iter().find(|run| run.run_id == job.run_id) {
            Some(prior) if job.run_attempt <= prior.run_attempt => waiting = true,
            Some(_) => {
                logs.push(format!(
                    "Required check {} failed again after rerun:\n{}",
                    job.name,
                    prs.failed_job_logs(&saved.repository, job.job_id)?
                ));
            }
            None => {
                if handled.contains(&job.run_id) {
                    continue;
                }
                handled.push(job.run_id);
                rerun_once(store, task, saved, health, job.run_id, prs)?;
                waiting = true;
            }
        }
    }
    if waiting || (health.required_checks_pending && !health.conflicting) {
        return Ok(FailedChecks::Waiting);
    }
    if !health.failed_required_checks.is_empty() {
        for name in &health.failed_required_checks {
            if !health.failed_jobs.iter().any(|job| job.name == *name) {
                task_records::hold_task(
                    store,
                    task,
                    &format!(
                        "required check has no rerunnable Actions job; requires a person: {name}"
                    ),
                )?;
                return Ok(FailedChecks::Waiting);
            }
        }
        journal::record_evidence(
            store,
            task,
            Some(&saved.attempt_id),
            "pr_checks_repeat_failure",
            "required checks failed again after one rerun; code repair required",
        )?;
    }
    Ok(FailedChecks::Ready(logs))
}

fn recorded(store: &StateStore, task: &str, head: &str) -> Result<Vec<Rerun>, DispatchError> {
    let values: Vec<String> = store.connection.prepare("SELECT payload FROM evidence WHERE task_id=?1 AND kind='pr_failed_jobs_rerun' AND json_extract(payload,'$.head')=?2").map_err(crate::model::StateError::from)?.query_map(params![task, head], |row| row.get(0)).map_err(crate::model::StateError::from)?.collect::<Result<_, _>>().map_err(crate::model::StateError::from)?;
    values.into_iter().map(|payload| {
        let audit: Rerun = serde_json::from_str(&payload).map_err(crate::model::StateError::from)?;
        // A crash or ambiguous POST without acknowledgement never authorizes
        // another write or code repair based on somebody else's rerun.
        let ack: usize = store.connection.query_row("SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='pr_failed_jobs_rerun_acknowledged' AND payload=?2", params![task,payload], |row| row.get(0)).map_err(crate::model::StateError::from)?;
        if ack != 1 { return Err(crate::model::StateError::LaunchBlocked.into()); }
        Ok(audit)
    }).collect()
}

fn rerun_once(
    store: &mut StateStore,
    task: &str,
    saved: &VerifiedOpenPr,
    health: &PrHealth,
    run_id: u64,
    prs: &mut impl PrHealthReader,
) -> Result<(), DispatchError> {
    let job = health
        .failed_jobs
        .iter()
        .find(|j| j.run_id == run_id)
        .ok_or(crate::model::StateError::LaunchBlocked)?;
    let audit = Rerun {
        head: health.head.clone(),
        run_id,
        run_attempt: job.run_attempt,
        checks: health
            .failed_jobs
            .iter()
            .filter(|j| j.run_id == run_id)
            .map(|j| j.name.clone())
            .collect(),
    };
    // Intent precedes POST; an ambiguous write is held, never repeated.
    journal::record_evidence(
        store,
        task,
        Some(&saved.attempt_id),
        "pr_failed_jobs_rerun",
        &serde_json::to_string(&audit).map_err(crate::model::StateError::from)?,
    )?;
    prs.rerun_failed_jobs(&saved.repository, run_id, &health.head)?;
    journal::record_evidence(
        store,
        task,
        Some(&saved.attempt_id),
        "pr_failed_jobs_rerun_acknowledged",
        &serde_json::to_string(&audit).map_err(crate::model::StateError::from)?,
    )?;
    Ok(())
}
