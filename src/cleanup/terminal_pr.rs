use crate::cleanup_error::CleanupError;
use crate::{
    github::pull_request::{LookupResult, PullRequestReader, lookup},
    model::VerifiedOpenPr,
};
use serde_json::Value;

/// A fresh detail read of the exact previously verified PR, not a title search
/// or inference from an absent open-PR listing. Reopened PRs stay untouched.
pub(crate) fn observe<Q: PullRequestReader>(
    prs: &mut Q,
    saved: &VerifiedOpenPr,
) -> Result<Option<Value>, CleanupError> {
    if prs.authenticated_identity()? != saved.active_login {
        return Err(CleanupError::Conflict("cleanup PR identity changed"));
    }
    let detail = prs.detail(&saved.repository, saved.number)?;
    verify_identity(&detail, saved)?;
    let state = text(&detail, "/state")?;
    let merged = detail
        .get("merged")
        .and_then(Value::as_bool)
        .ok_or(CleanupError::Conflict("cleanup PR merged status missing"))?;
    match (state, merged) {
        ("open", false) => return Ok(None),
        ("closed", _) => {}
        _ => return Err(CleanupError::Conflict("cleanup PR state contradictory")),
    }
    if text(&detail, "/closed_at")?.is_empty()
        || (merged && text(&detail, "/merged_at")?.is_empty())
        || (!merged && !detail.get("merged_at").is_some_and(Value::is_null))
    {
        return Err(CleanupError::Conflict(
            "cleanup PR terminal timestamp missing",
        ));
    }
    if lookup(prs, &saved.repository, &saved.tracker_issue_url)? != LookupResult::Absent {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "id":saved.id,"number":saved.number,"url":saved.url,
        "repository":saved.repository,"state":if merged {"merged"} else {"closed"},
        "closed_at":detail["closed_at"],"merged_at":detail["merged_at"],
        "head_commit_sha":text(&detail, "/head/sha")?
    })))
}

fn verify_identity(detail: &Value, saved: &VerifiedOpenPr) -> Result<(), CleanupError> {
    for (pointer, expected) in [
        ("/id", saved.id),
        ("/number", saved.number),
        ("/base/repo/id", saved.repository_id),
        ("/head/repo/id", saved.head_repository_id),
    ] {
        if detail.pointer(pointer).and_then(Value::as_u64) != Some(expected) {
            return Err(CleanupError::Conflict(
                "cleanup PR numeric identity changed",
            ));
        }
    }
    for (pointer, expected) in [
        ("/html_url", saved.url.as_str()),
        ("/base/repo/full_name", &saved.repository),
        ("/base/ref", &saved.base_branch),
        ("/head/repo/full_name", &saved.head_repository),
        ("/head/ref", &saved.head_branch),
        ("/user/login", &saved.author),
    ] {
        if text(detail, pointer)? != expected {
            return Err(CleanupError::Conflict("cleanup PR identity changed"));
        }
    }
    if !text(detail, "/body")?
        .lines()
        .any(|line| line == format!("Tracker-Issue: {}", saved.tracker_issue_url))
    {
        return Err(CleanupError::Conflict("cleanup PR tracker link changed"));
    }
    Ok(())
}

fn text<'a>(detail: &'a Value, pointer: &str) -> Result<&'a str, CleanupError> {
    detail
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or(CleanupError::Conflict("cleanup PR detail missing"))
}
