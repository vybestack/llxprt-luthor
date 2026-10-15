//! Terminal phases for tasks whose work is over, so they stop occupying capacity.
//!
//! A retired task keeps every row it had. Only its phase changes, its
//! reservation is released and one `task_retired` evidence row records why.
use super::database::StateStore;
use crate::model::StateError;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

/// SQL list of the retired phases, for statements that must ignore retired tasks.
pub(crate) const RETIRED_PHASES_SQL: &str = "'issue_closed','pr_merged','pr_closed','abandoned'";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetiredPhase {
    /// The tracker issue was closed, by whatever means.
    IssueClosed,
    /// The task's verified PR merged.
    PrMerged,
    /// The task's verified PR was closed without merging.
    PrClosed,
    /// An operator stopped the task.
    Abandoned,
}

impl RetiredPhase {
    pub const ALL: [Self; 4] = [
        Self::IssueClosed,
        Self::PrMerged,
        Self::PrClosed,
        Self::Abandoned,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::IssueClosed => "issue_closed",
            Self::PrMerged => "pr_merged",
            Self::PrClosed => "pr_closed",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn parse(phase: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|known| known.as_str() == phase)
    }
}

/// What is recorded about a retirement. `evidence` carries the observations
/// (issue close event, PR merge details) or the operator's audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Retirement {
    pub phase: RetiredPhase,
    pub reason: String,
    pub actor: Option<String>,
    pub observed_at_unix_secs: u64,
    pub evidence: Value,
}

/// A task may be retired only when no worker can still be running for it:
/// every attempt has a durable, verified exit, except an explicitly abandoned
/// initial attempt with contemporaneous preparation and never-dispatched proof.
/// Historical absence-only attempts never qualify. A cleanup in progress owns
/// the task until it finishes.
const RETIRABLE: &str = "t.state IN ('preparing','selected','claimed','attention','held','paused','pr_complete')
    AND NOT EXISTS(SELECT 1 FROM attempts a WHERE a.task_id=t.id AND NOT EXISTS(
        SELECT 1 FROM evidence e WHERE e.task_id=t.id AND e.attempt_id=a.id AND e.kind='attempt_exit') AND a.id IS NOT ?2)
    AND NOT EXISTS(SELECT 1 FROM intents WHERE task_id=t.id AND kind='cleanup')";

/// Tasks the reconciliation sweep inspects. `pr_complete` tasks are left to
/// cleanup, which removes their worktree and branch once the PR is finished.
pub(crate) fn candidates(store: &StateStore) -> Result<Vec<String>, StateError> {
    Ok(store
        .connection
        .prepare(&format!(
            "SELECT t.id FROM tasks t WHERE {RETIRABLE} AND t.state!='pr_complete' ORDER BY t.rowid"
        ))?
        .query_map(
            params![Option::<String>::None, Option::<String>::None],
            |row| row.get(0),
        )?
        .collect::<Result<_, _>>()?)
}

/// Moves the task to its terminal phase, releases its reservation and appends
/// the evidence, all in one transaction that rechecks the qualification.
pub fn retire(
    store: &mut StateStore,
    task: &str,
    retirement: &Retirement,
) -> Result<(), StateError> {
    let owner = if retirement.phase == RetiredPhase::Abandoned {
        Some(
            crate::ownership::WorktreeOwner::acquire(store.root(), task).map_err(|_| {
                StateError::NotRetirable("worktree ownership is busy or unavailable")
            })?,
        )
    } else {
        None
    };
    let tx = store
        .connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let never_dispatched = never_dispatched_abandonment(&tx, &store.root, task, owner.as_ref())?;
    let qualified: bool = tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM tasks t WHERE {RETIRABLE} AND t.id=?1)"),
        params![task, never_dispatched],
        |row| row.get(0),
    )?;
    if !qualified {
        return Err(StateError::NotRetirable(refusal(&tx, task)?));
    }
    let previous_phase: String =
        tx.query_row("SELECT state FROM tasks WHERE id=?1", [task], |row| {
            row.get(0)
        })?;
    let previous_reason: Option<String> = tx
        .query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND kind='held_reason' ORDER BY sequence DESC LIMIT 1",
            [task],
            |row| row.get(0),
        )
        .optional()?;
    tx.execute(
        "UPDATE tasks SET state=?2 WHERE id=?1",
        params![task, retirement.phase.as_str()],
    )?;
    tx.execute(
        "UPDATE reservations SET status='released' WHERE task_id=?1 AND status='reserved'",
        [task],
    )?;
    let mut payload = serde_json::to_value(retirement)?;
    payload["previous_phase"] = Value::from(previous_phase);
    payload["never_dispatched_attempt"] = never_dispatched.map_or(Value::Null, Value::from);
    payload["previous_hold_reason"] = previous_reason.map_or(Value::Null, Value::from);
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'task_retired',?2)",
        params![task, payload.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

/// The first failed condition, in operator terms.
fn refusal(tx: &Connection, task: &str) -> Result<&'static str, StateError> {
    let phase: Option<String> = tx
        .query_row("SELECT state FROM tasks WHERE id=?1", [task], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(phase) = phase else {
        return Err(StateError::InvalidSelection);
    };
    if RetiredPhase::parse(&phase).is_some() {
        return Ok("task is already retired");
    }
    if !matches!(
        phase.as_str(),
        "preparing" | "selected" | "claimed" | "attention" | "held" | "paused" | "pr_complete"
    ) {
        return Ok("task has not reached a phase that can be retired");
    }
    let unfinished: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts a WHERE a.task_id=?1 AND NOT EXISTS(
            SELECT 1 FROM evidence e WHERE e.task_id=?1 AND e.attempt_id=a.id AND e.kind='attempt_exit'))",
        [task],
        |row| row.get(0),
    )?;
    if unfinished {
        return Ok(
            "an attempt has no verified exit; a running task cannot be retired (reconcile it first)",
        );
    }
    Ok("a cleanup is in progress for the task")
}

/// The latest retirement record of a task, for display.
pub(crate) fn latest(conn: &Connection, task: &str) -> rusqlite::Result<Option<Value>> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload FROM evidence WHERE task_id=?1 AND kind='task_retired' ORDER BY sequence DESC LIMIT 1",
            [task],
            |row| row.get(0),
        )
        .optional()?;
    payload
        .map(|payload| {
            serde_json::from_str(&payload).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn never_dispatched_abandonment(
    db: &Connection,
    root: &std::path::Path,
    task: &str,
    owner: Option<&crate::ownership::WorktreeOwner>,
) -> Result<Option<String>, StateError> {
    let Some(owner) = owner else {
        return Ok(None);
    };
    let latest: Option<String> = db
        .query_row(
            "SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",
            [task],
            |row| row.get(0),
        )
        .optional()?;
    let Some(attempt) = latest else {
        return Ok(None);
    };
    let proof = super::preparation::verify_owner(db, root, task, &attempt, owner)
        .and_then(|()| super::continuation::read_context(db, root, task, &attempt));
    match proof {
        Ok(_) => Ok(Some(attempt)),
        Err(StateError::LaunchBlocked) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_phase_list_matches_the_phases() {
        let listed: Vec<&str> = RETIRED_PHASES_SQL
            .split(',')
            .map(|phase| phase.trim_matches('\''))
            .collect();
        let known: Vec<&str> = RetiredPhase::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(listed, known);
    }
}
