//! Base updates leave head adoption to the existing claim/receipt/lineage proof.
use super::types::{MergeError, now, verify_account};
use crate::{
    github::pull_request::{PrMerger, merge_validation::validate_identity},
    model::VerifiedOpenPr,
    pr_merge::{BranchUpdate, TrackerClosureEvidence, TrackerTarget},
    state::{StateError, StateStore, journal, merge_followup, pr_merge, task_records},
};
use serde_json::Value;

pub(crate) fn close_tracker<Q: PrMerger>(
    store: &mut StateStore,
    task: &str,
    prs: &mut Q,
) -> Result<(), MergeError> {
    let merge = pr_merge::merged(store, task)?.ok_or(StateError::LaunchBlocked)?;
    let selection =
        task_records::selection_evidence(store, task)?.ok_or(StateError::InvalidSelection)?;
    let c = selection.candidate;
    let target = TrackerTarget {
        repository: c.repository,
        number: c.issue_number,
        node_id: c.issue_node_id,
        url: c.issue_url,
    };
    let intent = merge_followup::closure_intent(store, task)?;
    if intent.as_ref().is_some_and(|prior| prior != &target) {
        return Err(MergeError::Refused("tracker close identity changed"));
    }
    verify_account(prs, &merge.authorization.pr)?;
    let mut issue = prs.tracker_issue(&target)?;
    if issue.target != target {
        return Err(MergeError::Refused("tracker read identity changed"));
    }
    if issue.closed_at.is_none() {
        if intent.is_some() {
            return Err(MergeError::Refused(
                "tracker close unresolved; no repeated write authorized",
            ));
        }
        merge_followup::begin_close(store, task, &target)?;
        prs.close_tracker(&merge, &target)?;
        issue = prs.tracker_issue(&target)?;
        if issue.target != target || issue.closed_at.is_none() {
            return Err(MergeError::Refused("tracker close not confirmed"));
        }
    }
    let proof = TrackerClosureEvidence {
        issue,
        merge,
        closed_by_luthor: merge_followup::closure_intent(store, task)?.is_some(),
        observed_at: now()?,
    };
    merge_followup::record_close(store, task, &proof)?;
    Ok(())
}

pub(crate) fn updated_head(
    store: &StateStore,
    task: &str,
    saved: &VerifiedOpenPr,
    detail: &Value,
) -> Result<bool, MergeError> {
    if detail.pointer("/head/sha").and_then(Value::as_str) == Some(&saved.head_commit_sha) {
        return Ok(false);
    }
    let Some(intent) = merge_followup::update(store, task, &saved.head_commit_sha)? else {
        return Ok(false);
    };
    if intent.pr != *saved {
        return Err(MergeError::Refused("branch update proof changed"));
    }
    let mut comparable = detail.clone();
    comparable["head"]["sha"] = serde_json::json!(saved.head_commit_sha);
    validate_identity(&comparable, saved)?;
    if detail["state"] != "open" || detail["merged"] != false {
        return Err(MergeError::Refused("updated PR no longer open"));
    }
    // This is NOT verification of the new head. Repair must prove and record
    // its base-merge lineage before any merge/check authorization can use it.
    Ok(true)
}

pub(crate) fn update_eligible(detail: &Value) -> bool {
    detail["mergeable_state"] == "behind" && detail["mergeable"] == true && detail["draft"] == false
}

pub(crate) fn update_branch<Q: PrMerger>(
    store: &mut StateStore,
    task: &str,
    saved: &VerifiedOpenPr,
    prs: &mut Q,
) -> Result<String, MergeError> {
    if merge_followup::update(store, task, &saved.head_commit_sha)?.is_some() {
        return Err(MergeError::Refused(
            "branch update unresolved; no repeated write authorized",
        ));
    }
    let proof = BranchUpdate {
        pr: saved.clone(),
        observed_at: now()?,
    };
    merge_followup::begin_update(store, task, &proof)?;
    prs.update_branch(saved)?;
    journal::record_evidence(
        store,
        task,
        Some(&saved.attempt_id),
        "pr_branch_updated",
        &serde_json::to_string(&proof).map_err(StateError::from)?,
    )?;
    Ok("base merge requested; await verified new head and checks".into())
}
