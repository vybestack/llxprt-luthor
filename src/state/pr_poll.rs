//! Persist optional-poll deadlines across daemon invocations and upgrades.
use super::{StateError, StateStore};
use rusqlite::{OptionalExtension, params};

pub(crate) fn begin(
    store: &StateStore,
    task: &str,
    purpose: &str,
    identity: &str,
) -> Result<bool, StateError> {
    if !crate::github::budget::optional_allowed() || !crate::github::budget::task_allowed(task) {
        return Ok(false);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| std::io::Error::other("clock unavailable"))?
        .as_secs();
    let prior: Option<(String, u64)> = store
        .connection
        .query_row(
            "SELECT identity,observed_at FROM pr_polls WHERE task_id=?1 AND purpose=?2",
            params![task, purpose],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if prior.is_some_and(|(key, at)| {
        key == identity && now.saturating_sub(at) < crate::github::budget::interval()
    }) {
        return Ok(false);
    }
    store.connection.execute(
        "INSERT INTO pr_polls(task_id,purpose,identity,observed_at) VALUES(?1,?2,?3,?4)
         ON CONFLICT(task_id,purpose) DO UPDATE SET identity=excluded.identity,observed_at=excluded.observed_at",
        params![task,purpose,identity,now],
    )?;
    Ok(true)
}

pub(crate) fn completed_candidates(store: &StateStore) -> Result<Vec<String>, StateError> {
    Ok(store
        .connection
        .prepare("SELECT id FROM tasks WHERE state='pr_complete' ORDER BY rowid")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

pub(crate) fn terminal_close_pending(store: &StateStore, task: &str) -> Result<bool, StateError> {
    Ok(store.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND kind='terminal_tracker_close')",
        [task],
        |r| r.get(0),
    )?)
}

pub(crate) fn changed_mapping(
    store: &mut StateStore,
    task: &str,
    facts: &str,
) -> Result<bool, StateError> {
    changed_facts(store, task, "merge_mapping_mismatch", facts)
}

pub(crate) fn changed_repair_mapping(
    store: &mut StateStore,
    task: &str,
    facts: &str,
) -> Result<bool, StateError> {
    changed_facts(store, task, "repair_mapping_mismatch", facts)
}

fn changed_facts(
    store: &mut StateStore,
    task: &str,
    kind: &str,
    facts: &str,
) -> Result<bool, StateError> {
    let prior: Option<String> = store.connection.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind=?2 ORDER BY sequence DESC LIMIT 1",
        params![task, kind], |row| row.get(0)).optional()?;
    if prior.as_deref() == Some(facts) {
        return Ok(false);
    }
    super::journal::record_evidence(store, task, None, kind, facts)?;
    Ok(true)
}
