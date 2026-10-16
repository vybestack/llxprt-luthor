//! A discovery-only view: never apply eligibility filters to saved-task reads.
use super::types::{Issue, Page, ProjectItem, ProjectReadError, ProjectReader};

pub(crate) struct CandidateReader<'a, R>(pub &'a mut R);

impl<R: ProjectReader> ProjectReader for CandidateReader<'_, R> {
    fn dependency(
        &mut self,
        reference: &crate::config::IssueReference,
    ) -> Result<crate::dependencies::DependencyState, ProjectReadError> {
        self.0.dependency(reference)
    }

    fn page(
        &mut self,
        project_id: &str,
        cursor: Option<&str>,
    ) -> Result<Page<ProjectItem>, ProjectReadError> {
        self.0.candidate_page(project_id, cursor)
    }

    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        self.0.issue(item)
    }
}
