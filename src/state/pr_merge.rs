use super::{StateError, StateStore, cleanup_records, journal, proofs};
use crate::pr_merge::{MergeAuthorization, MergeEvidence};

pub(crate) fn authorization(
    store: &StateStore,
    task: &str,
) -> Result<Option<MergeAuthorization>, StateError> {
    proofs::unique_payload(&store.connection, false, task, None, "pr_merge")?
        .map(|payload| proofs::parse_saved(&payload))
        .transpose()
}

pub(crate) fn completed(store: &StateStore, task: &str) -> Result<bool, StateError> {
    Ok(merged(store, task)?.is_some())
}

pub(crate) fn merged(store: &StateStore, task: &str) -> Result<Option<MergeEvidence>, StateError> {
    let evidence: Option<MergeEvidence> =
        proofs::unique_payload(&store.connection, true, task, None, "pr_merged")?
            .map(|payload| proofs::parse_saved(&payload))
            .transpose()?;
    Ok(evidence)
}

pub(crate) fn begin(
    store: &mut StateStore,
    task: &str,
    auth: &MergeAuthorization,
) -> Result<(), StateError> {
    if cleanup_records::verified_pr(store, task)? != auth.pr
        || authorization(store, task)?.is_some()
    {
        return Err(StateError::LaunchBlocked);
    }
    journal::record_intent(
        store,
        &format!("pr-merge-{task}"),
        task,
        None,
        "pr_merge",
        &serde_json::to_string(auth)?,
    )?;
    if authorization(store, task)?.as_ref() != Some(auth) {
        return Err(StateError::InvalidSelection);
    }
    Ok(())
}

pub(crate) fn complete(
    store: &mut StateStore,
    task: &str,
    evidence: &MergeEvidence,
) -> Result<(), StateError> {
    if authorization(store, task)?.as_ref() != Some(&evidence.authorization)
        || evidence.merged_head != evidence.authorization.pr.head_commit_sha
        || completed(store, task)?
    {
        return Err(StateError::LaunchBlocked);
    }
    journal::record_evidence(
        store,
        task,
        None,
        "pr_merged",
        &serde_json::to_string(evidence)?,
    )?;
    Ok(())
}
