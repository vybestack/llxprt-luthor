use super::*;
use luthor::state::{journal, scheduling, task_records};

#[cfg(unix)]
pub(crate) fn natural_exit_pr_error_keeps_held_slot_and_evidence() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let mut prs = ExitPr {
        fail: true,
        ..Default::default()
    };
    let mut projects = OtherProject(
        task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    let report =
        luthor::coordinator::startup_reconcile_all(&mut store, &mut projects, &mut prs).unwrap();
    assert!(matches!(
        report.attempts[0].review,
        luthor::coordinator::AttemptReview::Held(_)
    ));
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert!(matches!(
        scheduling::ensure_dispatch_capacity(&store),
        Err(StateError::Capacity { .. })
    ));
    let proof = exit_proof(&config);
    assert!(matches!(
        proof.status,
        PausePrStatus::Error {
            category: ErrorCategory::Transport,
            ..
        }
    ));
    assert!(scheduling::pending_attempts(&store).unwrap().is_empty());
    assert_eq!(prs.reads, 1);
    let outcome_before: String =
        rusqlite::Connection::open(config.state_root.join("state.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT outcome FROM attempts WHERE id='attempt-real'",
                [],
                |row| row.get(0),
            )
            .unwrap();

    assert_pr_error_recovery(&config, &mut store, &outcome_before);
    drop(store);
    let mut reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    let mut startup_prs = ExitPr::default();
    let mut projects = OtherProject(
        task_records::selection_evidence(&reopened, "task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    let startup =
        luthor::coordinator::startup_reconcile_all(&mut reopened, &mut projects, &mut startup_prs)
            .unwrap();
    assert!(startup.attempts.is_empty());
    assert_eq!(startup_prs.reads, 0);
    assert_eq!(
        task_records::task_phase(&reopened, "task")
            .unwrap()
            .as_deref(),
        Some("attention")
    );
}

#[cfg(unix)]
pub(crate) fn uncertain_child_group_never_reads_pr_or_frees_capacity() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    edit_receipt(&config, |receipt| receipt.child_pid = std::process::id());
    let mut prs = ExitPr::default();
    let mut projects = OtherProject(
        task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    let report =
        luthor::coordinator::startup_reconcile_all(&mut store, &mut projects, &mut prs).unwrap();
    assert!(matches!(
        report.attempts[0].review,
        luthor::coordinator::AttemptReview::Held(_)
    ));
    assert_eq!(prs.reads, 0);
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(matches!(
        scheduling::ensure_dispatch_capacity(&store),
        Err(StateError::Capacity { .. })
    ));
}

#[cfg(unix)]
pub(crate) fn verified_exit_seven_releases_once_and_survives_restart() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let expected = Reconciliation::Completed {
        exit_code: Some(7),
        signal: None,
    };
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        expected
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    drop(store);
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        expected
    );
    assert_eq!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .filter(|k| *k == "attempt_exit")
            .count(),
        1
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    edit_receipt(&config, |r| r.exit_code = Some(0));
    assert!(reconcile_attempt(&mut store, "task", "attempt-real").is_err());
    assert_eq!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .filter(|k| *k == "attempt_exit")
            .count(),
        1
    );
}

#[cfg(unix)]
fn assert_pr_error_recovery(config: &Config, store: &mut StateStore, outcome_before: &str) {
    let mut recovered_prs = ExitPr::default();
    let mut projects = OtherProject(
        task_records::selection_evidence(store, "task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    assert!(matches!(
        luthor::coordinator::reconcile_with_pr(
            store,
            "task",
            "attempt-real",
            &mut projects,
            &mut recovered_prs
        )
        .unwrap(),
        Reconciliation::Completed {
            exit_code: Some(7),
            signal: None
        }
    ));
    assert_eq!(recovered_prs.reads, 1);
    assert_eq!(
        task_records::task_phase(store, "task").unwrap().as_deref(),
        Some("attention")
    );
    assert_eq!(scheduling::reservation_count(store).unwrap(), 0);
    assert_eq!(
        task_records::latest_attempt(store, "task")
            .unwrap()
            .as_deref(),
        Some("attempt-real")
    );
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let records: Vec<String> = connection
        .prepare("SELECT payload FROM evidence WHERE task_id='task' AND attempt_id='attempt-real' AND kind='exit_pr_lookup' ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let proofs: Vec<ExitPrEvidence> = records
        .iter()
        .map(|record| serde_json::from_str(record).unwrap())
        .collect();
    assert_eq!(proofs.len(), 2);
    assert!(matches!(proofs[0].status, PausePrStatus::Error { .. }));
    assert_eq!(proofs[1].status, PausePrStatus::Absent);
    let outcome_after: String = connection
        .query_row(
            "SELECT outcome FROM attempts WHERE id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome_after, outcome_before);
}
