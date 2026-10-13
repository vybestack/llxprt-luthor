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
