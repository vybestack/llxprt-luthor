use std::{collections::HashMap, path::PathBuf, process::Command};

use serde_json::Value;

use super::{
    issue, page,
    response::{classify_status, required_string, status_value, stderr_status, with_issue_context},
    types::{
        Issue, Page, ProjectItem, ProjectReadError, ProjectReader, ReadCategory, ReadOperation,
    },
};

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
        page::parse_page(&value, project_id)
    }

    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        self.read_issue(item)
            .map_err(|error| with_issue_context(error, item))
    }
}

impl GhProjectReader {
    fn read_issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        let repository_id = self.issue_repository_id(&item.repository)?;
        if repository_id != item.tracker_repo_id {
            return Err("identity-mismatch".to_owned().into());
        }
        let path = format!(
            "repos/{}/issues/{}?per_page=100",
            item.repository, item.issue_number
        );
        let value = self.api(&["api", &path])?;
        issue::parse_issue(&value, item, repository_id)
    }

    fn issue_repository_id(&mut self, repository: &str) -> Result<String, ProjectReadError> {
        if let Some(id) = self.repository_ids.get(repository) {
            return Ok(id.clone());
        }
        let path = format!("repos/{repository}");
        let value = self.api(&["api", &path])?;
        let id = required_string(&value, "node_id", "invalid-repository")?;
        self.repository_ids
            .insert(repository.to_owned(), id.clone());
        Ok(id)
    }
}
