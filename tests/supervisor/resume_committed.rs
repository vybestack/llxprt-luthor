use super::*;
use luthor::WorktreeOwner;
use luthor::state::{exit_observation, task_records};

#[cfg(unix)]
pub(crate) fn committed_worker_resumes_same_session_and_worktree_after_verified_stop() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, first, _) = prepared_fake_worker(&dir);
    fs::write(
        &first.executable,
        r#"#!/bin/sh
case "$*" in
  *attempt-real*)
    echo committed > committed-file
    git add committed-file
    git commit -m 'worker commit' >/dev/null || exit 2
    echo committed-and-running
    trap 'exit 0' INT TERM
    while :; do :; done
    ;;
  *attempt-next*)
    test "$(cat committed-file)" = committed || exit 3
    echo resumed-same-worktree
    ;;
  *) exit 4;;
esac
"#,
    )
    .unwrap();
    let owner = WorktreeOwner::acquire(store.root(), &first.task_id).unwrap();
    execute_with_binary(
        &mut store,
        &first,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    let stdout = config.state_root.join("attempts/attempt-real.stdout.log");
    for _ in 0..200 {
        if fs::read_to_string(&stdout).is_ok_and(|s| s.contains("committed-and-running")) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        fs::read_to_string(&stdout)
            .unwrap()
            .contains("committed-and-running")
    );
    request_stop(&mut store, "task", "attempt-real").unwrap();
    assert!(!stopped_receipt(&config).stop_signals.is_empty());
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed { .. }
    ));
    exit_observation::record_pause_pr_lookup(
        &mut store,
        "task",
        "attempt-real",
        &luthor::state::PausePrEvidence {
            observed_at_unix_secs: 2,
            repository: "org/code".into(),
            status: luthor::state::PausePrStatus::Absent,
        },
    )
    .unwrap();
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("paused")
    );
    drop(store);

    assert_committed_resume(&config, &first);
}

#[cfg(unix)]
fn assert_committed_resume(config: &Config, first: &luthor::supervisor::LaunchPlan) {
    let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
    let next = prepare_resume(&mut store, "task", "attempt-next").unwrap();
    assert_eq!(next.session_id, first.session_id);
    assert_eq!(next.worktree, first.worktree);
    assert_eq!(
        task_records::latest_attempt(&store, "task")
            .unwrap()
            .as_deref(),
        Some("attempt-next")
    );
    assert_ne!(next.expected_worktree.head, first.expected_worktree.head);
    let owner = WorktreeOwner::acquire(store.root(), &next.task_id).unwrap();
    execute_with_binary(
        &mut store,
        &next,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    let receipt = config.state_root.join("attempts/attempt-next.receipt.json");
    for _ in 0..200 {
        if receipt.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert_eq!(
        receipt.exit_code,
        Some(0),
        "stdout: {}; stderr: {}",
        fs::read_to_string(&receipt.stdout_path).unwrap(),
        fs::read_to_string(&receipt.stderr_path).unwrap()
    );
    assert!(
        fs::read_to_string(receipt.stdout_path)
            .unwrap()
            .contains("resumed-same-worktree")
    );
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-next").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(0),
            ..
        }
    ));
}
