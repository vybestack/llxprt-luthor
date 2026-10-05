use super::*;
use luthor::state::{journal, scheduling, task_records, worktree_records};

#[cfg(unix)]
pub(crate) fn operator_recovery_matching_pr_completes_task_and_persists_proof() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = journal::evidence_payloads(&store, "task", "attempt-real", "supervisor_ready")
        .unwrap()
        .remove(0);
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    wait_for_process_and_group_absence(pid);
    fs::remove_file(receipt_path(&config)).unwrap();
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let branch = worktree_records::worktree_record(&store, "task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap()
        .branch;
    let mut projects = OtherProject(selection.candidate.clone(), 1);
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url.clone(), branch)),
        ..ExitPr::default()
    };
    assert_eq!(
        operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "receipt lost",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        luthor::coordinator::RecoveryResult::RecoveredPrComplete { pr_id: 4242 }
    );
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    let kinds = journal::evidence_kinds(&store, "task").unwrap();
    for kind in ["telemetry_lost", "exit_pr_lookup", "verified_open_pr"] {
        assert_eq!(
            kinds.iter().filter(|seen| *seen == kind).count(),
            1,
            "{kind}"
        );
    }
    assert!(!kinds.iter().any(|kind| kind == "attempt_exit"));
    assert_matching_recovery_after_restart(&config, store);
}

#[cfg(unix)]
fn assert_matching_recovery_after_restart(config: &Config, store: StateStore) {
    drop(store);
    let reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        task_records::task_phase(&reopened, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    assert_eq!(
        journal::evidence_kinds(&reopened, "task")
            .unwrap()
            .iter()
            .filter(|kind| *kind == "verified_open_pr")
            .count(),
        1
    );
    assert!(scheduling::pending_attempts(&reopened).unwrap().is_empty());
    assert_eq!(
        task_records::task_phase(&reopened, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    scheduling::ensure_dispatch_capacity(&reopened).unwrap();
    let db = rusqlite::Connection::open_with_flags(
        config.state_root.join("state.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let outcome: Option<String> = db
        .query_row(
            "SELECT outcome FROM attempts WHERE task_id='task' AND id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, None);
    assert!(
        !journal::evidence_kinds(&reopened, "task")
            .unwrap()
            .contains(&"attempt_exit".to_owned())
    );
}
