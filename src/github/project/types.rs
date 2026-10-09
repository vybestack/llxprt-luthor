use serde::{Deserialize, Serialize};
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
