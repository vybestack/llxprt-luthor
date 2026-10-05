use super::*;
use luthor::state::{journal, scheduling};

#[cfg(unix)]
pub(crate) fn detached_same_binary_dispatch_records_gate_and_worker_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let receipt = config.state_root.join("attempts/attempt-real.receipt.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if marker.exists() && receipt.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        marker.exists(),
        "released worker did not create its start marker"
    );
    assert!(
        receipt.exists(),
        "started worker did not produce its exit receipt"
    );
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(fs::read(receipt.stdout_path).unwrap(), b"worker stdout\n");
    assert_eq!(fs::read(receipt.stderr_path).unwrap(), b"worker stderr\n");
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    let reconciled = reconcile_attempt(&mut store, "task", "attempt-real").unwrap();
    assert_eq!(
        reconciled,
        Reconciliation::Completed {
            exit_code: Some(0),
            signal: None
        }
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    drop(store);
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(0),
            signal: None
        }
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    let kinds = journal::evidence_kinds(&store, "task").unwrap();
    assert!(kinds.contains(&"supervisor_ready".into()));
    assert!(kinds.contains(&"gate_sent".into()));
    assert!(
        execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).is_err()
    );
}
