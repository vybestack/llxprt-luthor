use super::*;
use luthor::state::{journal, scheduling, task_records, worktree_records};

#[cfg(unix)]
pub(crate) fn natural_exit_matching_pr_is_proved_and_persisted() {
    let (_dir, mut config, mut store) = dispatched_fixture(7);
    config.capacity = 1;
    drop(store);
    store = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert!(scheduling::ensure_dispatch_capacity(&store).is_err());
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let identity = worktree_records::worktree_record(&store, "task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url, identity.branch)),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(
        task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    let result = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut projects,
        &mut prs,
    )
    .unwrap();
    assert!(
        matches!(
            result,
            Reconciliation::Completed {
                exit_code: Some(7),
                signal: None
            }
        ),
        "unexpected reconciliation: {result:?}; held reason: {:?}",
        task_records::held_reason(&store, "task").unwrap()
    );
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert_eq!(exit_proof(&config).status, PausePrStatus::Open);
    assert!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"attempt_exit".into())
    );
    assert!(scheduling::ensure_dispatch_capacity(&store).is_ok());
    drop(store);
    let reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        task_records::task_phase(&reopened, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    assert!(scheduling::ensure_dispatch_capacity(&reopened).is_ok());
    assert_eq!(exit_proof(&config).status, PausePrStatus::Open);
    let output = luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap();
    let shown: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        shown["attempts"][0]["outcome"],
        "exit_code=Some(7);signal=None"
    );
    assert_eq!(prs.reads, 1);
}

#[cfg(unix)]
pub(crate) fn missing_receipt_cannot_be_overridden_by_matching_open_pr() {
    let (_dir, mut config, mut store) = dispatched_fixture(0);
    config.capacity = 1;
    let receipt = receipt_path(&config);
    assert!(receipt.exists());
    fs::remove_file(&receipt).unwrap();

    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let identity = worktree_records::worktree_record(&store, "task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url, identity.branch)),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(
        task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    assert!(matches!(
        luthor::coordinator::reconcile_with_pr(
            &mut store,
            "task",
            "attempt-real",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        Reconciliation::Held { .. }
    ));
    assert_eq!(prs.reads, 0);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(matches!(
        scheduling::ensure_dispatch_capacity(&store),
        Err(StateError::Capacity { .. })
    ));
    let kinds = journal::evidence_kinds(&store, "task").unwrap();
    assert!(!kinds.iter().any(|kind| kind == "verified_open_pr"));
    assert!(!kinds.iter().any(|kind| kind == "attempt_exit"));
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let outcome: Option<String> = connection
        .query_row(
            "SELECT outcome FROM attempts WHERE id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, None);

    drop(store);
    assert_missing_receipt_after_restart(&config, &prs);
}

#[cfg(unix)]
pub(crate) fn stopped_exit_matching_pr_is_proved_and_persisted() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    journal::record_stop_intent(&mut store, "task", "attempt-real").unwrap();
    edit_receipt(&config, |receipt| {
        receipt.stop_signals = vec![libc::SIGTERM]
    });
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let identity = worktree_records::worktree_record(&store, "task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url, identity.branch)),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(
        task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    let result = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut projects,
        &mut prs,
    )
    .unwrap();
    assert_eq!(
        result,
        Reconciliation::Completed {
            exit_code: Some(7),
            signal: None
        }
    );
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt_path(&config)).unwrap()).unwrap();
    assert_eq!(receipt.stop_signals, vec![libc::SIGTERM]);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert_eq!(pause_proof(&config).status, PausePrStatus::Open);
    let output = luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap();
    let shown: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        shown["attempts"][0]["outcome"],
        "exit_code=Some(7);signal=None"
    );
    assert_eq!(prs.reads, 1);
    drop(store);

    let mut reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        task_records::task_phase(&reopened, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    let output = luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap();
    let shown: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        shown["attempts"][0]["outcome"],
        "exit_code=Some(7);signal=None"
    );
    assert_eq!(pause_proof(&config).status, PausePrStatus::Open);
    assert!(matches!(
        prepare_resume(&mut reopened, "task", "attempt-next"),
        Err(SupervisorError::State(StateError::LaunchBlocked))
    ));
}

#[cfg(unix)]
fn assert_missing_receipt_after_restart(config: &Config, prs: &ExitPr) {
    let mut reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    let mut startup_prs = ExitPr {
        matching: prs.matching.clone(),
        ..ExitPr::default()
    };
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
    assert!(matches!(
        startup.attempts[0].review,
        luthor::coordinator::AttemptReview::Held(_)
    ));
    assert_eq!(startup_prs.reads, 0);
    assert_eq!(
        task_records::task_phase(&reopened, "task")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert_eq!(scheduling::reservation_count(&reopened).unwrap(), 1);
    assert!(matches!(
        scheduling::ensure_dispatch_capacity(&reopened),
        Err(StateError::Capacity { .. })
    ));
    assert!(
        !journal::evidence_kinds(&reopened, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}
