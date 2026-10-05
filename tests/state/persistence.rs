use super::*;
use luthor::state::{journal, pr_completion, scheduling, task_records};

pub(crate) fn evidence_payloads_are_ordered_task_scoped_and_durable() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    task_records::create_task(&mut store, "task", &candidate("issue", 1), "rev", &config())
        .unwrap();
    task_records::create_task(
        &mut store,
        "other-task",
        &candidate("other-issue", 2),
        "rev",
        &config(),
    )
    .unwrap();
    journal::record_evidence(
        &mut store,
        "task",
        Some("shared-attempt"),
        "tracked_descendant",
        "first",
    )
    .unwrap();
    journal::record_evidence(
        &mut store,
        "task",
        Some("shared-attempt"),
        "tracked_descendant",
        "second",
    )
    .unwrap();
    journal::record_evidence(
        &mut store,
        "other-task",
        Some("shared-attempt"),
        "tracked_descendant",
        "other task",
    )
    .unwrap();
    journal::record_evidence(
        &mut store,
        "task",
        Some("shared-attempt"),
        "different_kind",
        "other kind",
    )
    .unwrap();

    assert_eq!(
        journal::evidence_payloads(&store, "task", "shared-attempt", "tracked_descendant").unwrap(),
        vec!["first", "second"]
    );
    assert!(
        journal::evidence_payloads(&store, "missing", "shared-attempt", "tracked_descendant")
            .unwrap()
            .is_empty()
    );
    drop(store);

    let reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(
        journal::evidence_payloads(&reopened, "task", "shared-attempt", "tracked_descendant")
            .unwrap(),
        vec!["first", "second"]
    );
}

pub(crate) fn failed_attempt_insert_rolls_back_its_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 2).unwrap();
    task_records::create_task(&mut store, "t1", &candidate("i1", 1), "rev", &config()).unwrap();
    task_records::create_task(&mut store, "t2", &candidate("i2", 2), "rev", &config()).unwrap();
    scheduling::reserve(&mut store, "t1", "a1").unwrap();
    assert!(scheduling::reserve(&mut store, "t2", "a1").is_err());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}

pub(crate) fn persists_identity_evidence_and_reservations_transactionally() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    task_records::create_task(&mut store, "t1", &candidate("ISSUE1", 1), "rev", &config()).unwrap();
    assert!(matches!(
        task_records::create_task(&mut store, "t2", &candidate("ISSUE1", 1), "rev", &config()),
        Err(StateError::DuplicateTask(_, _))
    ));
    journal::record_evidence(&mut store, "t1", None, "project", "item-1").unwrap();
    journal::record_evidence(&mut store, "t1", None, "direct_issue", "issue-1").unwrap();
    assert_eq!(
        journal::evidence_kinds(&store, "t1").unwrap(),
        vec!["selection", "project", "direct_issue"]
    );
    let selection = task_records::selection_evidence(&store, "t1")
        .unwrap()
        .unwrap();
    assert_eq!(selection.candidate.project_id, "project-1");
    assert_eq!(selection.candidate.item_id, "item-ISSUE1");
    assert_eq!(
        selection.candidate.marker,
        Marker::Label {
            name: "ready".into()
        }
    );
    assert_eq!(selection.candidate.milestone_title, None);
    assert_eq!(selection.candidate.mapping.code_repository, "org/code");
    assert_eq!(selection.candidate.observed_at_unix_secs, 123);
    assert_eq!(selection.candidate.observed_state, "open");
    assert_eq!(selection.candidate.observed_labels, vec!["ready"]);
    assert!(selection.candidate.observed_assignees.is_empty());
    assert_eq!(selection.config_revision, "rev");
    assert_eq!(selection.effective_config.capacity, 1);
    assert_eq!(
        selection.effective_config.worktree_root,
        std::path::PathBuf::from("/worktrees")
    );
    assert_eq!(
        selection.effective_config.initial.args,
        vec!["start", "{task.id}"]
    );
    assert_eq!(
        selection.effective_config.mappings[0].code_repository,
        "org/code"
    );
    assert!(matches!(
        task_records::create_task(&mut store, "t2", &candidate("ISSUE1", 1), "rev", &config()),
        Err(StateError::DuplicateTask(_, _))
    ));
    assert_eq!(task_records::task_count(&store).unwrap(), 1);
    assert_eq!(
        task_records::selection_evidence(&store, "t2").unwrap(),
        None
    );
    scheduling::reserve(&mut store, "t1", "a1").unwrap();
    assert!(matches!(
        scheduling::reserve(&mut store, "t1", "a2"),
        Err(StateError::Capacity { .. })
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(
        scheduling::release_reservation(&mut store, "a1").unwrap(),
        1
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
}

pub(crate) fn reservation_history_allows_a_new_attempt_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    task_records::create_task(&mut store, "task", &candidate("issue", 1), "rev", &config())
        .unwrap();
    journal::record_evidence(
        &mut store,
        "task",
        Some("first"),
        "result",
        "prior evidence",
    )
    .unwrap();
    scheduling::reserve(&mut store, "task", "first").unwrap();
    assert!(matches!(
        scheduling::reserve(&mut store, "task", "blocked"),
        Err(StateError::Capacity { .. })
    ));
    assert_eq!(
        scheduling::release_reservation(&mut store, "first").unwrap(),
        1
    );
    drop(store);

    let mut store = StateStore::open(dir.path(), 1).unwrap();
    scheduling::reserve(&mut store, "task", "second").unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(
        journal::evidence_kinds(&store, "task").unwrap(),
        vec!["selection", "result"]
    );
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    assert_eq!(selection.candidate.project_id, "project-1");
    assert_eq!(selection.candidate.item_id, "item-issue");
    assert_eq!(
        selection.candidate.marker,
        Marker::Label {
            name: "ready".into()
        }
    );
    assert_eq!(selection.candidate.milestone_title, None);
    assert_eq!(selection.candidate.mapping.code_repository, "org/code");
    assert_eq!(selection.candidate.observed_at_unix_secs, 123);
    assert_eq!(selection.candidate.observed_state, "open");
    assert_eq!(selection.candidate.observed_labels, vec!["ready"]);
    assert!(selection.candidate.observed_assignees.is_empty());
    assert_eq!(selection.config_revision, "rev");
    assert_eq!(selection.effective_config.capacity, 1);
    assert_eq!(
        selection.effective_config.worktree_root,
        std::path::PathBuf::from("/worktrees")
    );
    assert_eq!(
        selection.effective_config.initial.args,
        vec!["start", "{task.id}"]
    );
    assert_eq!(
        selection.effective_config.mappings[0].code_repository,
        "org/code"
    );
    let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    let rows: Vec<(String, String)> = connection
        .prepare("SELECT attempt_id,status FROM reservations ORDER BY attempt_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("first".into(), "released".into()),
            ("second".into(), "reserved".into())
        ]
    );
}

pub(crate) fn verified_open_pr_requires_a_terminal_attempt() {
    use luthor::{
        github::pull_request::PullRequestEvidence,
        pr_evidence::{ExpectedPr, VerifiedOpenPr},
    };

    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    task_records::create_task(&mut store, "task", &candidate("issue", 4), "rev", &config())
        .unwrap();

    let expected = ExpectedPr {
        issue_url: "https://github.com/org/tracker/issues/4".into(),
        repository_id: 10,
        repository: "org/code".into(),
        base_branch: "main".into(),
        head_repository_id: 11,
        head_repository: "bot/fork".into(),
        task_branch: "luthor/task-4".into(),
        allowed_author: "bot".into(),
        current_identity: "bot".into(),
    };
    let proof = VerifiedOpenPr::from_matching(
        PullRequestEvidence {
            id: 9,
            number: 4,
            url: "https://github.com/org/code/pull/4".into(),
            repository_id: 10,
            repository: "org/code".into(),
            base_repository_id: 10,
            base_repository: "org/code".into(),
            base_branch: "main".into(),
            head_repository_id: 11,
            head_repository: "bot/fork".into(),
            head_branch: "luthor/task-4".into(),
            author: "bot".into(),
            draft: false,
            tracker_issue_url: "https://github.com/org/tracker/issues/4".into(),
            body: "Tracker-Issue: https://github.com/org/tracker/issues/4".into(),
            checks: Some(vec![]),
            created_at: "2026-09-29T00:00:00Z".into(),
            commit_sha: "abc123".into(),
        },
        &expected,
        "bot",
        "attempt-1",
        42,
    )
    .unwrap();

    assert!(matches!(
        pr_completion::record_verified_open_pr(&mut store, "task", "attempt-1", &proof),
        Err(StateError::LaunchBlocked)
    ));
    assert_ne!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"verified_open_pr".to_owned())
    );
}
