//! Exact-head base updates and explicit tracker closure, never retried writes.
use super::types::{ErrorCategory, error};
use super::{GhPullRequestReader, LookupError, PullRequestReader, merge_validation, merge_write};
use crate::{
    model::VerifiedOpenPr,
    pr_merge::{MergeEvidence, TrackerIssue, TrackerTarget},
};
use serde_json::Value;

pub(crate) fn update(
    reader: &mut GhPullRequestReader,
    pr: &VerifiedOpenPr,
) -> Result<(), LookupError> {
    merge_write::authorize(reader, pr)?;
    let detail = reader.api(&[
        "api",
        &format!("repos/{}/pulls/{}", pr.repository, pr.number),
    ])?;
    merge_validation::validate_identity(&detail, pr)?;
    if detail["mergeable_state"] != "behind" || detail["merged"] != false {
        return Err(invalid("update-no-longer-behind"));
    }
    let node = detail["node_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| invalid("update-node-missing"))?;
    // GraphQL exposes the update method explicitly. MERGE preserves history;
    // expectedHeadOid refuses races. No rebase, force or admin bypass exists.
    let query = "mutation($id:ID!,$head:GitObjectID!){updatePullRequestBranch(input:{pullRequestId:$id,expectedHeadOid:$head,updateMethod:MERGE}){pullRequest{id}}}";
    let output = merge_write::write(
        reader,
        &[
            "api",
            "graphql",
            "-f",
            &format!("query={query}"),
            "-f",
            &format!("id={node}"),
            "-f",
            &format!("head={}", pr.head_commit_sha),
        ],
    )?;
    let response: Value =
        serde_json::from_slice(&output).map_err(|_| invalid("update-response-invalid"))?;
    if response.get("errors").is_some()
        || response
            .pointer("/data/updatePullRequestBranch/pullRequest/id")
            .and_then(Value::as_str)
            != Some(node)
    {
        return Err(invalid("update-response-invalid"));
    }
    Ok(())
}

pub(crate) fn issue(
    reader: &GhPullRequestReader,
    target: &TrackerTarget,
) -> Result<TrackerIssue, LookupError> {
    let detail = reader.api(&[
        "api",
        &format!("repos/{}/issues/{}", target.repository, target.number),
    ])?;
    if detail["node_id"] != target.node_id
        || detail["number"] != target.number
        || detail["html_url"] != target.url
        || detail["repository_url"] != format!("https://api.github.com/repos/{}", target.repository)
        || detail.get("pull_request").is_some()
    {
        return Err(invalid("tracker-identity-changed"));
    }
    let closed_at = match (detail["state"].as_str(), detail.get("closed_at")) {
        (Some("open"), Some(Value::Null)) => None,
        (Some("closed"), Some(Value::String(at))) if !at.is_empty() => Some(at.clone()),
        _ => return Err(invalid("tracker-state-invalid")),
    };
    Ok(TrackerIssue {
        target: target.clone(),
        closed_at,
    })
}

pub(crate) fn close(
    reader: &mut GhPullRequestReader,
    merge: &MergeEvidence,
    target: &TrackerTarget,
) -> Result<(), LookupError> {
    let pr = &merge.authorization.pr;
    if reader.authenticated_identity()? != "acoliver" || pr.active_login != "acoliver" {
        return Err(invalid("tracker-account-changed"));
    }
    let detail = reader.api(&[
        "api",
        &format!("repos/{}/pulls/{}", pr.repository, pr.number),
    ])?;
    merge_validation::validate_identity(&detail, pr)?;
    if detail["state"] != "closed"
        || detail["merged"] != true
        || detail["merge_commit_sha"] != merge.merge_commit
        || merge.merged_head != pr.head_commit_sha
        || target.url != pr.tracker_issue_url
    {
        return Err(invalid("tracker-merge-proof-changed"));
    }
    if issue(reader, target)?.closed_at.is_some() {
        return Err(invalid("tracker-already-closed"));
    }
    let comment = format!(
        "Luthor merged {} at verified head {} (merge commit {}).",
        pr.url, merge.merged_head, merge.merge_commit
    );
    merge_write::write(
        reader,
        &[
            "issue",
            "close",
            &target.number.to_string(),
            "--repo",
            &target.repository,
            "--reason",
            "completed",
            "--comment",
            &comment,
        ],
    )?;
    Ok(())
}

fn invalid(code: &'static str) -> LookupError {
    error(ErrorCategory::Malformed, code, None)
}
