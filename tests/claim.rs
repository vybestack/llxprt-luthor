use luthor::github::pull_request::{
    ErrorCategory, LookupError, LookupResult, PullRequestReader, lookup,
};
use serde_json::{Value, json};
use std::collections::HashMap;

const REPO: &str = "code/project";
const ISSUE: &str = "https://github.com/tracker/issues/7";

#[derive(Default)]
struct Fake {
    pages: HashMap<u32, Vec<Value>>,
    details: HashMap<u64, Value>,
    failed_page: Option<u32>,
}

impl PullRequestReader for Fake {
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
    fn detail(&mut self, _: &str, number: u64) -> Result<Value, LookupError> {
        self.details.get(&number).cloned().ok_or(LookupError {
            category: ErrorCategory::Malformed,
            code: "invalid-json",
            status: None,
        })
    }
}

fn item(number: u64, body: &str) -> Value {
    json!({"number":number,"body":body})
}
fn detail(number: u64, body: &str) -> Value {
    json!({"id":number+100,"number":number,"html_url":format!("https://github.com/{REPO}/pull/{number}"),"body":body,"state":"open","draft":true,"base":{"ref":"main","repo":{"full_name":REPO}},"head":{"ref":format!("agent/{number}"),"repo":{"full_name":"fork/project"}},"user":{"login":"agent"},"mergeable_state":"dirty"})
}
fn matching(number: u64) -> (Value, Value) {
    let body = format!("body\nTracker-Issue: {ISSUE}\n");
    (item(number, &body), detail(number, &body))
}

#[test]
fn exhausts_two_pages_before_returning_absent() {
    let mut fake = Fake::default();
    fake.pages
        .insert(1, (0..100).map(|n| item(n, "other")).collect());
    fake.pages.insert(2, vec![]);
    assert_eq!(lookup(&mut fake, REPO, ISSUE), Ok(LookupResult::Absent));
}

#[test]
fn returns_linked_open_pr_even_when_draft_or_checks_fail() {
    let mut fake = Fake::default();
    let (item, details) = matching(3);
    fake.pages.insert(1, vec![item]);
    fake.details.insert(3, details);
    let LookupResult::OpenPreexisting(evidence) = lookup(&mut fake, REPO, ISSUE).unwrap() else {
        panic!("expected preexisting PR")
    };
    assert!(evidence.draft);
    assert_eq!(evidence.head_branch, "agent/3");
    assert_eq!(evidence.author, "agent");
}

#[test]
fn reports_multiple_exact_links_as_ambiguous() {
    let mut fake = Fake::default();
    let (a, ad) = matching(1);
    let (b, bd) = matching(2);
    fake.pages.insert(1, vec![a, b]);
    fake.details.insert(1, ad);
    fake.details.insert(2, bd);
    assert!(
        matches!(lookup(&mut fake, REPO, ISSUE), Ok(LookupResult::Ambiguous(found)) if found.len() == 2)
    );
}

#[test]
fn requires_exact_issue_url_and_whole_line() {
    let mut fake = Fake::default();
    fake.pages.insert(
        1,
        vec![
            item(1, "Tracker-Issue: https://github.com/tracker/issues/70"),
            item(2, format!("prefix Tracker-Issue: {ISSUE}").as_str()),
        ],
    );
    assert_eq!(lookup(&mut fake, REPO, ISSUE), Ok(LookupResult::Absent));
}

#[test]
fn malformed_details_and_page_failures_never_become_absent() {
    let mut fake = Fake::default();
    fake.pages
        .insert(1, vec![item(1, &format!("Tracker-Issue: {ISSUE}"))]);
    fake.details.insert(1, json!({"id":101,"state":"closed"}));
    assert_eq!(
        lookup(&mut fake, REPO, ISSUE).unwrap_err().category,
        ErrorCategory::Malformed
    );
    let mut failed = Fake {
        failed_page: Some(1),
        ..Fake::default()
    };
    assert_eq!(
        lookup(&mut failed, REPO, ISSUE).unwrap_err().category,
        ErrorCategory::Permission
    );
}

#[test]
fn failed_later_page_is_not_reported_as_absent() {
    let mut fake = Fake {
        failed_page: Some(2),
        ..Fake::default()
    };
    fake.pages
        .insert(1, (0..100).map(|n| item(n, "unrelated")).collect());
    assert!(lookup(&mut fake, REPO, ISSUE).is_err());
}
#[test]
fn verified_claim_assigns_once() {
    use luthor::{
        claim::claim,
        config::{CommandTemplate, Config, Mapping, Marker, Source},
        eligibility::Candidate,
        github::project::ProjectItem,
        state::StateStore,
    };
    use rusqlite::Connection;
    use std::{cell::RefCell, rc::Rc};

    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        state_root: dir.path().into(),
        worktree_root: dir.path().join("worktrees"),
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
    };
    let candidate = Candidate {
        project_id: "project-1".into(),
        item_id: "item-N7".into(),
        repository: "org/tracker".into(),
        issue_node_id: "N7".into(),
        issue_number: 7,
        issue_url: "https://github.com/org/tracker/issues/7".into(),
        tracker_repo_id: "R1".into(),
        milestone_id: None,
        milestone_title: None,
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
    };
    let shared_assignees = Rc::new(RefCell::new(Vec::<String>::new()));
    let mut projects = ClaimProjects {
        assignees: Rc::clone(&shared_assignees),
        issue_reads: 0,
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
    let mut prs = EmptyPullRequests;
    let mut writer = ClaimWriter {
        assignees: Rc::clone(&shared_assignees),
        calls: 0,
    };
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task("task", &candidate, "rev", &config)
        .unwrap();

    claim(
        &mut store,
        "task",
        &candidate,
        "bot",
        &mut projects,
        &mut prs,
        &mut writer,
    )
    .unwrap();

    assert_eq!(writer.calls, 1);
    assert_eq!(*shared_assignees.borrow(), vec!["bot"]);
    assert_eq!(projects.issue_reads, 2);
    drop(store);
    let db = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    let phase: String = db
        .query_row("SELECT state FROM tasks WHERE id='task'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(phase, "claimed");
}

struct ClaimProjects {
    assignees: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    issue_reads: usize,
    item: luthor::github::project::ProjectItem,
}
impl luthor::github::project::ProjectReader for ClaimProjects {
    fn page(
        &mut self,
        _: &str,
        _: Option<&str>,
    ) -> Result<
        luthor::github::project::Page<luthor::github::project::ProjectItem>,
        luthor::github::project::ProjectReadError,
    > {
        Ok(luthor::github::project::Page {
            items: vec![self.item.clone()],
            has_next_page: false,
            end_cursor: None,
        })
    }
    fn issue(
        &mut self,
        _: &luthor::github::project::ProjectItem,
    ) -> Result<luthor::github::project::Issue, luthor::github::project::ProjectReadError> {
        self.issue_reads += 1;
        Ok(luthor::github::project::Issue {
            node_id: "N7".into(),
            repository: "org/tracker".into(),
            tracker_repo_id: "R1".into(),
            number: 7,
            url: "https://github.com/org/tracker/issues/7".into(),
            state: "open".into(),
            assignees: self.assignees.borrow().clone(),
            labels: vec!["ready".into()],
            milestone: None,
            milestone_id: None,
            observed_at_unix_secs: 1_700_000_000,
        })
    }
}

struct EmptyPullRequests;
impl luthor::github::pull_request::PullRequestReader for EmptyPullRequests {
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<Value>, LookupError> {
        Ok(vec![])
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<Value, LookupError> {
        unreachable!()
    }
}

struct ClaimWriter {
    assignees: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    calls: usize,
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
        Ok(())
    }
}
