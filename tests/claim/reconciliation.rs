use super::support::*;
use luthor::state::{journal, scheduling, task_records, worktree_records};

pub(crate) fn optional_source_reconcile_compares_persisted_milestone_identity() {
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

pub(crate) fn source_reconcile_observes_claim_intent_without_attempt_or_assignment() {
    let (dir, candidate, mut projects, writer, mut store) = claim_fixture();
    task_records::record_claim_intent(
        &mut store,
        "task",
        "bot",
        &candidate.repository,
        candidate.issue_number,
    )
    .unwrap();
    task_records::hold_task(&mut store, "task", "interrupted claim").unwrap();
    projects.assignees.borrow_mut().push("bot".into());
    let report = luthor::coordinator::reconcile_source(&mut store, "task", &mut projects).unwrap();
    assert_eq!(report.status, "held");
    assert_eq!(report.project_membership, Some(true));
    assert_eq!(report.marker_present, Some(true));
    assert_eq!(report.assignees, Some(vec!["bot".into()]));
    assert!(report.reasons.contains(&"claim_intent_unverified"));
    assert_eq!(writer.calls, 0);
    assert_eq!(task_records::latest_attempt(&store, "task").unwrap(), None);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert!(
        scheduling::unresolved_sources(&store)
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

pub(crate) fn source_reconcile_partial_worktree_and_source_errors_stay_held() {
    let (dir, candidate, mut projects, writer, mut store) = claim_fixture();
    task_records::record_claim_intent(
        &mut store,
        "task",
        "bot",
        &candidate.repository,
        candidate.issue_number,
    )
    .unwrap();
    journal::record_evidence(&mut store, "task", None, "claim_verified", "bot").unwrap();
    task_records::set_task_phase(&mut store, "task", "claimed").unwrap();
    let root = dir.path().join("worktrees");
    std::fs::create_dir_all(root.join("task")).unwrap();
    worktree_records::begin_worktree(
        &mut store,
        "task",
        &luthor::state::WorktreeIntent {
            path: std::fs::canonicalize(&root).unwrap().join("task"),
            branch: "luthor/task".into(),
            base: "main".into(),
            repository: "org/code".into(),
        },
    )
    .unwrap();
    task_records::hold_task(&mut store, "task", "interrupted worktree").unwrap();
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
    assert_eq!(task_records::latest_attempt(&store, "task").unwrap(), None);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert!(
        scheduling::unresolved_sources(&store)
            .unwrap()
            .iter()
            .any(|(_, kind)| kind == "worktree_create")
    );
    assert!(scheduling::ensure_dispatch_capacity(&store).is_err());
}

pub(crate) fn fully_recorded_source_without_attempt_still_blocks_new_selection() {
    let (dir, candidate, mut projects, writer, mut store) = claim_fixture();
    task_records::record_claim_intent(
        &mut store,
        "task",
        "bot",
        &candidate.repository,
        candidate.issue_number,
    )
    .unwrap();
    journal::record_evidence(&mut store, "task", None, "claim_verified", "bot").unwrap();
    task_records::set_task_phase(&mut store, "task", "claimed").unwrap();
    let root = dir.path().join("worktrees");
    std::fs::create_dir_all(root.join("task")).unwrap();
    let path = std::fs::canonicalize(&root).unwrap().join("task");
    worktree_records::begin_worktree(
        &mut store,
        "task",
        &luthor::state::WorktreeIntent {
            path: path.clone(),
            branch: "luthor/task".into(),
            base: "main".into(),
            repository: "org/code".into(),
        },
    )
    .unwrap();
    worktree_records::finish_worktree(
        &mut store,
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
        scheduling::unresolved_sources(&store)
            .unwrap()
            .iter()
            .any(|(_, kind)| kind == "prelaunch")
    );
    let report = luthor::coordinator::reconcile_source(&mut store, "task", &mut projects).unwrap();
    assert_eq!(report.status, "held");
    assert!(report.reasons.contains(&"worktree_read_failed"));
    assert_eq!(writer.calls, 0);
    assert_eq!(task_records::latest_attempt(&store, "task").unwrap(), None);
    assert!(
        scheduling::unresolved_sources(&store)
            .unwrap()
            .iter()
            .any(|(_, kind)| kind == "prelaunch")
    );
}
