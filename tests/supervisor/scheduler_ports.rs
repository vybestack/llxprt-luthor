use super::{
    AssignmentError, AssignmentWriter, Candidate, IdCreator, Issue, Page, ProjectItem,
    ProjectReadError, ProjectReader, StateStore, SupervisorError, SupervisorLauncher,
};

#[cfg(unix)]
pub(crate) struct OtherProject(pub(crate) Candidate, pub(crate) usize);
#[cfg(unix)]
impl ProjectReader for OtherProject {
    fn page(&mut self, _: &str, _: Option<&str>) -> Result<Page<ProjectItem>, ProjectReadError> {
        let c = &self.0;
        Ok(Page {
            items: vec![ProjectItem {
                item_id: c.item_id.clone(),
                issue_node_id: c.issue_node_id.clone(),
                repository: c.repository.clone(),
                tracker_repo_id: c.tracker_repo_id.clone(),
                issue_number: c.issue_number,
                fields: vec![],
                unsupported_fields: vec![],
            }],
            has_next_page: false,
            end_cursor: None,
        })
    }
    fn issue(&mut self, _: &ProjectItem) -> Result<Issue, ProjectReadError> {
        self.1 += 1;
        let c = &self.0;
        Ok(Issue {
            node_id: c.issue_node_id.clone(),
            repository: c.repository.clone(),
            tracker_repo_id: c.tracker_repo_id.clone(),
            number: c.issue_number,
            url: c.issue_url.clone(),
            state: "open".into(),
            assignees: if self.1 == 1 {
                vec![]
            } else {
                vec!["operator".into()]
            },
            labels: vec!["ready".into()],
            milestone: None,
            milestone_id: None,
            observed_at_unix_secs: 1,
        })
    }
}
#[cfg(unix)]
pub(crate) struct OtherWriter;
#[cfg(unix)]
impl AssignmentWriter for OtherWriter {
    fn assign(&mut self, _: &str, _: u64, _: &str) -> Result<(), AssignmentError> {
        Ok(())
    }
}
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct OtherLauncher(pub(crate) usize);
#[cfg(unix)]
impl SupervisorLauncher for OtherLauncher {
    fn launch(
        &mut self,
        _: &mut StateStore,
        _: &luthor::supervisor::LaunchPlan,
        _: &luthor::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        self.0 += 1;
        Ok(())
    }
}
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct OtherIds(pub(crate) usize);
#[cfg(unix)]
impl IdCreator for OtherIds {
    fn create(&mut self) -> Result<String, std::io::Error> {
        self.0 += 1;
        Ok(self.0.to_string())
    }
}
