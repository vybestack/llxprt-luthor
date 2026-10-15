//! Tracker closure for externally delivered PRs, without inventing merge authorization.
use super::types::{ErrorCategory, error};
use super::{
    GhPullRequestReader, LookupError, PullRequestReader, merge_followup, merge_validation,
    merge_write,
};
use crate::{model::VerifiedOpenPr, pr_merge::TrackerTarget};
use serde_json::{Value, json};

pub(crate) fn close(
    reader: &mut GhPullRequestReader,
    pr: &VerifiedOpenPr,
    terminal: &Value,
    target: &TrackerTarget,
) -> Result<(), LookupError> {
    if reader.authenticated_identity()? != pr.active_login || pr.active_login != "acoliver" {
        return Err(invalid("terminal-tracker-account-changed"));
    }
    let detail = reader.terminal_detail(&pr.repository, pr.number)?;
    let mut comparable = detail.clone();
    comparable["head"]["sha"] = json!(pr.head_commit_sha);
    merge_validation::validate_identity(&comparable, pr)?;
    if detail["state"] != "closed"
        || detail["merged"] != true
        || terminal["state"] != "merged"
        || detail["head"]["sha"] != terminal["head_commit_sha"]
        || detail["merge_commit_sha"] != terminal["merge_commit_sha"]
        || detail["merged_at"] != terminal["merged_at"]
        || target.url != pr.tracker_issue_url
    {
        return Err(invalid("terminal-tracker-proof-changed"));
    }
    if merge_followup::issue(reader, target)?.closed_at.is_some() {
        return Err(invalid("terminal-tracker-already-closed"));
    }
    // One explicit REST write, no hidden reads/writes in `gh issue close`.
    merge_write::write(
        reader,
        &[
            "api",
            "--method",
            "PATCH",
            &format!("repos/{}/issues/{}", target.repository, target.number),
            "-f",
            "state=closed",
            "-f",
            "state_reason=completed",
        ],
    )?;
    Ok(())
}

fn invalid(code: &'static str) -> LookupError {
    error(ErrorCategory::Malformed, code, None)
}
