use luthor::{
    claim::{AssignmentError, AssignmentWriter},
    coordinator::{IdCreator, SupervisorLauncher},
    eligibility::Candidate,
    github::{
        project::{Issue, Page, ProjectItem, ProjectReadError, ProjectReader},
        pull_request::{ErrorCategory, LookupError, PullRequestReader},
    },
    state::StateStore,
    supervisor::{LaunchPlan, SupervisorError},
};
use serde_json::{Value, json};

pub(crate) struct FakeGithub {
    pub(crate) candidate: Candidate,
    pub(crate) reads: usize,
    pub(crate) change_on: Option<usize>,
    pub(crate) assigned: bool,
    pub(crate) prs: FakePr,
}
impl FakeGithub {
    pub(crate) fn new(candidate: &Candidate) -> Self {
        Self {
            candidate: candidate.clone(),
            reads: 0,
            change_on: None,
            assigned: false,
            prs: FakePr::default(),
        }
    }
}
impl ProjectReader for FakeGithub {
    fn page(&mut self, _: &str, _: Option<&str>) -> Result<Page<ProjectItem>, ProjectReadError> {
        let c = &self.candidate;
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
        self.reads += 1;
        let c = &self.candidate;
        let assignees = if self.change_on == Some(self.reads) {
            vec!["other".into()]
        } else if self.reads > 1 || self.assigned {
            vec!["bot".into()]
        } else {
            vec![]
        };
        Ok(Issue {
            node_id: c.issue_node_id.clone(),
            repository: c.repository.clone(),
            tracker_repo_id: c.tracker_repo_id.clone(),
            number: c.issue_number,
            url: c.issue_url.clone(),
            state: "open".into(),
            assignees,
            labels: vec!["ready".into()],
            milestone: Some("v1".into()),
            milestone_id: Some("milestone-id".into()),
            observed_at_unix_secs: 1,
        })
    }
}
#[derive(Default)]
pub(crate) struct FakePr {
    pub(crate) lookups: usize,
    pub(crate) present_on: Option<usize>,
    pub(crate) fail_on: Option<usize>,
}
impl PullRequestReader for FakePr {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok("bot".into())
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<Value>, LookupError> {
        self.lookups += 1;
        if self.fail_on == Some(self.lookups) {
            return Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            });
        }
        if self.present_on == Some(self.lookups) {
            return Ok(vec![
                json!({"number": 7, "body": "Tracker-Issue: https://github.com/org/tracker/issues/1"}),
            ]);
        }
        Ok(vec![])
    }
    fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError> {
        match name {
            "org/code" => Ok(10),
            "org/head" => Ok(20),
            _ => Err(LookupError {
                category: ErrorCategory::Malformed,
                code: "unexpected-repository",
                status: None,
            }),
        }
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<Value, LookupError> {
        Ok(
            json!({"id": 77, "number": 7, "state": "open", "html_url": "https://github.com/org/code/pull/7",
            "body": "Tracker-Issue: https://github.com/org/tracker/issues/1", "created_at": "2026-01-01T00:00:00Z",
            "base": {"repo": {"id": 10, "full_name": "org/code"}, "ref": "main"},
            "head": {"repo": {"id": 10, "full_name": "org/code"}, "ref": "branch", "sha": "abc123"},
            "user": {"login": "bot"}, "draft": false }),
        )
    }
}
#[derive(Default)]
pub(crate) struct FakeWriter {
    pub(crate) calls: usize,
}
impl AssignmentWriter for FakeWriter {
    fn assign(&mut self, _: &str, _: u64, _: &str) -> Result<(), AssignmentError> {
        self.calls += 1;
        Ok(())
    }
}
#[derive(Default)]
pub(crate) struct FakeLauncher {
    pub(crate) plans: Vec<LaunchPlan>,
    pub(crate) fail: bool,
    pub(crate) failure: Option<SupervisorError>,
}
impl SupervisorLauncher for FakeLauncher {
    fn launch(
        &mut self,
        _: &mut StateStore,
        plan: &LaunchPlan,
        _: &luthor::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        self.plans.push(plan.clone());
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        if self.fail {
            Err(SupervisorError::ExecutionUnavailable)
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
pub(crate) struct FixedIds(pub(crate) usize);
impl IdCreator for FixedIds {
    fn create(&mut self) -> Result<String, std::io::Error> {
        self.0 += 1;
        Ok(self.0.to_string())
    }
}
