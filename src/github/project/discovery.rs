//! Discovery is partial only for external read failures, never invalid evidence.
use super::{
    enumeration::enumerate_with,
    types::{Issue, ProjectError, ProjectItem, ProjectReadError, ProjectReader, ReadCategory},
};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateReadFailure {
    pub project_id: String,
    pub item: String,
    pub issue: String,
    pub http_status: Option<u16>,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceReadFailure {
    pub project_id: String,
    pub http_status: Option<u16>,
    pub code: String,
}

#[derive(Debug, Default)]
pub struct DiscoveryReads {
    pub items: Vec<(ProjectItem, Issue)>,
    pub candidate_read_failures: Vec<CandidateReadFailure>,
    pub source_read_failures: Vec<SourceReadFailure>,
}

pub fn discover<R: ProjectReader>(
    reader: &mut R,
    project_id: &str,
    repositories: &[String],
    target: Option<(&str, u64)>,
) -> Result<DiscoveryReads, ProjectError> {
    let mut report = DiscoveryReads::default();
    let result = enumerate_with(
        reader,
        project_id,
        |item| {
            repositories.contains(&item.repository)
                && target.is_none_or(|(repo, number)| {
                    item.repository == repo && item.issue_number == number
                })
        },
        |item, error| {
            if !external_read_failure(&error) {
                return Err(ProjectError::IssueRead(error));
            }
            report.candidate_read_failures.push(CandidateReadFailure {
                project_id: project_id.to_owned(),
                item: item.item_id.clone(),
                issue: item.issue_node_id.clone(),
                http_status: error.status,
                code: error.code,
            });
            Ok(())
        },
    );
    match result {
        Ok(items) => report.items = items,
        Err(ProjectError::Read(error)) if external_read_failure(&error) => {
            // Discard even earlier pages: an incomplete source cannot authorize dispatch.
            report.source_read_failures.push(SourceReadFailure {
                project_id: project_id.to_owned(),
                http_status: error.status,
                code: error.code,
            });
        }
        Err(error) => return Err(error),
    }
    Ok(report)
}

fn external_read_failure(error: &ProjectReadError) -> bool {
    error.category != ReadCategory::Malformed
        && (error.status.is_some()
            || matches!(
                error.category,
                ReadCategory::Permission | ReadCategory::RateLimit | ReadCategory::NotFound
            )
            || error.code == "connection-reset")
}
