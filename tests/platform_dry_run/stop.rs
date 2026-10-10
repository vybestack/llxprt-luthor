use super::*;
use luthor::WorktreeOwner;
use luthor::state::{exit_observation, scheduling, task_records};

pub(crate) fn run() {
    let fixture = NativeFixture::new();
    let (request_rx, server, profile) = start_provider(fixture.dir.path(), true);
    let (config, candidate) = fixture_config(fixture.dir.path(), &fixture.binary, &profile, true);
    let mut store = claimed_store(&config, &candidate);
    let plan = prepare_initial(&mut store, "task", "installed-stop").unwrap();
    let owner = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    execute_with_binary(
        &mut store,
        &plan,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    let config_root = &fixture.config_root;
    let dir = &fixture.dir;
    let request = request_rx.recv_timeout(Duration::from_secs(20));
    if request.is_err() {
        report_stop_request_failure(dir, &config, &store);
    }
    let request = request.expect("provider request");
    assert!(
        request.contains("/v1/chat/completions") || request.contains("/chat/completions"),
        "{request}"
    );
    let receipt = stop_and_pause(&mut store, &config);
    drop(store);
    let (mut reopened, resume_receipt) = execute_resume(&config, &plan);
    assert_resumed_turn(&plan, &request_rx, &mut reopened, &resume_receipt);
    let stdout = fs::metadata(&receipt.stdout_path).unwrap();
    let stderr = fs::metadata(&receipt.stderr_path).unwrap();
    assert_eq!(stdout.len(), receipt.stdout_bytes);
    assert_eq!(stderr.len(), receipt.stderr_bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(stdout.permissions().mode() & 0o777, 0o600);
        assert_eq!(stderr.permissions().mode() & 0o777, 0o600);
    }
    server.join().unwrap();
    assert!(
        fs::read_dir(config_root).unwrap().next().is_some(),
        "rs did not create session data under private config root"
    );
    assert!(!dir.path().join("gh-calls").exists());
}

fn stop_and_pause(store: &mut StateStore, config: &Config) -> ExitReceipt {
    luthor::supervisor::request_stop(store, "task", "installed-stop").unwrap();
    let receipt_path = config
        .state_root
        .join("attempts/installed-stop.receipt.json");
    wait_for_stop_receipt(&receipt_path);
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    assert!(
        !receipt.stop_signals.is_empty(),
        "supervisor sent no stop signals: {receipt:?}"
    );
    assert!(matches!(
        luthor::supervisor::reconcile_attempt(store, "task", "installed-stop").unwrap(),
        luthor::supervisor::Reconciliation::Completed { .. }
    ));
    assert_eq!(scheduling::reservation_count(store).unwrap(), 0);
    exit_observation::record_pause_pr_lookup(
        store,
        "task",
        "installed-stop",
        &luthor::state::PausePrEvidence {
            observed_at_unix_secs: 2,
            repository: "org/code".into(),
            status: luthor::state::PausePrStatus::Absent,
        },
    )
    .unwrap();
    assert_eq!(
        task_records::task_phase(store, "task").unwrap().as_deref(),
        Some("paused")
    );
    receipt
}
fn execute_resume(
    config: &Config,
    plan: &luthor::supervisor::LaunchPlan,
) -> (StateStore, ExitReceipt) {
    let mut reopened = StateStore::open(&config.state_root, 1).unwrap();
    let resume = prepare_resume(&mut reopened, "task", "installed-resume").unwrap();
    assert_eq!(resume.session_id, plan.session_id);
    assert_eq!(resume.worktree, plan.worktree);
    assert!(
        resume
            .args
            .join(" ")
            .contains("Distinct second turn after stop for installed-resume")
    );
    assert_eq!(scheduling::reservation_count(&reopened).unwrap(), 1);
    let owner = WorktreeOwner::acquire(reopened.root(), &plan.task_id).unwrap();
    execute_with_binary(
        &mut reopened,
        &resume,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    let resume_receipt_path = config
        .state_root
        .join("attempts/installed-resume.receipt.json");
    wait_for_stop_receipt(&resume_receipt_path);
    let resume_receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(&resume_receipt_path).unwrap()).unwrap();
    (reopened, resume_receipt)
}
fn assert_resumed_turn(
    plan: &luthor::supervisor::LaunchPlan,
    request_rx: &std::sync::mpsc::Receiver<String>,
    reopened: &mut StateStore,
    resume_receipt: &ExitReceipt,
) {
    assert_eq!(
        resume_receipt.exit_code,
        Some(0),
        "rs resume stdout tail:\n{}\nrs resume stderr tail:\n{}",
        stream_tail(&resume_receipt.stdout_path),
        stream_tail(&resume_receipt.stderr_path)
    );
    let resumed_request = request_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("resume provider request");
    assert!(
        resumed_request.contains("Distinct second turn after stop for installed-resume"),
        "{resumed_request}"
    );
    assert!(
        resumed_request.contains(&plan.session_id),
        "session id missing: {resumed_request}"
    );
    assert!(
        resumed_request.contains(&plan.worktree.display().to_string()),
        "worktree missing: {resumed_request}"
    );
    for (path, expected) in [
        (&resume_receipt.stdout_path, resume_receipt.stdout_bytes),
        (&resume_receipt.stderr_path, resume_receipt.stderr_bytes),
    ] {
        assert_eq!(fs::metadata(path).unwrap().len(), expected);
    }
    assert!(matches!(
        luthor::supervisor::reconcile_attempt(reopened, "task", "installed-resume").unwrap(),
        luthor::supervisor::Reconciliation::Completed { .. }
    ));
    assert_eq!(scheduling::reservation_count(reopened).unwrap(), 0);
}
