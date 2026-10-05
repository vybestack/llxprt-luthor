use super::*;
use luthor::state::{exit_observation, journal, launches, scheduling, task_records};

#[cfg(unix)]
pub(crate) fn paused_fixture() -> (
    tempfile::TempDir,
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
) {
    paused_fixture_with_resume_prompt("Continue {task.issue_url} for {attempt.id}")
}

#[cfg(unix)]
pub(crate) fn paused_fixture_with_resume_prompt(
    resume_prompt: &str,
) -> (
    tempfile::TempDir,
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
) {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker_with_resume_prompt(&dir, resume_prompt);
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let receipt = config.state_root.join("attempts/attempt-real.receipt.json");
    for _ in 0..200 {
        if receipt.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(receipt.exists());
    let initial = serde_json::from_str(
        &launches::launch_intent(&store, "attempt-real")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    journal::record_stop_intent(&mut store, "task", "attempt-real").unwrap();
    edit_receipt(&config, |receipt| {
        receipt.stop_signals = vec![libc::SIGTERM]
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last_reason = String::from("worker process group is still running");
    loop {
        match reconcile_attempt(&mut store, "task", "attempt-real").unwrap() {
            Reconciliation::Completed { .. } => break,
            Reconciliation::Held { reason } => {
                assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
                last_reason = reason;
            }
            Reconciliation::Running => {
                assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker reconciliation remained uncertain: {last_reason}"
        );
        thread::sleep(Duration::from_millis(20));
    }
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
    (dir, config, store, initial)
}
