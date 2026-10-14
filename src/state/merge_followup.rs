//! Follow-up intents are append-only and bound to the verified PR/selection.
use super::{StateError, StateStore, cleanup_records, journal, proofs};
use crate::pr_merge::{BranchUpdate, TrackerClosureEvidence, TrackerTarget};
use rusqlite::params;

pub(crate) fn closure(
    store: &StateStore,
    task: &str,
) -> Result<Option<TrackerClosureEvidence>, StateError> {
    proofs::unique_payload(&store.connection, true, task, None, "tracker_closed")?
        .map(|payload| proofs::parse_saved(&payload))
        .transpose()
}

pub(crate) fn closure_intent(
    store: &StateStore,
    task: &str,
) -> Result<Option<TrackerTarget>, StateError> {
    proofs::unique_payload(&store.connection, false, task, None, "tracker_close")?
        .map(|payload| proofs::parse_saved(&payload))
        .transpose()
}

pub(crate) fn begin_close(
    store: &mut StateStore,
    task: &str,
    target: &TrackerTarget,
) -> Result<(), StateError> {
    if super::pr_merge::merged(store, task)?.is_none() || closure_intent(store, task)?.is_some() {
        return Err(StateError::LaunchBlocked);
    }
    journal::record_intent(
        store,
        &format!("tracker-close-{task}"),
        task,
        None,
        "tracker_close",
        &serde_json::to_string(target)?,
    )?;
    if closure_intent(store, task)?.as_ref() != Some(target) {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

pub(crate) fn record_close(
    store: &mut StateStore,
    task: &str,
    proof: &TrackerClosureEvidence,
) -> Result<(), StateError> {
    if proof.issue.closed_at.is_none()
        || closure(store, task)?.is_some()
        || super::pr_merge::merged(store, task)?.as_ref() != Some(&proof.merge)
        || (proof.closed_by_luthor
            && closure_intent(store, task)?.as_ref() != Some(&proof.issue.target))
    {
        return Err(StateError::LaunchBlocked);
    }
    journal::record_evidence(
        store,
        task,
        None,
        "tracker_closed",
        &serde_json::to_string(proof)?,
    )?;
    Ok(())
}

pub(crate) fn update(
    store: &StateStore,
    task: &str,
    head: &str,
) -> Result<Option<BranchUpdate>, StateError> {
    let payloads: Vec<String> = store.connection.prepare(
        "SELECT detail FROM intents WHERE task_id=?1 AND kind='pr_branch_update' AND json_extract(detail,'$.pr.head_commit_sha')=?2"
    )?.query_map(params![task, head], |row| row.get(0))?.collect::<Result<_,_>>()?;
    match payloads.as_slice() {
        [] => Ok(None),
        [payload] => Ok(Some(proofs::parse_saved(payload)?)),
        _ => Err(StateError::LaunchBlocked),
    }
}

pub(crate) fn begin_update(
    store: &mut StateStore,
    task: &str,
    proof: &BranchUpdate,
) -> Result<(), StateError> {
    if cleanup_records::verified_pr(store, task)? != proof.pr
        || update(store, task, &proof.pr.head_commit_sha)?.is_some()
    {
        return Err(StateError::LaunchBlocked);
    }
    journal::record_intent(
        store,
        &format!("pr-update-{task}-{}", proof.pr.head_commit_sha),
        task,
        None,
        "pr_branch_update",
        &serde_json::to_string(proof)?,
    )?;
    if update(store, task, &proof.pr.head_commit_sha)?.as_ref() != Some(proof) {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
