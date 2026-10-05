use serde_json::Value;

use super::types::{ErrorCategory, LookupError, PullRequestEvidence, error};

struct PullRequestIdentity {
    id: u64,
    number: u64,
    url: String,
}

struct BranchEvidence {
    repository_id: u64,
    repository: String,
    branch: String,
}

pub(crate) fn parse_evidence(
    value: &Value,
    repository: &str,
) -> Result<PullRequestEvidence, LookupError> {
    let identity = parse_identity(value, repository)?;
    let base = parse_branch(value.get("base").ok_or_else(invalid_details)?)?;
    let head = parse_branch(value.get("head").ok_or_else(invalid_details)?)?;
    let body = text(value, "/body")?;
    let author = text(value, "/user/login")?;
    if base.repository != repository
        || base.branch.is_empty()
        || head.branch.is_empty()
        || head.repository.is_empty()
        || author.is_empty()
    {
        return Err(invalid_details());
    }
    let draft = value
        .get("draft")
        .and_then(Value::as_bool)
        .ok_or_else(invalid_details)?;
    let created_at = nonempty_text(value, "/created_at")?;
    let commit_sha = nonempty_text(value, "/head/sha")?;
    let tracker_issue_url = body
        .lines()
        .find_map(|line| line.strip_prefix("Tracker-Issue: "))
        .unwrap_or("")
        .to_owned();
    Ok(PullRequestEvidence {
        id: identity.id,
        number: identity.number,
        url: identity.url,
        repository_id: base.repository_id,
        repository: repository.to_owned(),
        base_repository_id: base.repository_id,
        base_repository: base.repository,
        base_branch: base.branch,
        head_repository_id: head.repository_id,
        head_repository: head.repository,
        head_branch: head.branch,
        author: author.to_owned(),
        draft,
        tracker_issue_url,
        body: body.to_owned(),
        checks: parse_checks(value),
        created_at: created_at.to_owned(),
        commit_sha: commit_sha.to_owned(),
    })
}

fn parse_identity(value: &Value, repository: &str) -> Result<PullRequestIdentity, LookupError> {
    let id = positive_id(value, "/id")?;
    let number = positive_id(value, "/number")?;
    if text(value, "/state")? != "open" {
        return Err(invalid_details());
    }
    let url = nonempty_text(value, "/html_url")?;
    let expected_prefix = format!("https://github.com/{repository}/pull/");
    let url_number = url
        .strip_prefix(&expected_prefix)
        .ok_or_else(invalid_details)?;
    if url_number != number.to_string() {
        return Err(invalid_details());
    }
    Ok(PullRequestIdentity {
        id,
        number,
        url: url.to_owned(),
    })
}

fn parse_branch(value: &Value) -> Result<BranchEvidence, LookupError> {
    Ok(BranchEvidence {
        repository_id: positive_id(value, "/repo/id")?,
        repository: text(value, "/repo/full_name")?.to_owned(),
        branch: text(value, "/ref")?.to_owned(),
    })
}

fn positive_id(value: &Value, pointer: &str) -> Result<u64, LookupError> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(invalid_details)
}

fn text<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, LookupError> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(invalid_details)
}

fn nonempty_text<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, LookupError> {
    let text = text(value, pointer)?;
    if text.is_empty() {
        return Err(invalid_details());
    }
    Ok(text)
}

fn parse_checks(value: &Value) -> Option<Vec<String>> {
    value
        .get("luthor_checks")
        .and_then(Value::as_array)
        .map(|checks| {
            checks
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
}

fn invalid_details() -> LookupError {
    error(ErrorCategory::Malformed, "invalid-pr-details", None)
}
