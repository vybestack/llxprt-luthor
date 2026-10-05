use super::{FakeGithub, FakePr, Lane, Refusal};
use luthor::github::{
    project::{Issue, Page, ProjectItem, ProjectReadError, ProjectReader},
    pull_request::{ErrorCategory, LookupError, PullRequestReader},
};
use luthor::state::task_records;
use serde_json::{Value, json};

pub(super) struct Projects<'a>(pub &'a mut FakeGithub, pub &'static str);
impl ProjectReader for Projects<'_> {
    fn page(
        &mut self,
        project: &str,
        cursor: Option<&str>,
    ) -> Result<Page<ProjectItem>, ProjectReadError> {
        if self.1 == "source_error" {
            return Err("injected-secret-source-failure".to_owned().into());
        }
        let mut page = self.0.page(project, cursor)?;
        if self.1 == "project_identity" {
            page.items[0].tracker_repo_id = "other".into();
        }
        Ok(page)
    }
    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        if self.1 == "issue_error" {
            return Err("injected-secret-issue-failure".to_owned().into());
        }
        let mut issue = self.0.issue(item)?;
        match self.1 {
            "issue_identity" => issue.number = 99,
            "closed" => issue.state = "closed".into(),
            "not_ready" => issue.labels.clear(),
            "unassigned" => issue.assignees.clear(),
            "extra_assignee" => issue.assignees.push("other".into()),
            _ => {}
        }
        Ok(issue)
    }
}
pub(super) struct Prs<'a>(pub &'a mut FakePr, pub &'static str);
impl PullRequestReader for Prs<'_> {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        match self.1 {
            "identity_error" => Err(LookupError {
                category: ErrorCategory::Transport,
                code: "injected-secret",
                status: None,
            }),
            "identity_mismatch" => Ok("other".into()),
            _ => self.0.authenticated_identity(),
        }
    }
    fn page(&mut self, repo: &str, page: u32) -> Result<Vec<Value>, LookupError> {
        if self.1.starts_with("paged") && page == 1 {
            self.0.lookups += 1;
            return Ok((1..=100)
                .map(|number| json!({"number": number, "body": "unrelated issue"}))
                .collect());
        }
        if self.1 == "ambiguous" {
            return Ok(vec![
                json!({"number": 7, "body": "Tracker-Issue: https://github.com/org/tracker/issues/1"}),
                json!({"number": 8, "body": "Tracker-Issue: https://github.com/org/tracker/issues/1"}),
            ]);
        }
        self.0.page(repo, page)
    }
    fn repository_identity(&mut self, repo: &str) -> Result<u64, LookupError> {
        self.0.repository_identity(repo)
    }
    fn detail(&mut self, repo: &str, number: u64) -> Result<Value, LookupError> {
        let mut detail = self.0.detail(repo, number)?;
        detail["id"] = json!(number + 70);
        detail["number"] = json!(number);
        detail["html_url"] = json!(format!("https://github.com/{repo}/pull/{number}"));
        if self.1 == "stale" {
            detail["state"] = json!("closed");
        }
        Ok(detail)
    }
}

#[test]
fn continuation_fresh_claim_requires_project_identity_open_ready_exact_assignment() {
    for scenario in [
        "project_identity",
        "issue_identity",
        "closed",
        "not_ready",
        "unassigned",
        "extra_assignee",
    ] {
        let mut lane = Lane::new();
        lane.read_scenario = scenario;
        lane.held(Refusal::ClaimChanged);
    }
}

#[test]
fn continuation_pr_absence_and_authenticated_identity_must_be_current_and_unambiguous() {
    for (scenario, expected) in [
        ("open", Refusal::PrPresent),
        ("ambiguous", Refusal::PrPresent),
        ("stale", Refusal::PrUnavailable),
        ("pr_error", Refusal::PrUnavailable),
        ("identity_error", Refusal::IdentityUnavailable),
        ("identity_mismatch", Refusal::ActorMismatch),
    ] {
        let mut lane = Lane::new();
        lane.read_scenario = scenario;
        if matches!(scenario, "open" | "stale") {
            lane.github.prs.present_on = Some(1);
        }
        if scenario == "pr_error" {
            lane.github.prs.fail_on = Some(1);
        }
        lane.held(expected);
    }
}

#[test]
fn continuation_failed_source_reads_record_only_bounded_refusal() {
    for scenario in ["source_error", "issue_error"] {
        let mut lane = Lane::new();
        lane.read_scenario = scenario;
        lane.held(Refusal::SourceUnavailable);
        let reason = task_records::held_reason(&lane.f.store, "task-a")
            .unwrap()
            .unwrap();
        assert_eq!(reason, "\"source_unavailable\"");
        assert!(!reason.contains("secret"));
    }
}
