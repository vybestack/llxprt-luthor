use super::*;
use luthor::state::{journal, scheduling, task_records};

#[cfg(unix)]
pub(crate) fn operator_recovery_missing_owner_proof_holds_before_github() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = journal::evidence_payloads(&store, "task", "attempt-real", "supervisor_ready")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    wait_for_process_and_group_absence(pid);
    fs::remove_file(receipt_path(&config)).unwrap();
    rusqlite::Connection::open(config.state_root.join("state.sqlite3"))
        .unwrap()
        .execute(
            "DELETE FROM evidence WHERE task_id='task' AND attempt_id='attempt-real' AND kind='worktree_owner_protocol'",
            [],
        )
        .unwrap();
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let mut projects = OtherProject(selection.candidate, 1);
    let mut prs = ExitPr::default();
    assert!(matches!(
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
        luthor::coordinator::RecoveryResult::Held(_)
    ));
    assert_eq!(prs.reads, 0);
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}

#[cfg(unix)]
pub(crate) fn operator_recovery_pr_lookup_failure_keeps_slot_reserved() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = journal::evidence_payloads(&store, "task", "attempt-real", "supervisor_ready")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    wait_for_process_and_group_absence(pid);
    fs::remove_file(receipt_path(&config)).unwrap();
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let mut projects = OtherProject(selection.candidate, 1);
    let mut prs = ExitPr {
        fail: true,
        ..ExitPr::default()
    };
    assert!(matches!(
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
        luthor::coordinator::RecoveryResult::Held(_)
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(prs.reads, 1);
}

#[cfg(unix)]
pub(crate) fn operator_recovery_absent_pr_records_telemetry_loss_and_releases_slot() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = journal::evidence_payloads(&store, "task", "attempt-real", "supervisor_ready")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    wait_for_process_and_group_absence(pid);
    fs::remove_file(receipt_path(&config)).unwrap();
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let mut projects = OtherProject(selection.candidate, 1);
    let mut prs = ExitPr::default();
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
        luthor::coordinator::RecoveryResult::RecoveredHeld
    );
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert_eq!(prs.reads, 1);
    assert!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"telemetry_lost".into())
    );
    assert!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"exit_pr_lookup".into())
    );
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"attempt_exit".into())
    );
    assert_absent_recovery_after_restart(&config, store);
}

#[cfg(unix)]
fn assert_absent_recovery_after_restart(config: &Config, store: StateStore) {
    let db = rusqlite::Connection::open_with_flags(
        config.state_root.join("state.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (lifecycle, outcome): (String, Option<String>) = db
        .query_row(
            "SELECT lifecycle, outcome FROM attempts WHERE task_id='task' AND id='attempt-real'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(lifecycle, "telemetry_lost");
    assert_eq!(outcome, None);
    drop(db);
    drop(store);
    let reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        task_records::task_phase(&reopened, "task")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert_eq!(scheduling::reservation_count(&reopened).unwrap(), 0);
    assert_eq!(
        scheduling::pending_attempts(&reopened).unwrap(),
        vec![("task".to_owned(), "attempt-real".to_owned())]
    );
    assert!(scheduling::ensure_dispatch_capacity(&reopened).is_err());
    assert!(
        journal::evidence_kinds(&reopened, "task")
            .unwrap()
            .contains(&"telemetry_lost".into())
    );
    assert!(
        journal::evidence_kinds(&reopened, "task")
            .unwrap()
            .contains(&"exit_pr_lookup".into())
    );
    assert!(
        !journal::evidence_kinds(&reopened, "task")
            .unwrap()
            .contains(&"attempt_exit".into())
    );
}
