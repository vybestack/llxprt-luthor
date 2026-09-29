use std::collections::HashSet;
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
    #[error("direct issue read failed for {0}")]
    IssueRead(String),
    #[error("Project item {item_id} disagrees with direct issue {issue_id}")]
    Inconsistent { item_id: String, issue_id: String },
    #[error("duplicate issue identity {0}")]
    Duplicate(String),
}

pub trait ProjectReader {
    fn page(&mut self, cursor: Option<&str>) -> Result<Page<ProjectItem>, String>;
    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, String>;
}

pub fn enumerate<R: ProjectReader>(
    reader: &mut R,
) -> Result<Vec<(ProjectItem, Issue)>, ProjectError> {
    let mut cursor = None;
    let mut items_seen = HashSet::new();
    let mut issues_seen = HashSet::new();
    let mut result = Vec::new();
    loop {
        let page = reader.page(cursor.as_deref()).map_err(ProjectError::Read)?;
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
        cursor = Some(page.end_cursor.ok_or(ProjectError::MissingCursor)?);
    }
    Ok(result)
}
