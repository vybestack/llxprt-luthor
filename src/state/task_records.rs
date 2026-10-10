use super::{database::StateStore, proofs};
use crate::{
    config::Config,
    eligibility::Candidate,
    model::{EffectiveConfigSnapshot, SelectionEvidence, StateError},
};
use rusqlite::{OptionalExtension, params};

fn validate_selection(candidate: &Candidate, config: &Config) -> Result<(), StateError> {
    let source = &candidate.source;
    let mapping = &candidate.mapping;
    let identity_matches = !candidate.project_id.is_empty()
        && !candidate.item_id.is_empty()
        && !candidate.issue_node_id.is_empty()
        && !candidate.tracker_repo_id.is_empty()
        && candidate.issue_number > 0
        && candidate.issue_url
            == format!(
                "https://github.com/{}/issues/{}",
                candidate.repository, candidate.issue_number
            );
    let milestone_matches = source
        .milestone
        .as_ref()
        .is_none_or(|title| candidate.milestone_title.as_ref() == Some(title));
    if !config.sources.contains(source)
        || !config.mappings.contains(mapping)
        || candidate.project_id != source.project_id
        || !source.repositories.contains(&candidate.repository)
        || candidate.repository != mapping.tracker_repository
        || candidate.marker != source.ready_marker
        || !milestone_matches
        || !identity_matches
    {
        return Err(StateError::InvalidSelection);
    }
    Ok(())
}

pub fn create_task(
    store: &mut StateStore,
    id: &str,
    candidate: &Candidate,
    config_revision: &str,
    config: &Config,
) -> Result<(), StateError> {
    config.validate().map_err(|_| StateError::InvalidConfig)?;
    validate_selection(candidate, config)?;
    let repo_id = &candidate.tracker_repo_id;
    let issue_node_id = &candidate.issue_node_id;
    let tx = store.connection.transaction()?;
    let exists: Option<String> = tx
        .query_row(
            "SELECT id FROM tasks WHERE tracker_repo_id=?1 AND issue_node_id=?2",
            params![repo_id, issue_node_id],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_some() {
        return Err(StateError::DuplicateTask(
            repo_id.into(),
            issue_node_id.into(),
        ));
    }
    tx.execute("INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision) VALUES(?1,?2,?3,?4,?5,'preparing',?6)", params![id, repo_id, issue_node_id, candidate.repository, candidate.issue_number, config_revision])?;
    let evidence = SelectionEvidence {
        candidate: candidate.clone(),
        config_revision: config_revision.to_owned(),
        effective_config: EffectiveConfigSnapshot::from(config),
    };
    let payload = serde_json::to_string(&evidence)?;
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'selection',?2)",
        params![id, payload],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn existing_issue(
    store: &StateStore,
    repo_id: &str,
    issue_id: &str,
) -> Result<bool, StateError> {
    Ok(store.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE tracker_repo_id=?1 AND issue_node_id=?2)",
        params![repo_id, issue_id],
        |row| row.get(0),
    )?)
}

/// Looks up persisted selection only; this does not establish eligibility or authorize dispatch.
pub fn existing_target(
    store: &StateStore,
    repository: &str,
    issue_number: u64,
) -> Result<bool, StateError> {
    if repository.trim().is_empty() || issue_number == 0 {
        return Err(StateError::InvalidTarget);
    }
    let mut statement = store
        .connection
        .prepare("SELECT COUNT(*) FROM tasks WHERE repository=?1 AND issue_number=?2")?;
    let count: i64 = statement.query_row(params![repository, issue_number], |row| row.get(0))?;
    match count {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(StateError::AmbiguousTarget(
            repository.to_owned(),
            issue_number,
        )),
    }
}

/// Record a non-retriable failure without releasing any launch reservation.
pub fn hold_task(store: &mut StateStore, task_id: &str, reason: &str) -> Result<(), StateError> {
    let tx = store.connection.transaction()?;
    let changed = tx.execute("UPDATE tasks SET state='held' WHERE id=?1", [task_id])?;
    if changed != 1 {
        return Err(StateError::InvalidSelection);
    }
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'held_reason',?2)",
        params![task_id, reason],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn has_attempt(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<bool, StateError> {
    store
        .connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE task_id=?1 AND id=?2)",
            rusqlite::params![task_id, attempt_id],
            |row| row.get(0),
        )
        .map_err(StateError::from)
}

pub fn latest_attempt(store: &StateStore, task_id: &str) -> Result<Option<String>, StateError> {
    store
        .connection
        .query_row(
            "SELECT id FROM attempts WHERE task_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(StateError::from)
}

pub fn task_phase(store: &StateStore, task_id: &str) -> Result<Option<String>, StateError> {
    store
        .connection
        .query_row("SELECT state FROM tasks WHERE id=?1", [task_id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(StateError::from)
}

pub fn held_reason(store: &StateStore, task_id: &str) -> Result<Option<String>, StateError> {
    store.connection.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind='held_reason' ORDER BY sequence DESC LIMIT 1",
        [task_id], |row| row.get(0),
    ).optional().map_err(StateError::from)
}

pub fn claim_assignment_login(
    store: &StateStore,
    task_id: &str,
) -> Result<Option<String>, StateError> {
    let evidence = selection_evidence(store, task_id)?;
    Ok(evidence.map(|evidence| evidence.effective_config.assignment_login))
}

pub fn source_claim_intent(
    store: &StateStore,
    task_id: &str,
) -> Result<Option<String>, StateError> {
    proofs::unique_payload(&store.connection, false, task_id, None, "claim_assignment")
}

pub fn record_claim_intent(
    store: &mut StateStore,
    task_id: &str,
    principal: &str,
    repository: &str,
    number: u64,
) -> Result<(), StateError> {
    let tx = store.connection.transaction()?;
    let prior: i64 = tx.query_row(
        "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND kind='claim_assignment'",
        [task_id],
        |r| r.get(0),
    )?;
    if prior != 0 {
        return Err(StateError::InvalidSelection);
    }
    tx.execute(
        "INSERT INTO intents(id,task_id,kind,detail) VALUES(?1,?2,'claim_assignment',?3)",
        params![
            format!("claim-{task_id}"),
            task_id,
            serde_json::json!({"principal":principal,"repository":repository,"number":number})
                .to_string()
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn set_task_phase(
    store: &mut StateStore,
    task_id: &str,
    phase: &str,
) -> Result<(), StateError> {
    let changed = store.connection.execute(
        "UPDATE tasks SET state=?2 WHERE id=?1",
        params![task_id, phase],
    )?;
    if changed != 1 {
        return Err(StateError::InvalidSelection);
    }
    Ok(())
}

pub fn task_count(store: &StateStore) -> Result<usize, StateError> {
    Ok(store
        .connection
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?)
}

pub fn selection_evidence(
    store: &StateStore,
    task_id: &str,
) -> Result<Option<SelectionEvidence>, StateError> {
    let payload: Option<String> = store.connection.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind='selection' ORDER BY sequence LIMIT 1",
        [task_id], |row| row.get(0),
    ).optional()?;
    payload
        .map(|payload| serde_json::from_str(&payload).map_err(StateError::from))
        .transpose()
}
