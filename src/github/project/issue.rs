use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::{
    response::required_string,
    types::{Issue, ProjectItem, ProjectReadError, ReadCategory, ReadOperation},
};

struct IssueIdentity {
    node_id: String,
    repository: String,
    number: u64,
    url: String,
}

struct IssueCollections {
    assignees: Vec<String>,
    labels: Vec<String>,
}

pub(crate) fn parse_issue(
    value: &Value,
    item: &ProjectItem,
    repository_id: String,
) -> Result<Issue, ProjectReadError> {
    if value.get("message").is_some() && value.get("documentation_url").is_some() {
        return Err(ProjectReadError {
            operation: ReadOperation::DirectIssue,
            project_id: None,
            item_id: Some(item.item_id.clone()),
            issue_id: Some(item.issue_node_id.clone()),
            category: ReadCategory::Unknown,
            status: None,
            code: "api-error".to_owned(),
        });
    }
    let identity = parse_identity(value, item)?;
    let state = required_string(value, "state", "invalid-issue")?;
    let collections = parse_collections(value)?;
    let observed_at_unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
        .as_secs();
    let (milestone, milestone_id) = parse_milestone(value)?;
    Ok(Issue {
        node_id: identity.node_id,
        repository: identity.repository,
        tracker_repo_id: repository_id,
        number: identity.number,
        url: identity.url,
        state,
        assignees: collections.assignees,
        labels: collections.labels,
        milestone,
        milestone_id,
        observed_at_unix_secs,
    })
}

fn parse_identity(value: &Value, item: &ProjectItem) -> Result<IssueIdentity, ProjectReadError> {
    let node_id = required_string(value, "node_id", "invalid-issue")?;
    let repository_url = value
        .pointer("/repository_url")
        .and_then(Value::as_str)
        .ok_or_else(|| "identity-mismatch".to_owned())?;
    if repository_url != format!("https://api.github.com/repos/{}", item.repository) {
        return Err("identity-mismatch".to_owned().into());
    }
    let repository = repository_url
        .strip_prefix("https://api.github.com/repos/")
        .filter(|name| *name == item.repository)
        .ok_or_else(|| "issue-repository-mismatch".to_owned())?;
    let number = value
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(|| "invalid-issue".to_owned())?;
    if number != item.issue_number {
        return Err("issue-number-mismatch".to_owned().into());
    }
    let url = required_string(value, "html_url", "invalid-issue")?;
    if url
        != format!(
            "https://github.com/{}/issues/{}",
            item.repository, item.issue_number
        )
    {
        return Err("issue-url-mismatch".to_owned().into());
    }
    Ok(IssueIdentity {
        node_id,
        repository: repository.to_owned(),
        number,
        url,
    })
}

fn parse_collections(value: &Value) -> Result<IssueCollections, ProjectReadError> {
    let assignees = value
        .get("assignees")
        .and_then(Value::as_array)
        .ok_or_else(|| "invalid-issue-assignees".to_owned())?;
    let labels = value
        .get("labels")
        .and_then(Value::as_array)
        .ok_or_else(|| "invalid-issue-labels".to_owned())?;
    if assignees.len() >= 100 || labels.len() >= 100 {
        return Err("issue-subcollection-at-cap".to_owned().into());
    }
    Ok(IssueCollections {
        assignees: assignees
            .iter()
            .map(|assignee| required_string(assignee, "login", "invalid-issue-assignees"))
            .collect::<Result<Vec<_>, _>>()?,
        labels: labels
            .iter()
            .map(|label| required_string(label, "name", "invalid-issue-labels"))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn parse_milestone(value: &Value) -> Result<(Option<String>, Option<String>), ProjectReadError> {
    match value.get("milestone") {
        Some(Value::Null) => Ok((None, None)),
        Some(milestone) => Ok((
            Some(required_string(
                milestone,
                "title",
                "invalid-issue-milestone",
            )?),
            Some(required_string(
                milestone,
                "node_id",
                "invalid-issue-milestone",
            )?),
        )),
        None => Err("invalid-issue-milestone".to_owned().into()),
    }
}
