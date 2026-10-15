//! Whole daemon cycles with a worker publishing a PR during an active lookup.
use super::{Command, fs};
use luthor::{
    config::Config,
    state::{StateStore, journal, task_records},
};
use serde_json::Value;
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
mod active_finish;
mod fixture;
use fixture::Fixture;

#[derive(Clone, Copy, Debug)]
enum ExitTiming {
    BeforeCycle,
    DuringAbsentLookup,
    DuringOpenLookup,
}

const EXIT_TIMINGS: [ExitTiming; 3] = [
    ExitTiming::BeforeCycle,
    ExitTiming::DuringAbsentLookup,
    ExitTiming::DuringOpenLookup,
];

fn await_exit(f: &Fixture, task: &str, attempt: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if f.config
            .state_root
            .join(format!("attempts/{attempt}.receipt.json"))
            .exists()
        {
            match luthor::WorktreeOwner::acquire_existing(&f.config.state_root, task) {
                Ok(owner) => {
                    drop(owner);
                    break;
                }
                Err(luthor::OwnershipError::Busy) => {}
                Err(luthor::OwnershipError::Unavailable) => panic!("owner unavailable"),
            }
        }
        assert!(Instant::now() < deadline, "worker did not exit");
        thread::sleep(Duration::from_millis(10));
    }
}

fn latest(f: &Fixture, task: &str) -> String {
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    task_records::latest_attempt(&store, task).unwrap().unwrap()
}

fn assert_complete(f: &Fixture, task: &str, attempt: &str, repair: bool) {
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        task_records::task_phase(&store, task).unwrap().as_deref(),
        Some("pr_complete")
    );
    assert_eq!(
        luthor::state::scheduling::reservation_count(&store).unwrap(),
        0
    );
    assert!(
        luthor::state::scheduling::pending_attempts(&store)
            .unwrap()
            .is_empty()
    );
    let db = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    let outcome: String = db
        .query_row(
            "SELECT outcome FROM attempts WHERE id=?1",
            [attempt],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "exit_code=Some(0);signal=None");
    let exit: i64 = db
        .query_row(
            "SELECT sequence FROM evidence WHERE attempt_id=?1 AND kind='attempt_exit'",
            [attempt],
            |row| row.get(0),
        )
        .unwrap();
    let lookup: i64 = db
        .query_row(
            "SELECT sequence FROM evidence WHERE attempt_id=?1 AND kind='exit_pr_lookup'",
            [attempt],
            |row| row.get(0),
        )
        .unwrap();
    let verified: i64 = db
        .query_row(
            "SELECT sequence FROM evidence WHERE attempt_id=?1 AND kind='verified_open_pr'",
            [attempt],
            |row| row.get(0),
        )
        .unwrap();
    assert!(exit < lookup && lookup < verified);
    if repair {
        let result: i64 = db
            .query_row(
                "SELECT sequence FROM evidence WHERE attempt_id=?1 AND kind='pr_repair_result'",
                [attempt],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exit < result && result < lookup);
    }
    assert_completion_tip(f, task, attempt, &store);
}

fn assert_completion_tip(f: &Fixture, task: &str, attempt: &str, store: &StateStore) {
    let proof: Value = serde_json::from_str(
        &journal::evidence_payloads(store, task, attempt, "verified_open_pr").unwrap()[0],
    )
    .unwrap();
    let remote_tip = Command::new("git")
        .arg("--git-dir")
        .arg(f.root.path().join("remote.git"))
        .args(["rev-parse", proof["head_branch"].as_str().unwrap()])
        .output()
        .unwrap();
    assert!(remote_tip.status.success());
    assert_eq!(
        proof["head_commit_sha"],
        String::from_utf8(remote_tip.stdout).unwrap().trim()
    );
    assert_eq!(proof["id"], 4242);
    let local = luthor::worktree::verify_existing_worktree(store, task).unwrap();
    assert_eq!(proof["head_commit_sha"], local.head);
    assert_ne!(Some(local.head), local.base_commit);
    assert_eq!(proof["attempt_id"], attempt);
    assert!(
        journal::stop_intent(store, task, attempt)
            .unwrap()
            .is_none()
    );
    for kind in ["attempt_exit", "exit_pr_lookup", "verified_open_pr"] {
        assert_eq!(
            journal::evidence_payloads(store, task, attempt, kind)
                .unwrap()
                .len(),
            1
        );
    }
    let calls = fs::read_to_string(f.root.path().join("calls")).unwrap();
    assert_eq!(
        calls
            .matches("POST repos/org/tracker/issues/7/assignees")
            .count(),
        1
    );
}

#[test]
fn daemon_completes_retry_with_pr_created_during_attempt_without_manual_reconcile() {
    for timing in EXIT_TIMINGS {
        let f = Fixture::new();
        let task = f.dispatch();
        let previous = latest(&f, &task);
        await_exit(&f, &task, &previous);
        f.cycle();
        f.enable_worker();
        let result = f.command(
            "retry",
            &[
                &task,
                "--attempt",
                &previous,
                "--config-revision",
                "retry",
                "--actor",
                "acoliver",
                "--reason",
                "finish issue",
                "--execute",
            ],
        );
        let attempt = result["attempt_id"].as_str().unwrap();
        f.finish_during_cycle(&task, attempt, timing);
        let report = f.cycle();
        assert_eq!(
            report["reconciliation"][0]["status"],
            "completed",
            "{}\n{}",
            report,
            f.state_evidence()
        );
        assert_complete(&f, &task, attempt, false);
        let store = StateStore::open(&f.config.state_root, 1).unwrap();
        let stale: Value = serde_json::from_str(
            &journal::evidence_payloads(&store, &task, attempt, "retry_pr_lookup").unwrap()[0],
        )
        .unwrap();
        assert_eq!(stale["status"]["status"], "absent");
        drop(store);
        f.cycle();
        assert_complete(&f, &task, attempt, false);
    }
}

#[test]
fn daemon_completes_repair_pushed_to_existing_pr_without_manual_reconcile() {
    for timing in EXIT_TIMINGS {
        let f = Fixture::new();
        let task = f.dispatch();
        let previous = latest(&f, &task);
        await_exit(&f, &task, &previous);
        f.publish_pr(&task);
        f.cycle();
        assert_eq!(
            task_records::task_phase(&StateStore::open(&f.config.state_root, 1).unwrap(), &task)
                .unwrap()
                .as_deref(),
            Some("pr_complete"),
            "{}",
            f.state_evidence()
        );
        fs::write(f.root.path().join("conflict"), "conflicting").unwrap();
        // Model the next CI-relevant interval without changing any timeout.
        let db = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
        db.execute("UPDATE pr_polls SET observed_at=0", []).unwrap();
        drop(db);
        f.enable_worker();
        let launched = f.cycle();
        assert!(
            launched["launched"][0]["attempt_id"].is_string(),
            "{launched}"
        );
        let attempt = launched["launched"][0]["attempt_id"].as_str().unwrap();
        f.finish_during_cycle(&task, attempt, timing);
        let report = f.cycle();
        assert_eq!(
            report["reconciliation"][0]["status"],
            "completed",
            "{}\n{}",
            report,
            f.state_evidence()
        );
        assert_complete(&f, &task, attempt, true);
        let store = StateStore::open(&f.config.state_root, 1).unwrap();
        assert_eq!(
            journal::evidence_payloads(&store, &task, &previous, "verified_open_pr")
                .unwrap()
                .len(),
            1
        );
        assert!(
            journal::stop_intent(&store, &task, attempt)
                .unwrap()
                .is_none()
        );
        drop(store);
        f.cycle();
        assert_complete(&f, &task, attempt, true);
    }
}
