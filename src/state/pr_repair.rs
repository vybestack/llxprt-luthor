//! PR repair authorization is separate from paused/absent-PR retries.
use super::{database::StateStore, proofs};
use crate::{
    model::{LaunchPlan, SelectionEvidence, StateError, VerifiedOpenPr},
    pr_repair::{MAX_PR_REPAIRS, PrRepairAuthorization},
};
use rusqlite::{Connection, params};

pub(crate) fn candidates(store: &StateStore) -> Result<Vec<String>, StateError> {
    Ok(store.connection.prepare("SELECT id FROM tasks t WHERE state='pr_complete' AND NOT EXISTS(SELECT 1 FROM intents WHERE task_id=t.id AND kind='cleanup') ORDER BY rowid")?
        .query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?)
}

pub(crate) fn saved_pr(db: &Connection, task: &str) -> Result<VerifiedOpenPr, StateError> {
    let payload: String = db.query_row("SELECT payload FROM evidence WHERE task_id=?1 AND kind='verified_open_pr' ORDER BY sequence DESC LIMIT 1", [task], |row| row.get(0))?;
    proofs::parse_saved(&payload)
}

pub(crate) fn count(db: &Connection, task: &str) -> Result<usize, StateError> {
    Ok(db.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='pr_repair_authorized'",
        [task],
        |row| row.get(0),
    )?)
}

pub(crate) fn authorization(
    db: &Connection,
    task: &str,
    attempt: &str,
) -> Result<Option<PrRepairAuthorization>, StateError> {
    proofs::unique_payload(db, true, task, Some(attempt), "pr_repair_authorized")?
        .map(|payload| proofs::parse_saved(&payload))
        .transpose()
}

pub(crate) fn context(
    store: &StateStore,
    task: &str,
) -> Result<(LaunchPlan, VerifiedOpenPr), StateError> {
    super::continuation_hold::require_unamended_task(&store.connection, task)?;
    let valid: bool = store.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND state='pr_complete')
         AND NOT EXISTS(SELECT 1 FROM reservations WHERE task_id=?1 AND status='reserved')
         AND NOT EXISTS(SELECT 1 FROM attempts WHERE task_id=?1 AND (lifecycle!='completed' OR outcome IS NULL))
         AND NOT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND kind='cleanup')",
        [task], |row| row.get(0))?;
    if !valid {
        return Err(StateError::LaunchBlocked);
    }
    let (attempt, payload): (String, String) = store.connection.query_row(
        "SELECT a.id,i.detail FROM attempts a JOIN intents i ON i.task_id=a.task_id AND i.attempt_id=a.id AND i.kind='launch' WHERE a.task_id=?1 ORDER BY a.rowid DESC LIMIT 1", [task], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let plan: LaunchPlan = proofs::parse_saved(&payload)?;
    let saved = saved_pr(&store.connection, task)?;
    if plan.attempt_id != attempt || saved.attempt_id != attempt {
        return Err(StateError::LaunchBlocked);
    }
    Ok((plan, saved))
}

pub(crate) fn validate(
    audit: &PrRepairAuthorization,
    selection: &SelectionEvidence,
) -> Result<(), StateError> {
    let prior = &audit.previous_plan;
    let plan = &audit.plan;
    if plan.task_id != prior.task_id
        || plan.attempt_id == prior.attempt_id
        || plan.session_id != prior.session_id
        || plan.session_id != plan.task_id
        || plan.worktree != prior.worktree
        || plan.worker_build != prior.worker_build
        || plan.session_environment != prior.session_environment
        || plan.config_revision != prior.config_revision
        || audit.saved_pr.attempt_id != prior.attempt_id
        || audit.observed_pr.attempt_id != prior.attempt_id
        || audit.saved_pr.id != audit.observed_pr.id
        || audit.saved_pr.number != audit.observed_pr.number
        || audit.saved_pr.head_commit_sha != audit.observed_pr.head_commit_sha
        || audit.saved_pr.repository_id != audit.observed_pr.repository_id
        || audit.saved_pr.head_repository_id != audit.observed_pr.head_repository_id
        || audit.health.id != audit.observed_pr.id
        || audit.health.head != audit.observed_pr.head_commit_sha
        || !audit.health.needs_repair()
        || audit.observed_pr.observed_at == 0
    {
        return Err(StateError::LaunchBlocked);
    }
    validate_source(&audit.source, selection)
}

fn validate_source(
    source: &crate::model::RetrySourceEvidence,
    selection: &SelectionEvidence,
) -> Result<(), StateError> {
    let c = &selection.candidate;
    if source.observed_at_unix_secs == 0
        || source.item.item_id != c.item_id
        || source.item.issue_node_id != c.issue_node_id
        || source.item.repository != c.repository
        || source.item.tracker_repo_id != c.tracker_repo_id
        || source.item.issue_number != c.issue_number
        || source.issue.node_id != c.issue_node_id
        || source.issue.repository != c.repository
        || source.issue.tracker_repo_id != c.tracker_repo_id
        || source.issue.number != c.issue_number
        || source.issue.url != c.issue_url
        || source.issue.state != "open"
        || source.issue.assignees != [selection.effective_config.assignment_login.as_str()]
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

pub(crate) fn hold_intent(
    store: &mut StateStore,
    audit: &PrRepairAuthorization,
    selection: &SelectionEvidence,
) -> Result<(), StateError> {
    let (previous, saved) = context(store, &audit.plan.task_id)?;
    validate(audit, selection)?;
    if previous != audit.previous_plan
        || saved != audit.saved_pr
        || count(&store.connection, &previous.task_id)? >= MAX_PR_REPAIRS
    {
        return Err(StateError::LaunchBlocked);
    }
    let claim: String = store.connection.query_row(
        "SELECT detail FROM intents WHERE task_id=?1 AND kind='claim_assignment'",
        [&previous.task_id],
        |row| row.get(0),
    )?;
    let expected = serde_json::json!({"principal":selection.effective_config.assignment_login,"repository":selection.candidate.repository,"number":selection.candidate.issue_number});
    if claim != audit.source.claim || serde_json::from_str::<serde_json::Value>(&claim)? != expected
    {
        return Err(StateError::LaunchBlocked);
    }
    let tx = store.connection.transaction()?;
    let capacity: usize = tx.query_row(
        "SELECT value FROM state_meta WHERE key='capacity'",
        [],
        |row| row.get(0),
    )?;
    let reserved: usize = tx.query_row(
        "SELECT COUNT(*) FROM reservations WHERE status='reserved'",
        [],
        |row| row.get(0),
    )?;
    if reserved >= capacity {
        return Err(StateError::Capacity { reserved, capacity });
    }
    let plan = &audit.plan;
    tx.execute(
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES(?1,?2,'launch_intended')",
        params![plan.attempt_id, plan.task_id],
    )?;
    tx.execute(
        "INSERT INTO reservations(attempt_id,task_id,status) VALUES(?1,?2,'reserved')",
        params![plan.attempt_id, plan.task_id],
    )?;
    tx.execute(
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'launch',?4)",
        params![
            format!("launch-{}", plan.attempt_id),
            plan.task_id,
            plan.attempt_id,
            serde_json::to_string(plan)?
        ],
    )?;
    tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,'pr_repair_authorized',?3)", params![plan.task_id, plan.attempt_id, serde_json::to_string(audit)?])?;
    tx.execute(
        "UPDATE tasks SET state='held' WHERE id=?1 AND state='pr_complete'",
        [&plan.task_id],
    )?;
    if tx.changes() != 1 {
        return Err(StateError::LaunchBlocked);
    }
    tx.commit()?;
    Ok(())
}

/// Bind the terminal PR lookup and successful worker report to completion in
/// the same transaction as the new PR proof, avoiding a restart gap.
pub(crate) fn completion_lookup(
    db: &Connection,
    task: &str,
    attempt: &str,
) -> Result<(String, String), StateError> {
    let report = proofs::unique_payload(db, true, task, Some(attempt), "pr_repair_result")?
        .ok_or(StateError::LaunchBlocked)?;
    let report: crate::pr_repair::PrRepairResult = proofs::parse_saved(&report)?;
    if report.attempt_id != attempt
        || report.status != crate::pr_repair::PrRepairStatus::Repaired
        || proofs::unique_payload(db, true, task, Some(attempt), "pr_repair_blocked")?.is_some()
    {
        return Err(StateError::LaunchBlocked);
    }
    let lookup = proofs::unique_payload(db, true, task, Some(attempt), "pr_repair_pr_lookup")?
        .ok_or(StateError::LaunchBlocked)?;
    let parsed: crate::model::ExitPrEvidence = proofs::parse_saved(&lookup)?;
    if parsed.status != crate::model::PausePrStatus::Open || parsed.observed_at_unix_secs == 0 {
        return Err(StateError::LaunchBlocked);
    }
    let stopped: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop')",
        params![task, attempt],
        |row| row.get(0),
    )?;
    let kind = if stopped {
        "pause_pr_lookup"
    } else {
        "exit_pr_lookup"
    };
    if proofs::unique_payload(db, true, task, Some(attempt), kind)?.is_some() {
        return Err(StateError::LaunchBlocked);
    }
    Ok((kind.into(), lookup))
}
