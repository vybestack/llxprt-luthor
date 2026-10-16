use super::types::{Issue, ProjectError, ProjectItem, ProjectReadError, ProjectReader};
use std::collections::HashSet;

/// Enumerates all project items, reading details only from configured repositories.
pub fn enumerate<R: ProjectReader>(
    reader: &mut R,
    project_id: &str,
    repositories: &[String],
) -> Result<Vec<(ProjectItem, Issue)>, ProjectError> {
    enumerate_with(
        reader,
        project_id,
        |item| Ok(repositories.contains(&item.repository)),
        |_, error| Err(ProjectError::IssueRead(error)),
        |error| Err(ProjectError::Read(error)),
    )
}

/// Enumerates the entire project, reading issue details only for matching content references.
pub fn enumerate_target<R: ProjectReader>(
    reader: &mut R,
    project_id: &str,
    repository: &str,
    number: u64,
) -> Result<Vec<(ProjectItem, Issue)>, ProjectError> {
    enumerate_with(
        reader,
        project_id,
        |item| Ok(item.repository == repository && item.issue_number == number),
        |_, error| Err(ProjectError::IssueRead(error)),
        |error| Err(ProjectError::Read(error)),
    )
}

/// Enumerates all pages and validates identities even for filtered-out items.
/// Matching items are always confirmed by an exact-identity direct issue read.
pub fn enumerate_matching<R: ProjectReader, E: From<ProjectError>>(
    reader: &mut R,
    project_id: &str,
    matches: impl FnMut(&ProjectItem) -> Result<bool, E>,
) -> Result<Vec<(ProjectItem, Issue)>, E> {
    enumerate_with(
        reader,
        project_id,
        matches,
        |_, error| Err(ProjectError::IssueRead(error).into()),
        |error| Err(ProjectError::Read(error).into()),
    )
}

pub(crate) fn enumerate_with<R: ProjectReader, E: From<ProjectError>>(
    reader: &mut R,
    project_id: &str,
    mut matches: impl FnMut(&ProjectItem) -> Result<bool, E>,
    mut failed_issue: impl FnMut(&ProjectItem, ProjectReadError) -> Result<(), E>,
    mut failed_source: impl FnMut(ProjectReadError) -> Result<(), E>,
) -> Result<Vec<(ProjectItem, Issue)>, E> {
    let mut cursor = None;
    let mut cursors_seen = HashSet::new();
    let mut items_seen = HashSet::new();
    let mut issues_seen = HashSet::new();
    let mut result = Vec::new();
    loop {
        let page = match reader.page(project_id, cursor.as_deref()) {
            Ok(page) => page,
            Err(error) => {
                failed_source(error)?;
                // An incomplete source cannot authorize dispatch, including earlier pages.
                return Ok(Vec::new());
            }
        };
        for item in page.items {
            if !items_seen.insert(item.item_id.clone()) {
                return Err(ProjectError::Duplicate(item.item_id).into());
            }
            if item.issue_node_id.is_empty() {
                return Err(ProjectError::InvalidItem(item.item_id).into());
            }
            if !issues_seen.insert(item.issue_node_id.clone()) {
                return Err(ProjectError::Duplicate(item.issue_node_id).into());
            }
            if !matches(&item)? {
                continue;
            }
            let issue = match reader.issue(&item) {
                Ok(issue) => issue,
                Err(error) => {
                    failed_issue(&item, error)?;
                    continue;
                }
            };
            verify_issue(&item, &issue)?;
            result.push((item, issue));
        }
        if !page.has_next_page {
            break;
        }
        let next = page.end_cursor.ok_or(ProjectError::MissingCursor)?;
        if !cursors_seen.insert(next.clone()) {
            return Err(ProjectError::RepeatedCursor.into());
        }
        cursor = Some(next);
    }
    Ok(result)
}

fn verify_issue(item: &ProjectItem, issue: &Issue) -> Result<(), ProjectError> {
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
            item_id: item.item_id.clone(),
            issue_id: item.issue_node_id.clone(),
        });
    }
    Ok(())
}
