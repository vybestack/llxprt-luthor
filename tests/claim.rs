use luthor::{
    claim::{AssignmentError, ClaimError, claim},
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    eligibility::Candidate,
    github::{
        project::{ProjectItem, ProjectReadError, ReadCategory, ReadOperation},
        pull_request::{ErrorCategory, LookupError, LookupResult, PullRequestReader, lookup},
    },
    state::StateStore,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

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
fn claim_fixture() -> (
    tempfile::TempDir,
    Candidate,
    ClaimProjects,
    ClaimWriter,
    StateStore,
) {
    claim_fixture_with_milestone(None)
}

fn claim_fixture_with_milestone(
    milestone: Option<&str>,
) -> (
    tempfile::TempDir,
    Candidate,
    ClaimProjects,
    ClaimWriter,
    StateStore,
) {
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
    };
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
    store
        .create_task("task", &candidate, "rev", &config)
        .unwrap();
    (dir, candidate, projects, writer, store)
}

fn assert_claim_state(dir: &tempfile::TempDir, phase: &str, intent: bool, verified: bool) {
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

#[test]
fn verified_claim_assigns_once() {
    let (dir, candidate, mut projects, mut writer, mut store) = claim_fixture();
    let assignees = Rc::clone(&projects.assignees);
    let mut prs = EmptyPullRequests;
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
    assert_eq!(*assignees.borrow(), vec!["bot"]);
    assert_eq!(projects.issue_reads, 2);
    assert_claim_state(&dir, "claimed", true, true);
}

#[test]
fn optional_source_claims_persisted_issue_milestone_after_reopen() {
    let (dir, candidate, mut projects, mut writer, store) =
        claim_fixture_with_milestone(Some("0.12.0"));
    assert_eq!(candidate.source.milestone, None);
    drop(store);
    let mut reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(
        reopened
            .selection_evidence("task")
            .unwrap()
            .unwrap()
            .candidate,
        candidate
    );
    let mut prs = EmptyPullRequests;
    claim(
        &mut reopened,
        "task",
        &candidate,
        "bot",
        &mut projects,
        &mut prs,
        &mut writer,
    )
    .unwrap();
    assert_eq!(projects.issue_reads, 2);
    assert_eq!(writer.calls, 1);
    assert_claim_state(&dir, "claimed", true, true);
}

#[test]
fn changed_optional_issue_milestone_refuses_claim_before_assignment() {
    for (title, id) in [
        (Some("0.13.0"), Some("MILESTONE1")),
        (Some("0.12.0"), Some("MILESTONE2")),
        (None, None),
    ] {
        let (dir, candidate, mut projects, mut writer, mut store) =
            claim_fixture_with_milestone(Some("0.12.0"));
        projects.milestone = title.map(str::to_string);
        projects.milestone_id = id.map(str::to_string);
        let mut prs = EmptyPullRequests;
        assert!(matches!(
            claim(
                &mut store,
                "task",
                &candidate,
                "bot",
                &mut projects,
                &mut prs,
                &mut writer,
            ),
            Err(ClaimError::Changed)
        ));
        assert_eq!(writer.calls, 0);
        assert_eq!(projects.issue_reads, 1);
        assert_claim_state(&dir, "preparing", false, false);
    }
}

#[test]
fn later_project_page_failure_prevents_assignment_and_verification() {
    let (dir, candidate, mut projects, mut writer, mut store) = claim_fixture();
    projects.later_page = Some(Err(ProjectReadError {
        operation: ReadOperation::ProjectPage,
        project_id: Some(candidate.project_id.clone()),
        item_id: None,
        issue_id: None,
        category: ReadCategory::Transport,
        status: None,
        code: "read-failed".into(),
    }));
    let mut prs = EmptyPullRequests;
    assert!(matches!(
        claim(
            &mut store,
            "task",
            &candidate,
            "bot",
            &mut projects,
            &mut prs,
            &mut writer
        ),
        Err(ClaimError::Source(_))
    ));
    assert_eq!(writer.calls, 0);
    assert_eq!(*projects.assignees.borrow(), Vec::<String>::new());
    assert_eq!(projects.issue_reads, 0);
    assert_claim_state(&dir, "preparing", false, false);
}

#[test]
fn duplicate_issue_on_later_project_page_prevents_assignment_and_verification() {
    let (dir, candidate, mut projects, mut writer, mut store) = claim_fixture();
    projects.later_page = Some(Ok(vec![projects.item.clone()]));
    let mut prs = EmptyPullRequests;
    assert!(matches!(
        claim(
            &mut store,
            "task",
            &candidate,
            "bot",
            &mut projects,
            &mut prs,
            &mut writer
        ),
        Err(ClaimError::Changed)
    ));
    assert_eq!(writer.calls, 0);
    assert_eq!(*projects.assignees.borrow(), Vec::<String>::new());
    assert_eq!(projects.issue_reads, 0);
    assert_claim_state(&dir, "preparing", false, false);
}

#[test]
fn held_on_ambiguous_assignment_never_reissues_write() {
    let (dir, candidate, mut projects, mut writer, mut store) = claim_fixture();
    writer.fail = true;
    let mut prs = EmptyPullRequests;
    assert!(matches!(
        claim(
            &mut store,
            "task",
            &candidate,
            "bot",
            &mut projects,
            &mut prs,
            &mut writer
        ),
        Err(ClaimError::Assignment)
    ));
    assert_eq!(writer.calls, 1);
    assert_eq!(*projects.assignees.borrow(), vec!["bot"]);
    assert_claim_state(&dir, "held", true, false);
    drop(store);

    // The write could have succeeded despite the failed response; even a fresh read with
    // no assignee must not trigger a second assignment after restart.
    projects.assignees.borrow_mut().clear();
    writer.fail = false;
    let mut reopened = StateStore::open(dir.path(), 1).unwrap();
    assert!(matches!(
        claim(
            &mut reopened,
            "task",
            &candidate,
            "bot",
            &mut projects,
            &mut prs,
            &mut writer
        ),
        Err(ClaimError::State(
            luthor::state::StateError::InvalidSelection
        ))
    ));
    assert_eq!(projects.issue_reads, 2);
    assert_eq!(writer.calls, 1);
    assert_claim_state(&dir, "held", true, false);
}

#[test]
fn preexisting_linked_pr_prevents_claim() {
    let (dir, candidate, mut projects, mut writer, mut store) = claim_fixture();
    let body = format!("Tracker-Issue: {}", candidate.issue_url);
    let mut prs = Fake::default();
    prs.pages.insert(1, vec![item(9, &body)]);
    let mut linked = detail(9, &body);
    linked["html_url"] = json!("https://github.com/org/code/pull/9");
    linked["base"]["repo"]["full_name"] = json!("org/code");
    prs.details.insert(9, linked);
    assert!(matches!(
        claim(
            &mut store,
            "task",
            &candidate,
            "bot",
            &mut projects,
            &mut prs,
            &mut writer
        ),
        Err(ClaimError::ExistingPr)
    ));
    assert_eq!(writer.calls, 0);
    assert_eq!(projects.issue_reads, 1);
    assert_claim_state(&dir, "preparing", false, false);
}

#[test]
fn postwrite_project_error_holds_intent() {
    let (dir, candidate, mut projects, mut writer, mut store) = claim_fixture();
    projects.failed_issue_read = Some(2);
    let mut prs = EmptyPullRequests;
    assert!(matches!(
        claim(
            &mut store,
            "task",
            &candidate,
            "bot",
            &mut projects,
            &mut prs,
            &mut writer
        ),
        Err(ClaimError::Verify)
    ));
    assert_eq!(writer.calls, 1);
    assert_eq!(projects.issue_reads, 2);
    assert_eq!(*projects.assignees.borrow(), vec!["bot"]);
    assert_claim_state(&dir, "held", true, false);
    drop(store);
    let reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(reopened.evidence_kinds("task").unwrap(), vec!["selection"]);
    assert_claim_state(&dir, "held", true, false);
}

struct ClaimProjects {
    assignees: Rc<RefCell<Vec<String>>>,
    issue_reads: usize,
    failed_issue_read: Option<usize>,
    later_page: Option<Result<Vec<ProjectItem>, ProjectReadError>>,
    milestone: Option<String>,
    milestone_id: Option<String>,
    item: ProjectItem,
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
    assignees: Rc<RefCell<Vec<String>>>,
    calls: usize,
    fail: bool,
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

#[test]
fn optional_source_reconcile_compares_persisted_milestone_identity() {
    let (_dir, _candidate, mut projects, _writer, mut store) =
        claim_fixture_with_milestone(Some("0.12.0"));
    let unchanged =
        luthor::coordinator::reconcile_source(&mut store, "task", &mut projects).unwrap();
    assert_eq!(unchanged.project_membership, Some(true));
    assert!(!unchanged.reasons.contains(&"issue_identity_mismatch"));

    projects.milestone_id = Some("MILESTONE2".into());
    let changed = luthor::coordinator::reconcile_source(&mut store, "task", &mut projects).unwrap();
    assert!(changed.reasons.contains(&"issue_identity_mismatch"));
}

#[test]
fn source_reconcile_observes_claim_intent_without_attempt_or_assignment() {
    let (dir, candidate, mut projects, writer, mut store) = claim_fixture();
    store
        .record_claim_intent("task", "bot", &candidate.repository, candidate.issue_number)
        .unwrap();
    store.hold_task("task", "interrupted claim").unwrap();
    projects.assignees.borrow_mut().push("bot".into());
    let report = luthor::coordinator::reconcile_source(&mut store, "task", &mut projects).unwrap();
    assert_eq!(report.status, "held");
    assert_eq!(report.project_membership, Some(true));
    assert_eq!(report.marker_present, Some(true));
    assert_eq!(report.assignees, Some(vec!["bot".into()]));
    assert!(report.reasons.contains(&"claim_intent_unverified"));
    assert_eq!(writer.calls, 0);
    assert_eq!(store.latest_attempt("task").unwrap(), None);
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert!(
        store
            .unresolved_sources()
            .unwrap()
            .iter()
            .any(|(_, kind)| kind == "claim_assignment")
    );
    let db = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    let observed: String = db
        .query_row(
            "SELECT payload FROM evidence WHERE task_id='task' AND kind='source_observation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&observed).unwrap()["status"],
        "held"
    );
}

#[test]
fn source_reconcile_partial_worktree_and_source_errors_stay_held() {
    let (dir, candidate, mut projects, writer, mut store) = claim_fixture();
    store
        .record_claim_intent("task", "bot", &candidate.repository, candidate.issue_number)
        .unwrap();
    store
        .record_evidence("task", None, "claim_verified", "bot")
        .unwrap();
    store.set_task_phase("task", "claimed").unwrap();
    let root = dir.path().join("worktrees");
    std::fs::create_dir_all(root.join("task")).unwrap();
    store
        .begin_worktree(
            "task",
            &luthor::state::WorktreeIntent {
                path: std::fs::canonicalize(&root).unwrap().join("task"),
                branch: "luthor/task".into(),
                base: "main".into(),
                repository: "org/code".into(),
            },
        )
        .unwrap();
    store.hold_task("task", "interrupted worktree").unwrap();
    let report = luthor::coordinator::reconcile_source(&mut store, "task", &mut projects).unwrap();
    assert_eq!(
        report.worktree,
        Some(luthor::worktree::WorktreeInspection::UnverifiedPathPresent),
        "{report:?}"
    );
    assert!(report.reasons.contains(&"worktree_unverified"));
    projects.item.item_id = "other".into();
    let mismatch =
        luthor::coordinator::reconcile_source(&mut store, "task", &mut projects).unwrap();
    assert!(mismatch.reasons.contains(&"project_membership_mismatch"));
    projects.failed_issue_read = Some(projects.issue_reads + 1);
    projects.item.item_id = candidate.item_id;
    let unreadable =
        luthor::coordinator::reconcile_source(&mut store, "task", &mut projects).unwrap();
    assert!(unreadable.reasons.contains(&"source_read_failed"));
    assert_eq!(writer.calls, 0);
    assert_eq!(store.latest_attempt("task").unwrap(), None);
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert!(
        store
            .unresolved_sources()
            .unwrap()
            .iter()
            .any(|(_, kind)| kind == "worktree_create")
    );
    assert!(store.ensure_dispatch_capacity().is_err());
}

#[test]
fn fully_recorded_source_without_attempt_still_blocks_new_selection() {
    let (dir, candidate, mut projects, writer, mut store) = claim_fixture();
    store
        .record_claim_intent("task", "bot", &candidate.repository, candidate.issue_number)
        .unwrap();
    store
        .record_evidence("task", None, "claim_verified", "bot")
        .unwrap();
    store.set_task_phase("task", "claimed").unwrap();
    let root = dir.path().join("worktrees");
    std::fs::create_dir_all(root.join("task")).unwrap();
    let path = std::fs::canonicalize(&root).unwrap().join("task");
    store
        .begin_worktree(
            "task",
            &luthor::state::WorktreeIntent {
                path: path.clone(),
                branch: "luthor/task".into(),
                base: "main".into(),
                repository: "org/code".into(),
            },
        )
        .unwrap();
    store
        .finish_worktree(
            "task",
            &luthor::state::WorktreeIdentity {
                path,
                device: 1,
                inode: 1,
                branch: "luthor/task".into(),
                base: "main".into(),
                head: "bogus".into(),
                repository: "org/code".into(),
                git_directory: root,
                remote: "origin".into(),
            },
        )
        .unwrap();
    assert!(
        store
            .unresolved_sources()
            .unwrap()
            .iter()
            .any(|(_, kind)| kind == "prelaunch")
    );
    let report = luthor::coordinator::reconcile_source(&mut store, "task", &mut projects).unwrap();
    assert_eq!(report.status, "held");
    assert!(report.reasons.contains(&"worktree_read_failed"));
    assert_eq!(writer.calls, 0);
    assert_eq!(store.latest_attempt("task").unwrap(), None);
    assert!(
        store
            .unresolved_sources()
            .unwrap()
            .iter()
            .any(|(_, kind)| kind == "prelaunch")
    );
}
