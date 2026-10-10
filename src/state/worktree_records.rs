use super::{database::StateStore, task_records::selection_evidence};
use crate::model::{
    SelectionEvidence, StateError, WorktreeIdentity, WorktreeIntent, WorktreeRecord,
};
use rusqlite::{OptionalExtension, params};

pub fn claimed_worktree_context(
    store: &StateStore,
    task_id: &str,
) -> Result<SelectionEvidence, StateError> {
    let phase: Option<String> = store
        .connection
        .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
            row.get(0)
        })
        .optional()?;
    let verified: i64 = store.connection.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified'",
        [task_id],
        |row| row.get(0),
    )?;
    let claim: i64 = store.connection.query_row(
        "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND kind='claim_assignment'",
        [task_id],
        |row| row.get(0),
    )?;
    if phase.as_deref() != Some("claimed") || verified != 1 || claim != 1 {
        return Err(StateError::InvalidSelection);
    }
    selection_evidence(store, task_id)?.ok_or(StateError::InvalidSelection)
}

pub fn worktree_record(
    store: &StateStore,
    task_id: &str,
) -> Result<Option<WorktreeRecord>, StateError> {
    let mut intents = store.connection.prepare(
        "SELECT detail FROM intents WHERE task_id=?1 AND kind='worktree_create' ORDER BY sequence",
    )?;
    let details = intents
        .query_map([task_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if details.len() > 1 {
        return Err(StateError::InvalidSelection);
    }
    let Some(detail) = details.first() else {
        return Ok(None);
    };
    let intent: WorktreeIntent = serde_json::from_str(detail)?;
    let mut evidence = store.connection.prepare(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind='worktree_created' ORDER BY sequence",
    )?;
    let payloads = evidence
        .query_map([task_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if payloads.len() > 1 {
        return Err(StateError::InvalidSelection);
    }
    let identity = payloads
        .first()
        .map(|payload| serde_json::from_str(payload))
        .transpose()?;
    Ok(Some(WorktreeRecord { intent, identity }))
}

pub fn begin_worktree(
    store: &mut StateStore,
    task_id: &str,
    intent: &WorktreeIntent,
) -> Result<(), StateError> {
    let tx = store.connection.transaction()?;
    let phase: Option<String> = tx
        .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
            row.get(0)
        })
        .optional()?;
    let verified: i64 = tx.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified'",
        [task_id],
        |row| row.get(0),
    )?;
    let claimed: i64 = tx.query_row(
        "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND kind='claim_assignment'",
        [task_id],
        |row| row.get(0),
    )?;
    let previous: i64 = tx.query_row(
        "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND kind='worktree_create'",
        [task_id],
        |row| row.get(0),
    )?;
    if phase.as_deref() != Some("claimed") || verified != 1 || claimed != 1 || previous != 0 {
        return Err(StateError::InvalidSelection);
    }
    tx.execute(
        "INSERT INTO intents(id,task_id,kind,detail) VALUES(?1,?2,'worktree_create',?3)",
        params![
            format!("worktree-{task_id}"),
            task_id,
            serde_json::to_string(intent)?
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn finish_worktree(
    store: &mut StateStore,
    task_id: &str,
    identity: &WorktreeIdentity,
) -> Result<(), StateError> {
    let tx = store.connection.transaction()?;
    let intent: Option<String> = tx
        .query_row(
            "SELECT detail FROM intents WHERE task_id=?1 AND kind='worktree_create'",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(intent) = intent else {
        return Err(StateError::InvalidSelection);
    };
    let intent: WorktreeIntent = serde_json::from_str(&intent)?;
    let existing: i64 = tx.query_row(
        "SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='worktree_created'",
        [task_id],
        |row| row.get(0),
    )?;
    if existing != 0
        || intent.path != identity.path
        || intent.branch != identity.branch
        || intent.base != identity.base
        || intent.repository != identity.repository
    {
        return Err(StateError::InvalidSelection);
    }
    tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'worktree_created',?2)",
        params![task_id, serde_json::to_string(identity)?])?;
    tx.commit()?;
    Ok(())
}
