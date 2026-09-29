use std::{collections::HashSet, path::PathBuf, process::Command};

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectItem {
    pub item_id: String,
    pub issue_node_id: String,
    pub repository: String,
    pub issue_number: u64,
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub node_id: String,
    pub repository: String,
    pub number: u64,
    pub state: String,
    pub assignees: Vec<String>,
    pub labels: Vec<String>,
    pub milestone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProjectError {
    #[error("Project enumeration failed: {0}")]
    Read(String),
    #[error("Project pagination returned no cursor")]
    MissingCursor,
    #[error("Project pagination repeated cursor")]
    RepeatedCursor,
    #[error("direct issue read failed for {0}")]
    IssueRead(String),
    #[error("Project item {item_id} disagrees with direct issue {issue_id}")]
    Inconsistent { item_id: String, issue_id: String },
    #[error("duplicate issue identity {0}")]
    Duplicate(String),
}

pub trait ProjectReader {
    fn page(&mut self, project_id: &str, cursor: Option<&str>)
    -> Result<Page<ProjectItem>, String>;
    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, String>;
}

pub struct GhProjectReader {
    pub executable: PathBuf,
}

impl GhProjectReader {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    fn api(&self, args: &[&str]) -> Result<Value, String> {
        let output = Command::new(&self.executable)
            .args(args)
            .output()
            .map_err(|_| "transport-error".to_owned())?;
        if !output.status.success() {
            return Err("command-failed".to_owned());
        }
        let value: Value =
            serde_json::from_slice(&output.stdout).map_err(|_| "invalid-json".to_owned())?;
        if value.get("message").is_some() && value.get("documentation_url").is_some() {
            return Err("api-error".to_owned());
        }
        Ok(value)
    }
}

impl ProjectReader for GhProjectReader {
    fn page(
        &mut self,
        project_id: &str,
        cursor: Option<&str>,
    ) -> Result<Page<ProjectItem>, String> {
        const QUERY: &str = "query($projectId: ID!, $cursor: String) { node(id: $projectId) { ... on ProjectV2 { items(first: 100, after: $cursor) { nodes { id content { __typename ... on Issue { id number repository { nameWithOwner } } } fieldValues(first: 100) { nodes { __typename ... on ProjectV2ItemFieldSingleSelectValue { name field { ... on ProjectV2SingleSelectField { name } ... on ProjectV2Field { name } } } ... on ProjectV2ItemFieldTextValue { text field { ... on ProjectV2SingleSelectField { name } ... on ProjectV2Field { name } } } } pageInfo { hasNextPage endCursor } } } pageInfo { hasNextPage endCursor } } } } }";
        let query_arg = format!("query={QUERY}");
        let project_id_arg = format!("projectId={project_id}");
        let cursor_arg = cursor.map(|cursor| format!("cursor={cursor}"));
        let mut args = vec!["api", "graphql", "-f", &query_arg, "-F", &project_id_arg];
        if let Some(cursor_arg) = &cursor_arg {
            args.extend(["-F", cursor_arg]);
        }
        let value = self.api(&args)?;
        if value.get("errors").is_some() {
            return Err("graphql-error".to_owned());
        }
        let connection = value
            .pointer("/data/node/items")
            .ok_or_else(|| "missing-project-connection".to_owned())?;
        let nodes = connection
            .get("nodes")
            .and_then(Value::as_array)
            .ok_or_else(|| "invalid-project-items".to_owned())?;
        let page_info = connection
            .get("pageInfo")
            .ok_or_else(|| "missing-page-info".to_owned())?;
        let has_next_page = page_info
            .get("hasNextPage")
            .and_then(Value::as_bool)
            .ok_or_else(|| "invalid-page-info".to_owned())?;
        let end_cursor = match page_info.get("endCursor") {
            Some(Value::String(cursor)) => Some(cursor.clone()),
            Some(Value::Null) => None,
            _ => return Err("invalid-page-info".to_owned()),
        };
        if has_next_page && end_cursor.as_deref().is_none_or(str::is_empty) {
            return Err("missing-page-cursor".to_owned());
        }

        let mut items = Vec::new();
        for node in nodes {
            let item_id = required_string(node, "id", "invalid-project-item")?;
            let content = node
                .get("content")
                .ok_or_else(|| "missing-project-content".to_owned())?;
            if content.get("__typename").and_then(Value::as_str) != Some("Issue") {
                continue;
            }
            let issue_node_id = required_string(content, "id", "invalid-project-issue")?;
            let number = content
                .get("number")
                .and_then(Value::as_u64)
                .ok_or_else(|| "invalid-project-issue".to_owned())?;
            let repository = content
                .pointer("/repository/nameWithOwner")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "invalid-project-issue".to_owned())?;
            let field_values = node
                .get("fieldValues")
                .ok_or_else(|| "missing-project-field-values".to_owned())?;
            if field_values
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                != Some(false)
            {
                return Err("incomplete-project-field-values".to_owned());
            }
            let field_nodes = field_values
                .get("nodes")
                .and_then(Value::as_array)
                .ok_or_else(|| "invalid-project-field-values".to_owned())?;
            let mut fields = Vec::new();
            for field in field_nodes {
                match field.get("__typename").and_then(Value::as_str) {
                    Some("ProjectV2ItemFieldSingleSelectValue") => fields.push((
                        required_string(
                            field
                                .get("field")
                                .ok_or_else(|| "invalid-project-field".to_owned())?,
                            "name",
                            "invalid-project-field",
                        )?,
                        required_string(field, "name", "invalid-project-field")?,
                    )),
                    Some("ProjectV2ItemFieldTextValue") => fields.push((
                        required_string(
                            field
                                .get("field")
                                .ok_or_else(|| "invalid-project-field".to_owned())?,
                            "name",
                            "invalid-project-field",
                        )?,
                        required_string(field, "text", "invalid-project-field")?,
                    )),
                    Some(_) | None => {}
                }
            }
            items.push(ProjectItem {
                item_id,
                issue_node_id,
                repository: repository.to_owned(),
                issue_number: number,
                fields,
            });
        }
        Ok(Page {
            items,
            has_next_page,
            end_cursor,
        })
    }

    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, String> {
        let path = format!(
            "repos/{}/issues/{}?per_page=100",
            item.repository, item.issue_number
        );
        let value = self.api(&["api", &path])?;
        if value.get("message").is_some() && value.get("documentation_url").is_some() {
            return Err("api-error".to_owned());
        }
        let node_id = required_string(&value, "node_id", "invalid-issue")?;
        let repository = value
            .pointer("/repository_url")
            .and_then(Value::as_str)
            .and_then(|url| url.strip_prefix("https://api.github.com/repos/"))
            .filter(|name| *name == item.repository)
            .ok_or_else(|| "issue-repository-mismatch".to_owned())?;
        let number = value
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| "invalid-issue".to_owned())?;
        if number != item.issue_number {
            return Err("issue-number-mismatch".to_owned());
        }
        let state = required_string(&value, "state", "invalid-issue")?;
        let assignees = value
            .get("assignees")
            .and_then(Value::as_array)
            .ok_or_else(|| "invalid-issue-assignees".to_owned())?;
        let labels = value
            .get("labels")
            .and_then(Value::as_array)
            .ok_or_else(|| "invalid-issue-labels".to_owned())?;
        if assignees.len() >= 100 || labels.len() >= 100 {
            return Err("issue-subcollection-at-cap".to_owned());
        }
        let assignees = assignees
            .iter()
            .map(|assignee| required_string(assignee, "login", "invalid-issue-assignees"))
            .collect::<Result<Vec<_>, _>>()?;
        let labels = labels
            .iter()
            .map(|label| required_string(label, "name", "invalid-issue-labels"))
            .collect::<Result<Vec<_>, _>>()?;
        let milestone = match value.get("milestone") {
            Some(Value::Null) => None,
            Some(milestone) => Some(required_string(
                milestone,
                "title",
                "invalid-issue-milestone",
            )?),
            None => return Err("invalid-issue-milestone".to_owned()),
        };
        Ok(Issue {
            node_id,
            repository: repository.to_owned(),
            number,
            state,
            assignees,
            labels,
            milestone,
        })
    }
}

fn required_string(value: &Value, key: &str, category: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| category.to_owned())
}

pub fn enumerate<R: ProjectReader>(
    reader: &mut R,
    project_id: &str,
) -> Result<Vec<(ProjectItem, Issue)>, ProjectError> {
    let mut cursor = None;
    let mut cursors_seen = HashSet::new();
    let mut items_seen = HashSet::new();
    let mut issues_seen = HashSet::new();
    let mut result = Vec::new();
    loop {
        let page = reader
            .page(project_id, cursor.as_deref())
            .map_err(ProjectError::Read)?;
        for item in page.items {
            if !items_seen.insert(item.item_id.clone()) {
                return Err(ProjectError::Duplicate(item.item_id));
            }
            if item.issue_node_id.is_empty() {
                continue;
            }
            let issue = reader
                .issue(&item)
                .map_err(|_| ProjectError::IssueRead(item.issue_node_id.clone()))?;
            if issue.node_id != item.issue_node_id
                || issue.repository != item.repository
                || issue.number != item.issue_number
            {
                return Err(ProjectError::Inconsistent {
                    item_id: item.item_id,
                    issue_id: item.issue_node_id,
                });
            }
            if !issues_seen.insert(issue.node_id.clone()) {
                return Err(ProjectError::Duplicate(issue.node_id));
            }
            result.push((item, issue));
        }
        if !page.has_next_page {
            break;
        }
        let next = page.end_cursor.ok_or(ProjectError::MissingCursor)?;
        if !cursors_seen.insert(next.clone()) {
            return Err(ProjectError::RepeatedCursor);
        }
        cursor = Some(next);
    }
    Ok(result)
}
