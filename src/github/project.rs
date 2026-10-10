use std::collections::HashSet;

mod issue;
mod page;
mod reader;
mod response;
mod types;

pub use reader::GhProjectReader;
pub use types::{
    Issue, Page, ProjectError, ProjectItem, ProjectReadError, ProjectReader, ReadCategory,
    ReadOperation,
};

/// Enumerates all project items, reading details only from configured repositories.
pub fn enumerate<R: ProjectReader>(
    reader: &mut R,
    project_id: &str,
    repositories: &[String],
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
            if !repositories.contains(&item.repository) {
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
