use super::{GhProjectReader, ProjectReadError, ReadOperation};
use crate::{config::IssueReference, dependencies::DependencyState};
use serde_json::Value;

impl GhProjectReader {
    pub(crate) fn read_dependency(
        &self,
        reference: &IssueReference,
    ) -> Result<DependencyState, ProjectReadError> {
        self.dependency_state(reference).map_err(|mut error| {
            error.operation = ReadOperation::Dependency;
            error.issue_id = Some(format!("{}#{}", reference.repository, reference.number));
            error
        })
    }

    fn dependency_state(
        &self,
        reference: &IssueReference,
    ) -> Result<DependencyState, ProjectReadError> {
        let path = format!("repos/{}/issues/{}", reference.repository, reference.number);
        let value = self.api(&["api", &path])?;
        let is_pr = match value.get("pull_request") {
            None => false,
            Some(Value::Object(pr))
                if pr.get("url").and_then(Value::as_str)
                    == Some(
                        format!(
                            "https://api.github.com/repos/{}/pulls/{}",
                            reference.repository, reference.number
                        )
                        .as_str(),
                    ) =>
            {
                true
            }
            _ => return Err("invalid-dependency-type".to_owned().into()),
        };
        verify_identity(&value, reference, is_pr)?;
        if value.get("repository_url").and_then(Value::as_str)
            != Some(format!("https://api.github.com/repos/{}", reference.repository).as_str())
        {
            return Err("dependency-repository-mismatch".to_owned().into());
        }
        match value.get("state").and_then(Value::as_str) {
            Some("open") => Ok(DependencyState::Open),
            Some("closed") if !is_pr => Ok(DependencyState::ClosedIssue),
            Some("closed") => self.closed_pr_state(reference, &value),
            _ => Err("invalid-dependency-state".to_owned().into()),
        }
    }

    fn closed_pr_state(
        &self,
        reference: &IssueReference,
        issue: &Value,
    ) -> Result<DependencyState, ProjectReadError> {
        let path = format!("repos/{}/pulls/{}", reference.repository, reference.number);
        let value = self.api(&["api", &path])?;
        verify_identity(&value, reference, true)?;
        if value.get("state").and_then(Value::as_str) != Some("closed")
            || value.get("node_id") != issue.get("node_id")
        {
            return Err("dependency-pr-state-changed".to_owned().into());
        }
        match value.get("merged").and_then(Value::as_bool) {
            Some(true) => Ok(DependencyState::MergedPr),
            Some(false) => Ok(DependencyState::ClosedUnmergedPr),
            None => Err("invalid-dependency-merge-state".to_owned().into()),
        }
    }
}

fn verify_identity(
    value: &Value,
    reference: &IssueReference,
    is_pr: bool,
) -> Result<(), ProjectReadError> {
    let kind = if is_pr { "pull" } else { "issues" };
    let url = format!(
        "https://github.com/{}/{kind}/{}",
        reference.repository, reference.number
    );
    if value.get("number").and_then(Value::as_u64) != Some(reference.number)
        || value.get("html_url").and_then(Value::as_str) != Some(url.as_str())
        || value
            .get("node_id")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err("dependency-identity-mismatch".to_owned().into());
    }
    Ok(())
}
