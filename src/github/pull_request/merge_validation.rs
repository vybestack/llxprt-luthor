//! Revalidate immutable ownership even on terminal reads, without requiring an open PR.
use super::{
    LookupError,
    types::{ErrorCategory, error},
};
use crate::model::VerifiedOpenPr;
use serde_json::Value;

pub(crate) fn validate_identity(detail: &Value, saved: &VerifiedOpenPr) -> Result<(), LookupError> {
    let valid = detail["id"].as_u64() == Some(saved.id)
        && detail["number"].as_u64() == Some(saved.number)
        && detail["html_url"].as_str() == Some(&saved.url)
        && detail.pointer("/head/sha").and_then(Value::as_str) == Some(&saved.head_commit_sha)
        && valid_sha(&saved.head_commit_sha)
        && detail.pointer("/base/repo/id").and_then(Value::as_u64) == Some(saved.repository_id)
        && detail
            .pointer("/base/repo/full_name")
            .and_then(Value::as_str)
            == Some(&saved.repository)
        && detail.pointer("/base/ref").and_then(Value::as_str) == Some(&saved.base_branch)
        && detail.pointer("/head/repo/id").and_then(Value::as_u64)
            == Some(saved.head_repository_id)
        && detail
            .pointer("/head/repo/full_name")
            .and_then(Value::as_str)
            == Some(&saved.head_repository)
        && detail.pointer("/head/ref").and_then(Value::as_str) == Some(&saved.head_branch)
        && detail.pointer("/user/login").and_then(Value::as_str) == Some(&saved.author)
        && detail["created_at"].as_str() == Some(&saved.created_at)
        && detail["body"].as_str().is_some_and(|body| {
            body.lines()
                .any(|line| line == format!("Tracker-Issue: {}", saved.tracker_issue_url))
        });
    if !valid {
        return Err(error(
            ErrorCategory::Malformed,
            "merge-pr-identity-changed",
            None,
        ));
    }
    Ok(())
}

pub(crate) fn valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}
