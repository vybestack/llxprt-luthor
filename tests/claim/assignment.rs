use super::support::*;
use luthor::state::{journal, task_records};

pub(crate) fn verified_claim_assigns_once() {
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

pub(crate) fn optional_source_claims_persisted_issue_milestone_after_reopen() {
    let (dir, candidate, mut projects, mut writer, store) =
        claim_fixture_with_milestone(Some("0.12.0"));
    assert_eq!(candidate.source.milestone, None);
    drop(store);
    let mut reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(
        task_records::selection_evidence(&reopened, "task")
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

pub(crate) fn changed_optional_issue_milestone_refuses_claim_before_assignment() {
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

pub(crate) fn later_project_page_failure_prevents_assignment_and_verification() {
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

pub(crate) fn duplicate_issue_on_later_project_page_prevents_assignment_and_verification() {
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

pub(crate) fn held_on_ambiguous_assignment_never_reissues_write() {
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

pub(crate) fn preexisting_linked_pr_prevents_claim() {
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

pub(crate) fn postwrite_project_error_holds_intent() {
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
    assert_eq!(
        journal::evidence_kinds(&reopened, "task").unwrap(),
        vec!["selection"]
    );
    assert_claim_state(&dir, "held", true, false);
}

pub(crate) fn assignment_observes_durable_claim_intent_before_external_write() {
    struct DurableAssignment {
        db: std::path::PathBuf,
        assignees: Rc<RefCell<Vec<String>>>,
        observed: bool,
    }
    impl luthor::claim::AssignmentWriter for DurableAssignment {
        fn assign(&mut self, _: &str, _: u64, principal: &str) -> Result<(), AssignmentError> {
            let db = rusqlite::Connection::open(&self.db).unwrap();
            let count: usize = db
                .query_row(
                    "SELECT COUNT(*) FROM intents WHERE task_id='task' AND kind='claim_assignment'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "assignment must not precede durable claim intent");
            self.observed = true;
            self.assignees.borrow_mut().push(principal.into());
            Ok(())
        }
    }
    let (dir, candidate, mut projects, _, mut store) = claim_fixture();
    let mut writer = DurableAssignment {
        db: store.root().join("state.sqlite3"),
        assignees: Rc::clone(&projects.assignees),
        observed: false,
    };
    claim(
        &mut store,
        "task",
        &candidate,
        "bot",
        &mut projects,
        &mut EmptyPullRequests,
        &mut writer,
    )
    .unwrap();
    assert!(writer.observed);
    assert_claim_state(&dir, "claimed", true, true);
    assert_eq!(*projects.assignees.borrow(), ["bot"]);
}
