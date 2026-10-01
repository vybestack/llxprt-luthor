use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectItem {
    pub item_id: String,
    pub issue_node_id: String,
    pub repository: String,
    pub tracker_repo_id: String,
    pub issue_number: u64,
    pub fields: Vec<(String, String)>,
    pub unsupported_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub node_id: String,
    pub repository: String,
    pub tracker_repo_id: String,
    pub number: u64,
    pub url: String,
    pub state: String,
    pub assignees: Vec<String>,
    pub labels: Vec<String>,
    pub milestone: Option<String>,
    pub milestone_id: Option<String>,
    pub observed_at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOperation {
    ProjectPage,
    DirectIssue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadCategory {
    Permission,
    RateLimit,
    NotFound,
    Malformed,
    Transport,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectReadError {
    pub operation: ReadOperation,
    pub project_id: Option<String>,
    pub item_id: Option<String>,
    pub issue_id: Option<String>,
    pub category: ReadCategory,
    pub status: Option<u16>,
    pub code: String,
}

impl From<String> for ProjectReadError {
    fn from(code: String) -> Self {
        let safe_code = if !code.is_empty()
            && code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            code
        } else {
            "malformed-response".to_owned()
        };
        Self {
            operation: ReadOperation::ProjectPage,
            project_id: None,
            item_id: None,
            issue_id: None,
            category: ReadCategory::Malformed,
            status: None,
            code: safe_code,
        }
    }
}

impl std::fmt::Display for ProjectReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} {:?} ({})",
            self.operation, self.category, self.code
        )?;
        if let Some(status) = self.status {
            write!(f, " HTTP {status}")?;
        }
        if let Some(id) = &self.project_id {
            write!(f, " project={id}")?;
        }
        if let Some(id) = &self.item_id {
            write!(f, " item={id}")?;
        }
        if let Some(id) = &self.issue_id {
            write!(f, " issue={id}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProjectError {
    #[error("Project enumeration failed: {0}")]
    Read(ProjectReadError),
    #[error("Project pagination returned no cursor")]
    MissingCursor,
    #[error("Project pagination repeated cursor")]
    RepeatedCursor,
    #[error("direct issue read failed: {0}")]
    IssueRead(ProjectReadError),
    #[error("Project item {item_id} disagrees with direct issue {issue_id}")]
    Inconsistent { item_id: String, issue_id: String },
    #[error("duplicate issue identity {0}")]
    Duplicate(String),
    #[error("Project item {0} has an empty issue identity")]
    InvalidItem(String),
}

pub trait ProjectReader {
    fn page(
        &mut self,
        project_id: &str,
        cursor: Option<&str>,
    ) -> Result<Page<ProjectItem>, ProjectReadError>;
    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError>;
}

pub struct GhProjectReader {
    pub executable: PathBuf,
    repository_ids: HashMap<String, String>,
}

impl GhProjectReader {
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            repository_ids: HashMap::new(),
        }
    }

    fn api(&self, args: &[&str]) -> Result<Value, ProjectReadError> {
        let output = Command::new(&self.executable)
            .args(args)
            .output()
            .map_err(|_| ProjectReadError {
                operation: ReadOperation::ProjectPage,
                project_id: None,
                item_id: None,
                issue_id: None,
                category: ReadCategory::Transport,
                status: None,
                code: "transport-error".into(),
            })?;
        let parsed = serde_json::from_slice::<Value>(&output.stdout);
        if !output.status.success() {
            let json_error = parsed.as_ref().ok();
            let status = json_error
                .and_then(|value| value.get("status"))
                .and_then(status_value);
            let stderr = &output.stderr[..output.stderr.len().min(4096)];
            let stderr_text = String::from_utf8_lossy(stderr);
            let status = status.or_else(|| stderr_status(&stderr_text));
            let message = json_error
                .and_then(|value| value.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let category = classify_status(status, message, &stderr_text);
            return Err(ProjectReadError {
                operation: ReadOperation::ProjectPage,
                project_id: None,
                item_id: None,
                issue_id: None,
                category,
                status,
                code: "command-failed".into(),
            });
        }
        let value = parsed.map_err(|_| ProjectReadError {
            operation: ReadOperation::ProjectPage,
            project_id: None,
            item_id: None,
            issue_id: None,
            category: ReadCategory::Malformed,
            status: None,
            code: "invalid-json".into(),
        })?;
        if value.get("message").is_some() && value.get("documentation_url").is_some() {
            return Err("api-error".to_owned().into());
        }
        Ok(value)
    }
}

impl ProjectReader for GhProjectReader {
    fn page(
        &mut self,
        project_id: &str,
        cursor: Option<&str>,
    ) -> Result<Page<ProjectItem>, ProjectReadError> {
        const QUERY: &str = "query($projectId: ID!, $cursor: String) { node(id: $projectId) { ... on ProjectV2 { items(first: 100, after: $cursor) { nodes { id content { __typename ... on Issue { id number repository { id nameWithOwner } } } fieldValues(first: 100) { nodes { __typename ... on ProjectV2ItemFieldSingleSelectValue { name field { ... on ProjectV2SingleSelectField { name } ... on ProjectV2Field { name } } } ... on ProjectV2ItemFieldTextValue { text field { ... on ProjectV2SingleSelectField { name } ... on ProjectV2Field { name } } } ... on ProjectV2ItemFieldIterationValue { field { ... on ProjectV2IterationField { name } } } ... on ProjectV2ItemFieldDateValue { field { ... on ProjectV2Field { name } } } } pageInfo { hasNextPage endCursor } } } pageInfo { hasNextPage endCursor } } } } }";
        let query_arg = format!("query={QUERY}");
        let project_id_arg = format!("projectId={project_id}");
        let cursor_arg = cursor.map(|cursor| format!("cursor={cursor}"));
        let mut args = vec!["api", "graphql", "-f", &query_arg, "-F", &project_id_arg];
        if let Some(cursor_arg) = &cursor_arg {
            args.extend(["-F", cursor_arg]);
        }
        let value = self.api(&args).map_err(|mut error| {
            error.operation = ReadOperation::ProjectPage;
            error.project_id = Some(project_id.to_owned());
            error
        })?;
        if let Some(errors) = value.get("errors").and_then(Value::as_array) {
            let category = classify_graphql_errors(errors);
            return Err(ProjectReadError {
                operation: ReadOperation::ProjectPage,
                project_id: Some(project_id.to_owned()),
                item_id: None,
                issue_id: None,
                category,
                status: None,
                code: "graphql-error".to_owned(),
            });
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
            _ => return Err("invalid-page-info".to_owned().into()),
        };
        if has_next_page && end_cursor.as_deref().is_none_or(str::is_empty) {
            return Err("missing-page-cursor".to_owned().into());
        }

        let mut items = Vec::new();
        for node in nodes {
            let item_id = required_string(node, "id", "invalid-project-item").map_err(|_| {
                ProjectReadError {
                    operation: ReadOperation::ProjectPage,
                    project_id: Some(project_id.to_owned()),
                    item_id: None,
                    issue_id: None,
                    category: ReadCategory::Malformed,
                    status: None,
                    code: "invalid-project-item".to_owned(),
                }
            })?;
            let content = node
                .get("content")
                .ok_or_else(|| item_error(project_id, &item_id, "missing-project-item-content"))?;
            if content.is_null() {
                return Err(item_error(
                    project_id,
                    &item_id,
                    "null-project-item-content",
                ));
            }
            let typename = content
                .get("__typename")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    item_error(project_id, &item_id, "missing-project-item-content-type")
                })?;
            if typename == "PullRequest" {
                continue;
            }
            if typename != "Issue" {
                return Err(item_error(
                    project_id,
                    &item_id,
                    "unsupported-project-item-content-type",
                ));
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
            let tracker_repo_id = required_string(
                content
                    .get("repository")
                    .ok_or_else(|| "invalid-project-issue".to_owned())?,
                "id",
                "invalid-project-issue",
            )?;
            let field_values = node
                .get("fieldValues")
                .ok_or_else(|| "missing-project-field-values".to_owned())?;
            if field_values
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                != Some(false)
            {
                return Err("incomplete-project-field-values".to_owned().into());
            }
            let field_nodes = field_values
                .get("nodes")
                .and_then(Value::as_array)
                .ok_or_else(|| "invalid-project-field-values".to_owned())?;
            let mut fields = Vec::new();
            let mut unsupported_fields = Vec::new();
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
                    Some("ProjectV2ItemFieldDateValue") => {
                        let name = required_string(
                            field
                                .get("field")
                                .ok_or_else(|| "invalid-project-field".to_owned())?,
                            "name",
                            "invalid-project-field",
                        )?;
                        unsupported_fields.push(name);
                    }
                    Some("ProjectV2ItemFieldIterationValue") => {
                        unsupported_fields.push(required_string(
                            field
                                .get("field")
                                .ok_or_else(|| "invalid-project-field".to_owned())?,
                            "name",
                            "invalid-project-field",
                        )?)
                    }
                    Some(
                        "ProjectV2ItemFieldNumberValue"
                        | "ProjectV2ItemFieldUserValue"
                        | "ProjectV2ItemFieldRepositoryValue"
                        | "ProjectV2ItemFieldLabelValue"
                        | "ProjectV2ItemFieldMilestoneValue"
                        | "ProjectV2ItemFieldPullRequestValue",
                    ) => {}
                    Some(_) | None => {
                        return Err("unsupported-project-field-value-type".to_owned().into());
                    }
                }
            }
            items.push(ProjectItem {
                item_id,
                issue_node_id,
                repository: repository.to_owned(),
                tracker_repo_id,
                issue_number: number,
                fields,
                unsupported_fields,
            });
        }
        Ok(Page {
            items,
            has_next_page,
            end_cursor,
        })
    }

    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        let result = (|| {
            let repository_id = if let Some(id) = self.repository_ids.get(&item.repository) {
                id.clone()
            } else {
                let path = format!("repos/{}", item.repository);
                let repository = self.api(&["api", &path])?;
                let id = required_string(&repository, "node_id", "invalid-repository")?;
                self.repository_ids
                    .insert(item.repository.clone(), id.clone());
                id
            };
            if repository_id != item.tracker_repo_id {
                return Err("identity-mismatch".to_owned().into());
            }
            let path = format!(
                "repos/{}/issues/{}?per_page=100",
                item.repository, item.issue_number
            );
            let value = self.api(&["api", &path])?;
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
            let node_id = required_string(&value, "node_id", "invalid-issue")?;
            let repository_url = value
                .pointer("/repository_url")
                .and_then(Value::as_str)
                .ok_or_else(|| "identity-mismatch".to_owned())?;
            let expected_repository_url =
                format!("https://api.github.com/repos/{}", item.repository);
            if repository_url != expected_repository_url {
                return Err("identity-mismatch".to_owned().into());
            }
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
                return Err("issue-number-mismatch".to_owned().into());
            }
            let url = required_string(&value, "html_url", "invalid-issue")?;
            let expected_url = format!(
                "https://github.com/{}/issues/{}",
                item.repository, item.issue_number
            );
            if url != expected_url {
                return Err("issue-url-mismatch".to_owned().into());
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
                return Err("issue-subcollection-at-cap".to_owned().into());
            }
            let assignees = assignees
                .iter()
                .map(|assignee| required_string(assignee, "login", "invalid-issue-assignees"))
                .collect::<Result<Vec<_>, _>>()?;
            let labels = labels
                .iter()
                .map(|label| required_string(label, "name", "invalid-issue-labels"))
                .collect::<Result<Vec<_>, _>>()?;
            let observed_at_unix_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
                .as_secs();
            let (milestone, milestone_id) = match value.get("milestone") {
                Some(Value::Null) => (None, None),
                Some(milestone) => (
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
                ),
                None => return Err("invalid-issue-milestone".to_owned().into()),
            };
            Ok(Issue {
                node_id,
                repository: repository.to_owned(),
                tracker_repo_id: repository_id,
                number,
                url,
                state,
                assignees,
                labels,
                milestone,
                milestone_id,
                observed_at_unix_secs,
            })
        })();
        result.map_err(|error| with_issue_context(error, item))
    }
}

fn status_value(value: &Value) -> Option<u16> {
    value
        .as_str()
        .and_then(|status| status.parse().ok())
        .or_else(|| value.as_u64().and_then(|status| u16::try_from(status).ok()))
}

fn stderr_status(stderr: &str) -> Option<u16> {
    let words = stderr.split_whitespace().collect::<Vec<_>>();
    words.windows(2).find_map(|pair| {
        if pair[0] == "HTTP" {
            pair[1]
                .trim_matches(|ch: char| !ch.is_ascii_digit())
                .parse()
                .ok()
        } else {
            None
        }
    })
}

fn classify_status(status: Option<u16>, json_message: &str, stderr: &str) -> ReadCategory {
    let rate_limited = json_message.to_ascii_lowercase().contains("rate limit")
        || stderr.to_ascii_lowercase().contains("rate limit");
    match status {
        Some(401) => ReadCategory::Permission,
        Some(403) if rate_limited => ReadCategory::RateLimit,
        Some(403) => ReadCategory::Permission,
        Some(404) => ReadCategory::NotFound,
        Some(429) => ReadCategory::RateLimit,
        _ => ReadCategory::Unknown,
    }
}

fn classify_graphql_errors(errors: &[Value]) -> ReadCategory {
    let mut category = ReadCategory::Unknown;
    for error in errors {
        let code = error
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| error.pointer("/extensions/code").and_then(Value::as_str))
            .unwrap_or("")
            .to_ascii_uppercase();
        let candidate = match code.as_str() {
            "RATE_LIMITED" | "RATE_LIMIT" => ReadCategory::RateLimit,
            "FORBIDDEN" | "UNAUTHORIZED" => ReadCategory::Permission,
            "NOT_FOUND" => ReadCategory::NotFound,
            _ => {
                let message = error.get("message").and_then(Value::as_str).unwrap_or("");
                if message.len() <= 256 && message.to_ascii_lowercase().contains("rate limit") {
                    ReadCategory::RateLimit
                } else {
                    ReadCategory::Unknown
                }
            }
        };
        match candidate {
            ReadCategory::RateLimit => return candidate,
            ReadCategory::Permission => category = candidate,
            ReadCategory::NotFound if category == ReadCategory::Unknown => category = candidate,
            _ => {}
        }
    }
    category
}

fn with_issue_context(mut error: ProjectReadError, item: &ProjectItem) -> ProjectReadError {
    error.operation = ReadOperation::DirectIssue;
    error.item_id = Some(item.item_id.clone());
    error.issue_id = Some(item.issue_node_id.clone());
    error
}

fn item_error(project_id: &str, item_id: &str, code: &str) -> ProjectReadError {
    ProjectReadError {
        operation: ReadOperation::ProjectPage,
        project_id: Some(project_id.to_owned()),
        item_id: Some(item_id.to_owned()),
        issue_id: None,
        category: ReadCategory::Malformed,
        status: None,
        code: code.to_owned(),
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
                return Err(ProjectError::InvalidItem(item.item_id));
            }
            let issue = reader.issue(&item).map_err(ProjectError::IssueRead)?;
            if issue.node_id != item.issue_node_id
                || issue.repository != item.repository
                || issue.tracker_repo_id != item.tracker_repo_id
                || issue.number != item.issue_number
                || issue.url
                    != format!(
                        "https://github.com/{}/issues/{}",
                        item.repository, item.issue_number
                    )
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

/// Enumerates the entire project, reading issue details only for matching content references.
pub fn enumerate_target<R: ProjectReader>(
    reader: &mut R,
    project_id: &str,
    repository: &str,
    number: u64,
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
                return Err(ProjectError::InvalidItem(item.item_id));
            }
            if !issues_seen.insert(item.issue_node_id.clone()) {
                return Err(ProjectError::Duplicate(item.issue_node_id));
            }
            if item.repository != repository || item.issue_number != number {
                continue;
            }
            let issue = reader.issue(&item).map_err(ProjectError::IssueRead)?;
            if issue.node_id != item.issue_node_id
                || issue.repository != item.repository
                || issue.tracker_repo_id != item.tracker_repo_id
                || issue.number != item.issue_number
                || issue.url
                    != format!(
                        "https://github.com/{}/issues/{}",
                        item.repository, item.issue_number
                    )
            {
                return Err(ProjectError::Inconsistent {
                    item_id: item.item_id,
                    issue_id: item.issue_node_id,
                });
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
