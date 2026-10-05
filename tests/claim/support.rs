use luthor::state::task_records;
pub(super) use luthor::{
    claim::{AssignmentError, ClaimError, claim},
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    eligibility::Candidate,
    github::{
        project::{ProjectItem, ProjectReadError, ReadCategory, ReadOperation},
        pull_request::{
            ErrorCategory, GhPullRequestReader, LookupError, LookupResult, PullRequestReader,
            lookup,
        },
    },
    state::StateStore,
};
pub(super) use rusqlite::Connection;
pub(super) use serde_json::{Value, json};
pub(super) use std::{cell::RefCell, collections::HashMap, rc::Rc};

pub(super) const REPO: &str = "code/project";
pub(super) const ISSUE: &str = "https://github.com/tracker/issues/7";

#[derive(Default)]
pub(super) struct Fake {
    pub(super) pages: HashMap<u32, Vec<Value>>,
    pub(super) details: HashMap<u64, Value>,
    pub(super) failed_page: Option<u32>,
}

impl PullRequestReader for Fake {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok("agent".into())
    }
    fn page(&mut self, _: &str, page: u32) -> Result<Vec<Value>, LookupError> {
        if self.failed_page == Some(page) {
            return Err(LookupError {
                category: ErrorCategory::Permission,
                code: "command-failed",
                status: Some(403),
            });
        }
        Ok(self.pages.get(&page).cloned().unwrap_or_default())
    }
    fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError> {
        match name {
            "org/code" => Ok(10),
            "fork/project" => Ok(20),
            _ => Err(LookupError {
                category: ErrorCategory::Malformed,
                code: "unexpected-repository",
                status: None,
            }),
        }
    }
    fn detail(&mut self, _: &str, number: u64) -> Result<Value, LookupError> {
        self.details.get(&number).cloned().ok_or(LookupError {
            category: ErrorCategory::Malformed,
            code: "invalid-json",
            status: None,
        })
    }
}

pub(super) fn item(number: u64, body: &str) -> Value {
    json!({"number":number,"body":body})
}
pub(super) fn detail(number: u64, body: &str) -> Value {
    json!({"id":number+100,"number":number,"html_url":format!("https://github.com/{REPO}/pull/{number}"),"body":body,"state":"open","draft":true,"created_at":"2026-01-01T00:00:00Z","base":{"ref":"main","repo":{"id":10,"full_name":REPO}},"head":{"sha":format!("sha-{number}"),"ref":format!("agent/{number}"),"repo":{"id":20,"full_name":"fork/project"}},"user":{"login":"agent"},"mergeable_state":"dirty"})
}
pub(super) fn matching(number: u64) -> (Value, Value) {
    let body = format!("body\nTracker-Issue: {ISSUE}\n");
    (item(number, &body), detail(number, &body))
}

pub(super) fn claim_fixture() -> (
    tempfile::TempDir,
    Candidate,
    ClaimProjects,
    ClaimWriter,
    StateStore,
) {
    claim_fixture_with_milestone(None)
}

pub(super) fn claim_fixture_with_milestone(
    milestone: Option<&str>,
) -> (
    tempfile::TempDir,
    Candidate,
    ClaimProjects,
    ClaimWriter,
    StateStore,
) {
    let dir = tempfile::tempdir().unwrap();
    let config = fixture_config(dir.path());
    let candidate = fixture_candidate(&config, milestone);
    let shared_assignees = Rc::new(RefCell::new(Vec::<String>::new()));
    let projects = ClaimProjects {
        assignees: Rc::clone(&shared_assignees),
        issue_reads: 0,
        failed_issue_read: None,
        later_page: None,
        milestone: candidate.milestone_title.clone(),
        milestone_id: candidate.milestone_id.clone(),
        item: ProjectItem {
            item_id: "item-N7".into(),
            issue_node_id: "N7".into(),
            repository: "org/tracker".into(),
            tracker_repo_id: "R1".into(),
            issue_number: 7,
            fields: vec![],
            unsupported_fields: vec![],
        },
    };
    let writer = ClaimWriter {
        assignees: Rc::clone(&shared_assignees),
        calls: 0,
        fail: false,
    };
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    task_records::create_task(&mut store, "task", &candidate, "rev", &config).unwrap();
    (dir, candidate, projects, writer, store)
}

pub(super) fn assert_claim_state(
    dir: &tempfile::TempDir,
    phase: &str,
    intent: bool,
    verified: bool,
) {
    let db = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    let actual_phase: String = db
        .query_row("SELECT state FROM tasks WHERE id='task'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(actual_phase, phase);
    let intents: Vec<(String, String, Value)> = db
        .prepare("SELECT id, kind, detail FROM intents WHERE task_id='task' ORDER BY sequence")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
        })
        .unwrap()
        .map(|row| {
            let (id, kind, detail) = row.unwrap();
            (id, kind, serde_json::from_str(&detail).unwrap())
        })
        .collect();
    assert_eq!(
        intents,
        if intent {
            vec![(
                "claim-task".into(),
                "claim_assignment".into(),
                json!({"principal":"bot","repository":"org/tracker","number":7}),
            )]
        } else {
            vec![]
        }
    );
    let evidence: Vec<(String, String)> = db
        .prepare("SELECT kind, payload FROM evidence WHERE task_id='task' ORDER BY sequence")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        evidence
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        if verified {
            vec!["selection", "claim_verified"]
        } else {
            vec!["selection"]
        }
    );
    if verified {
        assert_eq!(evidence[1].1, "bot");
    }
}

pub(super) struct ClaimProjects {
    pub(super) assignees: Rc<RefCell<Vec<String>>>,
    pub(super) issue_reads: usize,
    pub(super) failed_issue_read: Option<usize>,
    pub(super) later_page: Option<Result<Vec<ProjectItem>, ProjectReadError>>,
    pub(super) milestone: Option<String>,
    pub(super) milestone_id: Option<String>,
    pub(super) item: ProjectItem,
}
impl luthor::github::project::ProjectReader for ClaimProjects {
    fn page(
        &mut self,
        _: &str,
        cursor: Option<&str>,
    ) -> Result<
        luthor::github::project::Page<luthor::github::project::ProjectItem>,
        luthor::github::project::ProjectReadError,
    > {
        if cursor.is_some() {
            return match self.later_page.take().expect("later page configured") {
                Ok(items) => Ok(luthor::github::project::Page {
                    items,
                    has_next_page: false,
                    end_cursor: None,
                }),
                Err(error) => Err(error),
            };
        }
        Ok(luthor::github::project::Page {
            items: vec![self.item.clone()],
            has_next_page: self.later_page.is_some(),
            end_cursor: self.later_page.as_ref().map(|_| "next".into()),
        })
    }
    fn issue(
        &mut self,
        _: &luthor::github::project::ProjectItem,
    ) -> Result<luthor::github::project::Issue, luthor::github::project::ProjectReadError> {
        self.issue_reads += 1;
        if self.failed_issue_read == Some(self.issue_reads) {
            return Err(ProjectReadError {
                operation: ReadOperation::DirectIssue,
                project_id: None,
                item_id: Some(self.item.item_id.clone()),
                issue_id: Some(self.item.issue_node_id.clone()),
                category: ReadCategory::Transport,
                status: None,
                code: "read-failed".into(),
            });
        }
        Ok(luthor::github::project::Issue {
            node_id: "N7".into(),
            repository: "org/tracker".into(),
            tracker_repo_id: "R1".into(),
            number: 7,
            url: "https://github.com/org/tracker/issues/7".into(),
            state: "open".into(),
            assignees: self.assignees.borrow().clone(),
            labels: vec!["ready".into()],
            milestone: self.milestone.clone(),
            milestone_id: self.milestone_id.clone(),
            observed_at_unix_secs: 1_700_000_000,
        })
    }
}

pub(super) struct EmptyPullRequests;
impl luthor::github::pull_request::PullRequestReader for EmptyPullRequests {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok("acoliver".into())
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<Value>, LookupError> {
        Ok(vec![])
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<Value, LookupError> {
        unreachable!()
    }
    fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError> {
        match name {
            "org/code" => Ok(10),
            "fork/project" => Ok(20),
            _ => Err(LookupError {
                category: ErrorCategory::Malformed,
                code: "unexpected-repository",
                status: None,
            }),
        }
    }
}

pub(super) struct ClaimWriter {
    pub(super) assignees: Rc<RefCell<Vec<String>>>,
    pub(super) calls: usize,
    pub(super) fail: bool,
}
impl luthor::claim::AssignmentWriter for ClaimWriter {
    fn assign(
        &mut self,
        _: &str,
        _: u64,
        principal: &str,
    ) -> Result<(), luthor::claim::AssignmentError> {
        self.calls += 1;
        self.assignees.borrow_mut().push(principal.into());
        if self.fail {
            return Err(AssignmentError { ambiguous: true });
        }
        Ok(())
    }
}

fn fixture_config(root: &std::path::Path) -> Config {
    Config {
        state_root: root.into(),
        worktree_root: root.join("worktrees"),
        capacity: 1,
        assignment_login: "bot".into(),
        sources: vec![Source {
            project_id: "project-1".into(),
            repositories: vec!["org/tracker".into()],
            ready_marker: Marker::Label {
                name: "ready".into(),
            },
            milestone: None,
        }],
        mappings: vec![Mapping {
            tracker_repository: "org/tracker".into(),
            code_repository: "org/code".into(),
            checkout: "/checkout".into(),
            base_branch: "main".into(),
            push_remote: "origin".into(),
            allowed_pr_head_repository: "bot/fork".into(),
            allowed_pr_author: "bot".into(),
        }],
        initial: CommandTemplate {
            executable: "/bin/worker".into(),
            args: vec!["start".into(), "{task.id}".into()],
        },
        resume: CommandTemplate {
            executable: "/bin/worker".into(),
            args: vec!["resume".into(), "{task.id}".into()],
        },
    }
}

fn fixture_candidate(config: &Config, milestone: Option<&str>) -> Candidate {
    Candidate {
        project_id: "project-1".into(),
        item_id: "item-N7".into(),
        repository: "org/tracker".into(),
        issue_node_id: "N7".into(),
        issue_number: 7,
        issue_url: "https://github.com/org/tracker/issues/7".into(),
        tracker_repo_id: "R1".into(),
        milestone_id: milestone.map(|_| "MILESTONE1".into()),
        milestone_title: milestone.map(str::to_string),
        observed_at_unix_secs: 1_700_000_000,
        observed_state: "open".into(),
        observed_assignees: vec![],
        observed_labels: vec!["ready".into()],
        observed_project_fields: vec![],
        marker: Marker::Label {
            name: "ready".into(),
        },
        mapping: config.mappings[0].clone(),
        source: config.sources[0].clone(),
    }
}
