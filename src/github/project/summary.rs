//! Parse complete project-query eligibility evidence without issuing REST reads.
use serde_json::Value;

use super::{
    response::required_string,
    types::{ProjectIssueSummary, ProjectReadError},
};

pub(crate) fn parse_summary(content: &Value) -> Result<ProjectIssueSummary, ProjectReadError> {
    let state = match content.get("state").and_then(Value::as_str) {
        Some("OPEN") => "open",
        Some("CLOSED") => "closed",
        _ => return Err("invalid-project-issue-state".to_owned().into()),
    };
    let milestone = match content.get("milestone") {
        Some(Value::Null) => None,
        Some(value) => Some(required_string(
            value,
            "title",
            "invalid-project-milestone",
        )?),
        None => return Err("missing-project-milestone".to_owned().into()),
    };
    Ok(ProjectIssueSummary {
        state: state.to_owned(),
        assignees: collection(content, "assignees", "login")?,
        labels: collection(content, "labels", "name")?,
        milestone,
    })
}

fn collection(content: &Value, name: &str, field: &str) -> Result<Vec<String>, ProjectReadError> {
    let connection = content
        .get(name)
        .ok_or_else(|| "missing-project-issue-collection".to_owned())?;
    if connection
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("incomplete-project-issue-collection".to_owned().into());
    }
    connection
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "invalid-project-issue-collection".to_owned())?
        .iter()
        .map(|node| {
            required_string(node, field, "invalid-project-issue-collection")
                .map_err(ProjectReadError::from)
        })
        .collect()
}
