//! The PR may be published several execute cycles before the worker's summary.
use super::{Fixture, assert_complete, await_exit, latest};
use luthor::{
    state::{PausePrEvidence, PausePrStatus, StateStore, exit_observation, journal, task_records},
    supervisor::{self, Reconciliation},
};
use serde_json::{Value, json};
use std::{
    fs, thread,
    time::{Duration, Instant},
};

fn published_worker() -> (Fixture, String, String) {
    let f = Fixture::new();
    f.enable_worker();
    fs::write(f.root.path().join("finish-wait"), "wait").unwrap();
    fs::write(f.root.path().join("release"), "publish").unwrap();
    let task = f.dispatch();
    let attempt = latest(&f, &task);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(bytes) = fs::read(f.root.path().join("pr.json"))
            && serde_json::from_slice::<Value>(&bytes).is_ok()
        {
            break;
        }
        assert!(Instant::now() < deadline, "worker did not publish PR");
        thread::sleep(Duration::from_millis(10));
    }
    (f, task, attempt)
}

fn assert_running(f: &Fixture, task: &str, attempt: &str) {
    let report = f.cycle();
    assert_eq!(report["reconciliation"][0]["status"], "running", "{report}");
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        luthor::state::scheduling::reservation_count(&store).unwrap(),
        1
    );
    assert!(
        journal::stop_intent(&store, task, attempt)
            .unwrap()
            .is_none()
    );
    assert!(
        journal::evidence_payloads(&store, task, attempt, "attempt_exit")
            .unwrap()
            .is_empty()
    );
    assert!(
        journal::evidence_payloads(&store, task, attempt, "verified_open_pr")
            .unwrap()
            .is_empty()
    );
    let observations = journal::evidence_payloads(&store, task, attempt, "active_open_pr").unwrap();
    let proof: Value = serde_json::from_str(observations.last().unwrap()).unwrap();
    assert_eq!(proof["attempt_id"], attempt);
    assert_eq!(proof["id"], 4242);
    assert_eq!(
        proof["head_commit_sha"],
        fs::read_to_string(f.root.path().join("head")).unwrap()
    );
}

#[test]
fn daemon_lets_published_pr_worker_sleep_across_cycles_and_exit_without_stop() {
    let (f, task, attempt) = published_worker();
    assert_running(&f, &task, &attempt);
    assert_running(&f, &task, &attempt);
    fs::write(f.root.path().join("finish-release"), "finish").unwrap();
    await_exit(&f, &task, &attempt);
    let receipt: supervisor::ExitReceipt = serde_json::from_slice(
        &fs::read(
            f.config
                .state_root
                .join(format!("attempts/{attempt}.receipt.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(receipt.stop_signals.is_empty());
    assert_eq!(receipt.exit_code, Some(0));
    assert!(
        fs::read_to_string(receipt.stdout_path)
            .unwrap()
            .contains("{\"summary\":\"worker finished\"}")
    );
    assert_eq!(f.cycle()["reconciliation"][0]["status"], "completed");
    assert_complete(&f, &task, &attempt, false);
    f.cycle();
    assert_complete(&f, &task, &attempt, false);
}

fn change_pr(f: &Fixture, conflict: &str) {
    let path = f.root.path().join("pr.json");
    let mut pr: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    match conflict {
        "branch" => pr["head"]["ref"] = json!("another-task"),
        "tip" => pr["head"]["sha"] = json!("1234567890abcdef1234567890abcdef12345678"),
        "closing" => {
            pr["body"] = json!("Tracker-Issue: https://github.com/org/tracker/issues/7\nFixes #7")
        }
        "author" => pr["user"]["login"] = json!("someone-else"),
        "repository" => pr["head"]["repo"] = json!({"id":11,"full_name":"other/code"}),
        "ambiguous" => fs::write(f.root.path().join("ambiguous"), "two PRs").unwrap(),
        _ => panic!("unknown conflict"),
    }
    fs::write(path, pr.to_string()).unwrap();
}

#[test]
fn daemon_holds_conflicting_active_prs_without_accepting_completion() {
    for conflict in [
        "branch",
        "tip",
        "author",
        "repository",
        "closing",
        "ambiguous",
    ] {
        let (f, task, attempt) = published_worker();
        change_pr(&f, conflict);
        let report = f.cycle();
        assert_eq!(
            report["reconciliation"][0]["status"], "held",
            "{conflict}: {report}"
        );
        let mut store = StateStore::open(&f.config.state_root, 1).unwrap();
        assert_eq!(
            task_records::task_phase(&store, &task).unwrap().as_deref(),
            Some("held")
        );
        assert!(
            journal::evidence_payloads(&store, &task, &attempt, "active_open_pr")
                .unwrap()
                .is_empty()
        );
        assert!(
            journal::evidence_payloads(&store, &task, &attempt, "verified_open_pr")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            luthor::state::scheduling::reservation_count(&store).unwrap(),
            1
        );
        // Explicit test cleanup, not a daemon stop decision.
        supervisor::request_stop(&mut store, &task, &attempt).unwrap();
        drop(store);
        await_exit(&f, &task, &attempt);
    }
}

fn legacy_pause_hold(f: &Fixture, task: &str, attempt: &str) {
    let mut store = StateStore::open(&f.config.state_root, 1).unwrap();
    journal::record_evidence(
        &mut store,
        task,
        Some(attempt),
        "held_reason",
        "matching PR observed while worker active",
    )
    .unwrap();
    supervisor::request_stop(&mut store, task, attempt).unwrap();
    drop(store);
    await_exit(f, task, attempt);
    let mut store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert!(matches!(
        supervisor::reconcile_attempt(&mut store, task, attempt).unwrap(),
        Reconciliation::Completed { .. }
    ));
    exit_observation::record_pause_pr_lookup(
        &mut store,
        task,
        attempt,
        &PausePrEvidence {
            observed_at_unix_secs: 1,
            repository: "org/code".into(),
            status: PausePrStatus::Open,
        },
    )
    .unwrap();
    assert_eq!(
        task_records::held_reason(&store, task).unwrap().as_deref(),
        Some("pause PR present")
    );
}

#[test]
fn daemon_restores_legacy_pause_pr_present_hold_with_fresh_exact_tip_proof() {
    let (f, task, attempt) = published_worker();
    legacy_pause_hold(&f, &task, &attempt);
    assert_eq!(f.cycle()["reconciliation"][0]["status"], "completed");
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        task_records::task_phase(&store, &task).unwrap().as_deref(),
        Some("pr_complete")
    );
    assert!(luthor::state::scheduling::ensure_dispatch_capacity(&store).is_ok());
    assert!(
        luthor::state::scheduling::pending_attempts(&store)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        journal::evidence_payloads(&store, &task, &attempt, "verified_open_pr")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        journal::evidence_payloads(&store, &task, &attempt, "pause_pr_lookup")
            .unwrap()
            .len(),
        2
    );
    drop(store);
    f.cycle();
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        journal::evidence_payloads(&store, &task, &attempt, "verified_open_pr")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn daemon_retains_legacy_pause_hold_for_conflicting_prs() {
    for conflict in [
        "branch",
        "tip",
        "author",
        "repository",
        "closing",
        "ambiguous",
    ] {
        let (f, task, attempt) = published_worker();
        legacy_pause_hold(&f, &task, &attempt);
        change_pr(&f, conflict);
        let report = f.cycle();
        assert_eq!(
            report["reconciliation"][0]["status"], "held",
            "{conflict}: {report}"
        );
        let store = StateStore::open(&f.config.state_root, 1).unwrap();
        assert_eq!(
            task_records::task_phase(&store, &task).unwrap().as_deref(),
            Some("held")
        );
        assert!(luthor::state::scheduling::ensure_dispatch_capacity(&store).is_err());
        assert!(
            journal::evidence_payloads(&store, &task, &attempt, "verified_open_pr")
                .unwrap()
                .is_empty()
        );
    }
}
