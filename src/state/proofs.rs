use crate::{
    model::VerifiedOpenPr,
    model::{SelectionEvidence, StateError, WorktreeIdentity, WorktreeIntent},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};

pub(crate) fn unique_payload(
    connection: &Connection,
    evidence: bool,
    task_id: &str,
    attempt_id: Option<&str>,
    kind: &str,
) -> Result<Option<String>, StateError> {
    let sql = match (evidence, attempt_id) {
        (true, Some(_)) => {
            "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind=?3"
        }
        (true, None) => {
            "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS ?2 AND kind=?3"
        }
        (false, None) => {
            "SELECT detail FROM intents WHERE task_id=?1 AND attempt_id IS ?2 AND kind=?3"
        }
        (false, Some(_)) => {
            "SELECT detail FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind=?3"
        }
    };
    let values: Vec<String> = connection
        .prepare(sql)?
        .query_map(params![task_id, attempt_id, kind], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    match values.len() {
        0 => Ok(None),
        1 => Ok(values.into_iter().next()),
        _ => Err(StateError::LaunchBlocked),
    }
}

pub(crate) fn parse_saved<T: DeserializeOwned + Serialize>(payload: &str) -> Result<T, StateError> {
    let parsed: T = serde_json::from_str(payload)?;
    if serde_json::to_value(&parsed)? != serde_json::from_str::<serde_json::Value>(payload)? {
        return Err(StateError::LaunchBlocked);
    }
    Ok(parsed)
}
pub(crate) fn dispatch_intent(kind: &str) -> bool {
    matches!(kind, "supervisor_dispatch" | "gate_release")
}
pub(crate) fn dispatch_evidence(kind: &str) -> bool {
    matches!(
        kind,
        "supervisor_ready"
            | "child_registered"
            | "tracked_descendant"
            | "gate_sent"
            | "worktree_owner_protocol"
    )
}

pub(crate) fn validate_owner_protocol(
    db: &Connection,
    task: &str,
    attempt: &str,
) -> Result<i64, StateError> {
    use crate::ownership::WorktreeOwnerProtocolInternal;
    let rows: Vec<(i64, String, Option<String>, String)> = db
        .prepare("SELECT sequence,task_id,attempt_id,payload FROM evidence WHERE kind='worktree_owner_protocol' AND (task_id=?1 OR attempt_id=?2) ORDER BY sequence")?
        .query_map(params![task, attempt], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    let [(sequence, row_task, row_attempt, payload)] = rows.as_slice() else {
        return Err(StateError::LaunchBlocked);
    };
    let proof: crate::ownership::WorktreeOwnerProtocol = parse_saved(payload)?;
    let dispatch_count: i64 = db.query_row(
        "SELECT COUNT(*) FROM intents WHERE kind='supervisor_dispatch' AND task_id=?1 AND attempt_id=?2",
        params![task, attempt], |row| row.get(0),
    )?;
    if row_task != task
        || row_attempt.as_deref() != Some(attempt)
        || !proof.matches_attempt(task, attempt)
        || dispatch_count != 1
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(*sequence)
}

pub(crate) fn validate_verified_open_pr(
    tx: &Transaction<'_>,
    task_id: &str,
    _attempt_id: &str,
    proof: &VerifiedOpenPr,
) -> Result<(), StateError> {
    let state: Option<String> = tx
        .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
            row.get(0)
        })
        .optional()?;
    if state.as_deref() != Some("held") {
        return Err(StateError::LaunchBlocked);
    }
    let selection_payload: String = tx.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection'",
        [task_id],
        |row| row.get(0),
    )?;
    let selection: SelectionEvidence = serde_json::from_str(&selection_payload)?;
    let intent_payload: String = tx.query_row(
        "SELECT detail FROM intents WHERE task_id=?1 AND kind='worktree_create'",
        [task_id],
        |row| row.get(0),
    )?;
    let intent: WorktreeIntent = serde_json::from_str(&intent_payload)?;
    let identity_payload: String = tx.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='worktree_created'",
        [task_id],
        |row| row.get(0),
    )?;
    let identity: WorktreeIdentity = serde_json::from_str(&identity_payload)?;
    validate_pr_identity(&selection, &intent, &identity, proof)?;
    let duplicate: i64 = tx.query_row(
        "SELECT COUNT(*) FROM evidence WHERE kind='verified_open_pr' AND json_extract(payload,'$.id')=?1",
        [proof.id],
        |row| row.get(0),
    )?;
    let existing: i64 = tx.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='verified_open_pr'",
        [task_id],
        |row| row.get(0),
    )?;
    if duplicate != 0 || existing != 0 {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

pub(crate) fn validate_pr_identity(
    selection: &SelectionEvidence,
    intent: &WorktreeIntent,
    identity: &WorktreeIdentity,
    proof: &VerifiedOpenPr,
) -> Result<(), StateError> {
    let mapping = &selection.candidate.mapping;
    if intent.branch != identity.branch
        || intent.repository != identity.repository
        || intent.base != identity.base
        || identity.branch.is_empty()
        || identity.head.is_empty()
        || identity.repository != mapping.code_repository
        || identity.branch != proof.head_branch
        || proof.repository_id == 0
        || proof.repository != mapping.code_repository
        || proof.base_branch != mapping.base_branch
        || proof.head_repository != mapping.allowed_pr_head_repository
        || proof.author != mapping.allowed_pr_author
        || proof.active_login != mapping.allowed_pr_author
        || proof.tracker_issue_url != selection.candidate.issue_url
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
