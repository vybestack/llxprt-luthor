use super::*;
use luthor::{
    WorktreeOwner,
    state::{journal, scheduling},
};

#[cfg(unix)]
pub(crate) fn reaped_supervisor_with_removed_receipt_proves_recovery_quiescence() {
    let (_dir, config, store) = dispatched_fixture(7);
    let payloads =
        journal::evidence_payloads(&store, "task", "attempt-real", "supervisor_ready").unwrap();
    assert_eq!(payloads.len(), 1);
    let identity: serde_json::Value = serde_json::from_str(&payloads[0]).unwrap();
    let supervisor_pid = identity["pid"].as_u64().expect("supervisor PID");
    let supervisor_pid = libc::pid_t::try_from(supervisor_pid).unwrap();

    wait_for_process_and_group_absence(supervisor_pid);
    fs::remove_file(receipt_path(&config)).unwrap();

    assert_eq!(
        inspect_recovery_quiescence(&store, "task", "attempt-real").unwrap(),
        RecoveryInspection::Quiescent
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

#[cfg(unix)]
pub(crate) fn resumed_missing_receipt_accepts_attempt_snapshot_descending_from_original() {
    let (_dir, config, mut store, initial) = paused_fixture();
    let worktree = &initial.worktree;
    fs::write(worktree.join("recovery-advance"), "committed").unwrap();
    git(worktree, &["add", "recovery-advance"]);
    git(worktree, &["commit", "-m", "advance before resume"]);

    let resumed = prepare_resume(&mut store, "task", "attempt-next").unwrap();
    assert_ne!(
        resumed.expected_worktree.head,
        initial.expected_worktree.head
    );
    let owner = WorktreeOwner::acquire(store.root(), &resumed.task_id).unwrap();
    execute_with_binary(
        &mut store,
        &resumed,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    let receipt = config.state_root.join("attempts/attempt-next.receipt.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !receipt.exists() {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(receipt.exists(), "resumed worker did not finish");

    let payload = journal::evidence_payloads(&store, "task", "attempt-next", "supervisor_ready")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    wait_for_process_and_group_absence(pid);
    fs::remove_file(receipt).unwrap();
    assert_eq!(
        inspect_recovery_quiescence(&store, "task", "attempt-next").unwrap(),
        RecoveryInspection::Quiescent
    );

    git(
        worktree,
        &["reset", "--hard", &initial.expected_worktree.head],
    );
    assert!(matches!(
        inspect_recovery_quiescence(&store, "task", "attempt-next").unwrap(),
        RecoveryInspection::Held("worktree snapshot mismatch")
    ));
    git(worktree, &["checkout", "--orphan", "replacement"]);
    git(worktree, &["rm", "-rf", "."]);
    fs::write(worktree.join("replacement"), "unrelated history").unwrap();
    git(worktree, &["add", "replacement"]);
    git(worktree, &["commit", "-m", "unrelated replacement history"]);
    assert!(matches!(
        inspect_recovery_quiescence(&store, "task", "attempt-next").unwrap(),
        RecoveryInspection::Held(_)
    ));
}
