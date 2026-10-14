//! Return finished tasks that an earlier release wrongly held for PR repair.
use super::{DispatchError, notes::TaskNote, ports::observation_time};
use crate::{
    cleanup::{CleanupError, terminal_pr},
    github::pull_request::PullRequestReader,
    pr_repair::UNSPECIFIC_REPAIR_HOLD_REASON,
    state::{StateError, StateStore, pr_repair, pr_restore, task_records},
};

#[derive(Debug, Default)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    pub retained: Vec<TaskNote>,
}

enum Proof {
    Terminal(serde_json::Value),
    Retained(String),
}

/// A held task goes back to `pr_complete` only on a fresh exact-identity read
/// showing its saved PR closed or merged. Anything less leaves it held.
pub fn restore_wrongly_held_prs<Q: PullRequestReader>(
    store: &mut StateStore,
    prs: &mut Q,
) -> Result<RestoreReport, DispatchError> {
    let mut report = RestoreReport::default();
    for task in pr_restore::candidates(store)? {
        match terminal_proof(store, prs, &task)? {
            Proof::Terminal(pr) => {
                let proof = serde_json::json!({
                    "restored_from_reason": UNSPECIFIC_REPAIR_HOLD_REASON,
                    "observed_at_unix_secs": observation_time()?,
                    "terminal_pr": pr,
                });
                pr_restore::restore(store, &task, &proof)?;
                report.restored.push(task);
            }
            Proof::Retained(reason) => report.retained.push(TaskNote {
                task_id: task,
                reason,
            }),
        }
    }
    Ok(report)
}

fn terminal_proof<Q: PullRequestReader>(
    store: &StateStore,
    prs: &mut Q,
    task: &str,
) -> Result<Proof, StateError> {
    let saved = match pr_repair::saved_pr(&store.connection, task) {
        Ok(saved) => saved,
        Err(StateError::Sql(rusqlite::Error::QueryReturnedNoRows)) => {
            return Ok(Proof::Retained("no saved verified PR".into()));
        }
        Err(StateError::Serialization(_)) => {
            return Ok(Proof::Retained("saved verified PR is malformed".into()));
        }
        Err(error) => return Err(error),
    };
    if task_records::latest_attempt(store, task)?.as_deref() != Some(saved.attempt_id.as_str()) {
        return Ok(Proof::Retained(
            "saved verified PR is not from the latest attempt".into(),
        ));
    }
    match terminal_pr::observe(prs, &saved) {
        Ok(Some(pr)) => Ok(Proof::Terminal(pr)),
        Ok(None) => Ok(Proof::Retained("verified PR is still open".into())),
        Err(CleanupError::State(error)) => Err(error),
        Err(error @ (CleanupError::Conflict(_) | CleanupError::PullRequest(_))) => Ok(
            Proof::Retained(format!("terminal PR proof not established ({error})")),
        ),
        Err(_) => unreachable!("terminal PR observation only reads GitHub and saved state"),
    }
}

/// Restore the historical open-PR identity holds only after fresh claim, PR,
/// owned worktree and receipt verification. An unavailable proof stays held.
pub fn restore_open_held_prs<P: crate::github::project::ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    projects: &mut P,
    prs: &mut Q,
) -> Result<RestoreReport, DispatchError> {
    let mut report = RestoreReport::default();
    for task in pr_restore::open_candidates(store)? {
        match restore_open_task(store, &task, projects, prs) {
            Ok(()) => report.restored.push(task),
            Err(DispatchError::State(error)) if !matches!(error, StateError::LaunchBlocked) => {
                return Err(error.into());
            }
            Err(_) => report.retained.push(TaskNote {
                task_id: task,
                reason: "open PR, claim, worktree or receipt proof did not verify".into(),
            }),
        }
    }
    Ok(report)
}

fn restore_open_task<P: crate::github::project::ProjectReader, Q: PullRequestReader>(
    store: &mut StateStore,
    task: &str,
    projects: &mut P,
    prs: &mut Q,
) -> Result<(), DispatchError> {
    use super::ports::{
        PrVerificationStage, verify_completion_claim, verify_finishing_pr, verify_open_pr,
    };
    use crate::{
        github::pull_request::{LookupResult, lookup},
        state::pr_completion,
        supervisor::SupervisorError,
    };
    pr_restore::require_open_context(store, task)?;
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    let attempt = task_records::latest_attempt(store, task)?.ok_or(StateError::LaunchBlocked)?;
    let reason = task_records::held_reason(store, task)?.ok_or(StateError::LaunchBlocked)?;
    let LookupResult::OpenPreexisting(pr) = lookup(
        prs,
        &selection.candidate.mapping.code_repository,
        &selection.candidate.issue_url,
    )?
    else {
        return Err(StateError::LaunchBlocked.into());
    };
    let saved = match pr_repair::saved_pr(&store.connection, task) {
        Ok(saved) => Some(saved),
        Err(StateError::Sql(rusqlite::Error::QueryReturnedNoRows)) => None,
        Err(error) => return Err(error.into()),
    };
    let observed_at = observation_time()?;
    if let Some(saved) = saved {
        let observed = verify_open_pr(
            store,
            task,
            &attempt,
            prs,
            (*pr, observed_at),
            PrVerificationStage::Exit,
        )
        .map_err(|_| SupervisorError::Conflict)?;
        let _owner = super::pr_head::synchronize(store, task, &saved, &observed, projects)?;
        pr_restore::restore_open(
            store,
            task,
            &serde_json::json!({
                "restored_from_reason":reason,"observed_at_unix_secs":observed_at,"open_pr":observed,
            }),
        )?;
    } else {
        // An old `exit PR present` may predate any completion proof. There is
        // no recorded PR lineage to adopt: require the exact live local tip.
        if reason != "exit PR present" {
            return Err(StateError::LaunchBlocked.into());
        }
        let observed = verify_finishing_pr(store, task, &attempt, prs, (*pr, observed_at))
            .map_err(|_| SupervisorError::Conflict)?;
        verify_completion_claim(projects, &selection)?;
        let _owner = super::pr_head::accounted_owner(store, task, &attempt)?;
        pr_completion::record_restored_open_pr(
            store,
            task,
            &attempt,
            &observed,
            &serde_json::json!({
                "restored_from_reason":reason,"observed_at_unix_secs":observed_at,"open_pr":observed,
            }),
        )?;
    }
    Ok(())
}
