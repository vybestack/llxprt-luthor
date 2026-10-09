use super::*;
use luthor::state::{journal, scheduling, task_records};

#[cfg(unix)]
pub(crate) fn stop_term_has_durable_intent_and_keeps_slot_until_reconcile() {
    let (_dir, config, mut store) = running_worker(
        "#!/bin/sh\necho started\ntrap 'exit 0' INT\ntrap 'exit 0' TERM\nwhile :; do sleep 0.01; done\n",
    );
    request_stop(&mut store, "task", "attempt-real").unwrap();
    assert!(
        journal::stop_intent(&store, "task", "attempt-real")
            .unwrap()
            .is_some()
    );
    let receipt = stopped_receipt(&config);
    assert_eq!(receipt.stop_signals.first(), Some(&libc::SIGINT));
    assert!(receipt.stop_signals.len() <= 2);
    assert!(
        receipt
            .stop_signals
            .iter()
            .all(|signal| *signal != libc::SIGKILL)
    );
    assert!(
        receipt
            .stop_signals
            .windows(2)
            .all(|signals| { signals == [libc::SIGINT, libc::SIGTERM] })
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    drop(store);
    let mut reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        task_records::task_phase(&reopened, "task")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert_eq!(scheduling::reservation_count(&reopened).unwrap(), 0);
    assert!(matches!(
        reconcile_attempt(&mut reopened, "task", "attempt-real").unwrap(),
        Reconciliation::Completed { .. }
    ));
}

#[cfg(unix)]
pub(crate) fn stop_escalates_only_on_live_matching_child() {
    let (_dir, config, mut store) =
        running_worker("#!/bin/sh\necho started\ntrap '' INT TERM\nwhile :; do :; done\n");
    request_stop(&mut store, "task", "attempt-real").unwrap();
    let receipt = stopped_receipt(&config);
    assert_eq!(
        receipt.stop_signals,
        vec![libc::SIGINT, libc::SIGTERM, libc::SIGKILL]
    );
    assert_eq!(receipt.signal, Some(libc::SIGKILL));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed { .. }
    ));
}

#[cfg(unix)]
pub(crate) fn absent_supervisor_and_wrong_identity_never_signal_unrelated_process() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, _plan, _) = prepared_fake_worker(&dir);
    let mut unrelated = Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    assert!(request_stop(&mut store, "task", "attempt-real").is_err());
    assert!(
        journal::stop_intent(&store, "task", "attempt-real")
            .unwrap()
            .is_some()
    );
    assert!(unrelated.try_wait().unwrap().is_none());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(!receipt_path(&config).exists());
    let (boot, _) = test_process_identity(unrelated.id());
    let fake =
        serde_json::json!({"pid":unrelated.id(),"boot_identity":boot,"start_identity":"wrong"});
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "supervisor_ready",
        &fake.to_string(),
    )
    .unwrap();
    assert!(request_stop(&mut store, "task", "attempt-real").is_err());
    assert!(unrelated.try_wait().unwrap().is_none());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { .. }
    ));
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}
