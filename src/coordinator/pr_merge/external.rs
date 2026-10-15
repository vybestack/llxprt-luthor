//! A PR GitHub already merged outside Luthor, after its head advanced past the
//! saved verified head. Luthor never authorized that merge, so it records no
//! merge evidence. It retires the task as delivered only when the PR identity
//! is unchanged and the head moved purely by merges of the configured base.
use super::types::{MergeError, now};
use crate::{
    github::pull_request::merge_validation::{valid_sha, validate_identity},
    model::VerifiedOpenPr,
    state::{
        StateError, StateStore,
        retirement::{RetiredPhase, Retirement},
    },
    worktree::{WorktreeError, pr_head},
};
use serde_json::{Value, json};

pub(crate) fn proof(
    store: &StateStore,
    task: &str,
    saved: &VerifiedOpenPr,
    detail: &Value,
) -> Result<Retirement, MergeError> {
    // Everything but the head must match the saved proof, exactly as strictly
    // as before Luthor merges.
    let mut comparable = detail.clone();
    comparable["head"]["sha"] = json!(saved.head_commit_sha);
    validate_identity(&comparable, saved)
        .map_err(|_| MergeError::Refused("merged PR identity differs from the verified PR"))?;
    let head = detail
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .filter(|sha| valid_sha(sha))
        .ok_or(MergeError::Refused("merged PR head is not a commit id"))?;
    let merge_commit = detail["merge_commit_sha"]
        .as_str()
        .filter(|sha| valid_sha(sha))
        .ok_or(MergeError::Refused("merged PR has no merge commit"))?;
    if detail["state"] != "closed"
        || !detail["merged_at"]
            .as_str()
            .is_some_and(|time| !time.is_empty())
    {
        return Err(MergeError::Refused(
            "merged PR terminal state is incomplete",
        ));
    }
    let lineage = if head == saved.head_commit_sha {
        Value::Null
    } else {
        serde_json::to_value(
            pr_head::verify_merged_lineage(store, task, &saved.head_commit_sha, head, saved.number)
                .map_err(lineage_refusal)?,
        )
        .map_err(StateError::from)?
    };
    let reason = if head == saved.head_commit_sha {
        format!(
            "saved verified PR {} merged outside Luthor at its verified head",
            saved.number
        )
    } else {
        format!(
            "saved verified PR {} merged outside Luthor after its head advanced only by merges of the base",
            saved.number
        )
    };
    let delivery = Retirement {
        phase: RetiredPhase::PrMerged,
        reason: reason.clone(),
        actor: None,
        observed_at_unix_secs: now()?,
        evidence: json!({
            "pr": {"id": saved.id, "number": saved.number, "url": saved.url,
                "repository": saved.repository, "state": "merged",
                "merged_at": detail["merged_at"], "merge_commit_sha": merge_commit,
                "head_commit_sha": head, "verified_head_commit_sha": saved.head_commit_sha},
            "base_merge_lineage": lineage,
            "luthor_merge_evidence": "none",
        }),
    };
    Ok(delivery)
}

pub(crate) fn lineage_refusal(error: WorktreeError) -> MergeError {
    match error {
        WorktreeError::Conflict(reason) | WorktreeError::Git(reason) => MergeError::Refused(reason),
        WorktreeError::State(error) => MergeError::State(error),
        _ => MergeError::Refused("base-merge lineage of the merged PR could not be checked"),
    }
}
