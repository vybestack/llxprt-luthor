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

pub fn discover<R: ProjectReader, E: From<ProjectError>>(
    reader: &mut R,
    project_id: &str,
    matches: impl FnMut(&ProjectItem) -> Result<bool, E>,
) -> Result<DiscoveryReads, E> {
    let mut report = DiscoveryReads::default();
    report.items = enumerate_with(
        reader,
        project_id,
        matches,
        |item, error| {
            if !external_read_failure(&error) {
                return Err(ProjectError::IssueRead(error).into());
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
        |error| {
            if !external_read_failure(&error) {
                return Err(ProjectError::Read(error).into());
            }
            report.source_read_failures.push(SourceReadFailure {
                project_id: project_id.to_owned(),
                http_status: error.status,
                code: error.code,
            });
            Ok(())
        },
    )?;
    Ok(report)
}

pub(crate) fn external_read_failure(error: &ProjectReadError) -> bool {
    error.category != ReadCategory::Malformed
        && (error.status.is_some()
            || matches!(
                error.category,
                ReadCategory::Permission | ReadCategory::RateLimit | ReadCategory::NotFound
            )
            || error.code == "connection-reset")
}
