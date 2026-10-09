use super::{database::StateStore, proofs};
use crate::model::StateError;
use rusqlite::params;

pub fn record_intent(
    store: &mut StateStore,
    id: &str,
    task_id: &str,
    attempt_id: Option<&str>,
    kind: &str,
    detail: &str,
) -> Result<(), StateError> {
    store.connection.execute(
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,?4,?5)",
        params![id, task_id, attempt_id, kind, detail],
    )?;
    Ok(())
}

pub fn record_evidence(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: Option<&str>,
    kind: &str,
    payload: &str,
) -> Result<i64, StateError> {
    store.connection.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,?3,?4)",
        params![task_id, attempt_id, kind, payload],
    )?;
    Ok(store.connection.last_insert_rowid())
}

/// A reserved attempt remains reserved even if the supervisor is unreachable.
/// Repeated requests reuse the first durable intent rather than adding a new one.
pub fn record_stop_intent(
    store: &mut StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<(), StateError> {
    let tx = store.connection.transaction()?;
    let active: i64 = tx.query_row(
        "SELECT COUNT(*) FROM attempts a JOIN reservations r ON r.attempt_id=a.id
             JOIN tasks t ON t.id=a.task_id
             WHERE a.id=?1 AND a.task_id=?2 AND a.lifecycle='launch_intended'
               AND a.outcome IS NULL AND r.task_id=?2 AND r.status='reserved' AND t.state='held'
               AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?1 AND task_id=?2 AND kind='launch')=1",
        params![attempt_id, task_id], |row| row.get(0)
    )?;
    if active != 1 {
        return Err(StateError::LaunchBlocked);
    }
    let prior: Vec<String> = tx
        .prepare("SELECT detail FROM intents WHERE attempt_id=?1 AND kind='stop'")?
        .query_map([attempt_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let detail = serde_json::json!({"task_id":task_id,"attempt_id":attempt_id}).to_string();
    if prior.is_empty() {
        tx.execute(
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail)
                 VALUES(?1,?2,?3,'stop',?4)",
            params![format!("stop-{attempt_id}"), task_id, attempt_id, detail],
        )?;
    } else if prior != [detail] {
        return Err(StateError::LaunchBlocked);
    }
    tx.commit()?;
    Ok(())
}

pub fn stop_intent(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<Option<String>, StateError> {
    intent_payload(store, task_id, attempt_id, "stop")
}

pub(crate) fn evidence_payload(
    store: &StateStore,
    task_id: &str,
    attempt_id: Option<&str>,
    kind: &str,
) -> Result<Option<String>, StateError> {
    proofs::unique_payload(&store.connection, true, task_id, attempt_id, kind)
}

pub fn evidence_payloads(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    kind: &str,
) -> Result<Vec<String>, StateError> {
    let mut statement = store.connection.prepare(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind=?3 ORDER BY sequence",
    )?;
    let payloads = statement
        .query_map(params![task_id, attempt_id, kind], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(payloads)
}

pub(crate) fn intent_payload(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    kind: &str,
) -> Result<Option<String>, StateError> {
    proofs::unique_payload(&store.connection, false, task_id, Some(attempt_id), kind)
}

pub fn evidence_kinds(store: &StateStore, task_id: &str) -> Result<Vec<String>, StateError> {
    let mut statement = store
        .connection
        .prepare("SELECT kind FROM evidence WHERE task_id=?1 ORDER BY sequence")?;
    let rows = statement.query_map([task_id], |r| r.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
