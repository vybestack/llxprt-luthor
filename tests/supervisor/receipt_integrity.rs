use super::*;
use luthor::WorktreeOwner;
use luthor::state::{journal, scheduling, task_records};

#[cfg(unix)]
pub(crate) fn absent_and_corrupt_receipt_keep_reservation() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    let receipt = receipt_path(&config);
    fs::remove_file(&receipt).unwrap();
    hold_slot(&mut store);
    fs::write(&receipt, b"{partial").unwrap();
    hold_slot(&mut store);
}

#[cfg(unix)]
pub(crate) fn plan_without_persisted_launch_intent_keeps_slot() {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    task_records::create_task(&mut store, "task", &candidate, "rev", &config).unwrap();
    scheduling::reserve(&mut store, "task", "attempt-real").unwrap();
    let attempts = config.state_root.join("attempts");
    fs::DirBuilder::new().mode(0o700).create(&attempts).unwrap();
    let mut plan = fake_plan(dir.path(), "attempt-real", 0);
    plan.session_id = "task".into();
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempts.join("attempt-real.plan.json"))
        .unwrap();
    serde_json::to_writer(file, &plan).unwrap();
    assert!(
        matches!(reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "missing launch intent")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}

#[cfg(unix)]
pub(crate) fn missing_receipt_after_dispatch_before_worker_start_keeps_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    let owner = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    assert!(
        execute_with_binary(
            &mut store,
            &plan,
            &dir.path().join("missing-binary"),
            &owner
        )
        .is_err()
    );
    assert!(!receipt_path(&config).exists());
    hold_slot(&mut store);
}

#[cfg(unix)]
pub(crate) fn incomplete_or_nonregular_logs_keep_reservation() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    let stdout = config.state_root.join("attempts/attempt-real.stdout.log");
    let original = fs::read(&stdout).unwrap();
    fs::write(&stdout, &original[..original.len() - 1]).unwrap();
    hold_slot(&mut store);
    fs::remove_file(&stdout).unwrap();
    std::os::unix::fs::symlink(receipt_path(&config), &stdout).unwrap();
    hold_slot(&mut store);
}

#[cfg(unix)]
pub(crate) fn contradictory_identity_and_malformed_receipts_keep_reservation() {
    let (_dir, _config, mut store) = dispatched_fixture(0);
    let fake = serde_json::json!({"pid":std::process::id(),"boot_identity":"fake","start_identity":"fake"});
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "gate_sent",
        &fake.to_string(),
    )
    .unwrap();
    assert!(reconcile_attempt(&mut store, "task", "attempt-real").is_err());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    let (_other_dir, config, mut other_store) = dispatched_fixture(0);
    edit_receipt(&config, |r| r.signal = Some(9));
    hold_slot(&mut other_store);
}

#[cfg(unix)]
pub(crate) fn reused_child_pid_with_contradictory_start_identity_keeps_slot() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    let (boot, _) = test_process_identity(std::process::id());
    edit_receipt(&config, |r| {
        r.child_pid = std::process::id();
        r.boot_identity = boot;
        r.child_start_identity = "not this process start".into();
    });
    assert!(
        matches!(reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "receipt identity or shape mismatch")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}

#[cfg(unix)]
pub(crate) fn gate_release_decision_reconciles_without_gate_sent_evidence() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute(
        "DELETE FROM evidence WHERE task_id='task' AND attempt_id='attempt-real' AND kind='gate_sent'",
        [],
    )
    .unwrap();
    let expected = Reconciliation::Completed {
        exit_code: Some(0),
        signal: None,
    };
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        expected
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        expected
    );
    assert_eq!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .filter(|kind| *kind == "attempt_exit")
            .count(),
        1
    );
}

#[cfg(unix)]
pub(crate) fn missing_child_registration_holds_even_with_valid_receipt() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute(
        "DELETE FROM evidence WHERE attempt_id='attempt-real' AND kind='child_registered'",
        [],
    )
    .unwrap();
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "missing child registration"
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}

#[cfg(unix)]
pub(crate) fn live_child_group_is_not_released() {
    use std::os::unix::process::CommandExt;
    let (_dir, config, mut store) = dispatched_fixture(0);
    let mut live = Command::new("/bin/sleep")
        .arg("10")
        .process_group(0)
        .spawn()
        .unwrap();
    assert_eq!(unsafe { libc::kill(-(live.id() as i32), 0) }, 0);
    let (boot, start) = test_process_identity(live.id());
    edit_receipt(&config, |r| {
        r.child_pid = live.id();
        r.boot_identity = boot;
        r.child_start_identity = start;
    });
    assert!(
        matches!(reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "receipt identity or shape mismatch")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    live.kill().unwrap();
    live.wait().unwrap();
}
