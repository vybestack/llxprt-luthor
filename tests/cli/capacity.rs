//! Issue 116: resizing observes a real dispatched worker, without reconciling it.
use super::harness::Harness;
use luthor::{config::Config, state::StateStore};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::fs;

fn db(h: &Harness) -> Connection {
    Connection::open(h.state.join("state.sqlite3")).unwrap()
}

fn resize(h: &Harness, expected: usize, new: usize, reservations: usize) -> std::process::Output {
    let mut config = Config::from_json(&fs::read_to_string(&h.config).unwrap()).unwrap();
    config.capacity = new;
    let path = h._dir.path().join("resize.json");
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    h.run(&[
        "resize-capacity",
        "--config",
        path.to_str().unwrap(),
        "--expected-capacity",
        &expected.to_string(),
        "--expected-reservations",
        &reservations.to_string(),
        "--actor",
        "operator",
        "--reason",
        "grow a busy queue",
        "--execute",
    ])
}

fn history(h: &Harness) -> Vec<String> {
    [
        "tasks",
        "attempts",
        "reservations",
        "intents",
        "evidence",
        "launch_preparations",
    ]
    .iter()
    .map(|table| {
        let db = db(h);
        let mut statement = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let count = statement.column_count();
        let rows: Vec<Vec<rusqlite::types::Value>> = statement
            .query_map([], |row| (0..count).map(|i| row.get(i)).collect())
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        format!("{rows:?}")
    })
    .collect()
}

fn assert_refused(h: &Harness, expected: usize, new: usize, reservations: usize) {
    let before = history(h);
    let output = resize(h, expected, new, reservations);
    assert!(!output.status.success(), "accepted resize: {:?}", output);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "refused");
    assert_eq!(history(h), before);
    let audits: i64 = db(h)
        .query_row("SELECT COUNT(*) FROM capacity_changes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(audits, 0);
    drop(StateStore::open(&h.state, 2).unwrap());
}

#[test]
fn increase_counts_confirmed_running_attempt_and_preserves_reservation_history_and_owner() {
    let h = Harness::new();
    h.seed_running_task();
    let before = history(&h);
    let owner = h.state.join("worktree-task.lock");
    let bytes = fs::read(&owner).unwrap();
    let output = resize(&h, 2, 3, 1);
    // Stop before asserting so a red regression does not leave a worker behind.
    let after = history(&h);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let limit: usize = db(&h)
        .query_row(
            "SELECT value FROM state_meta WHERE key='capacity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    stop(&h, limit);
    assert!(output.status.success(), "{result}");
    assert_eq!(after, before);
    assert_eq!(fs::read(owner).unwrap(), bytes);
    assert_eq!(result["change"]["reservations"], 1);
    assert_eq!(result["change"]["permanent_holds"], json!([]));
    let counted = json!([{"task_id":"task", "attempt_id":"running-attempt"}]);
    assert_eq!(result["change"]["running_attempts"], counted);
    let saved: String = db(&h)
        .query_row("SELECT running_attempts FROM capacity_changes", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), counted);
    assert!(!h.log.exists(), "resize must not call GitHub");
}

#[test]
fn running_attempt_does_not_authorize_decreases_or_stale_expectations() {
    let h = Harness::new();
    h.seed_running_task();
    assert_refused(&h, 2, 1, 1);
    assert_refused(&h, 2, 3, 0);
    assert_refused(&h, 1, 3, 1);
    h.stop_running_task();
}

#[test]
fn in_progress_and_unreconciled_launches_still_refuse_increases() {
    for mutation in [
        "DELETE FROM evidence WHERE kind='gate_sent'",
        "DELETE FROM intents WHERE kind='gate_release'",
        "UPDATE intents SET detail='{}' WHERE kind='supervisor_dispatch'",
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('stop','task','running-attempt','stop','{}')",
    ] {
        let h = Harness::new();
        h.seed_running_task();
        db(&h).execute_batch("CREATE TABLE saved_intents AS SELECT * FROM intents; CREATE TABLE saved_evidence AS SELECT * FROM evidence;").unwrap();
        db(&h).execute_batch(mutation).unwrap();
        assert_refused(&h, 2, 3, 1);
        // Restore exact original evidence only for identity-safe fixture shutdown.
        db(&h).execute_batch("BEGIN; PRAGMA defer_foreign_keys=ON; DELETE FROM intents; INSERT INTO intents SELECT * FROM saved_intents; DELETE FROM evidence; INSERT INTO evidence SELECT * FROM saved_evidence; COMMIT;").unwrap();
        h.stop_running_task();
    }
}

fn stop(h: &Harness, capacity: usize) {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let mut store = StateStore::open(&h.state, capacity).unwrap();
    luthor::supervisor::request_stop(&mut store, "task", "running-attempt").unwrap();
    let receipt = h.state.join("attempts/running-attempt.receipt.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !receipt.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(receipt.exists(), "cooperative child did not exit");
}

#[test]
fn confirmed_worker_and_permanent_historical_hold_are_counted_separately() {
    let h = Harness::new();
    h.seed_running_task();
    db(&h).execute_batch(
        "INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision)
         VALUES('legacy','repo','legacy-issue','org/tracker',9,'held','old');
         INSERT INTO attempts(id,task_id,lifecycle) VALUES('legacy-attempt','legacy','launch_intended');
         INSERT INTO reservations(attempt_id,task_id,status) VALUES('legacy-attempt','legacy','reserved');
         INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('legacy-launch','legacy','legacy-attempt','launch','original');",
    ).unwrap();
    drop(luthor::WorktreeOwner::acquire(&h.state, "legacy").unwrap());
    let before = history(&h);
    let output = resize(&h, 2, 3, 2);
    let after = history(&h);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let capacity = if output.status.success() { 3 } else { 2 };
    stop(&h, capacity);
    assert!(output.status.success(), "{result}");
    assert_eq!(before, after);
    assert_eq!(result["change"]["reservations"], 2);
    assert_eq!(
        result["change"]["running_attempts"],
        json!([{"task_id":"task", "attempt_id":"running-attempt"}])
    );
    assert_eq!(result["change"]["permanent_holds"][0]["task_id"], "legacy");
    assert_eq!(
        result["change"]["permanent_holds"][0]["attempt_id"],
        "legacy-attempt"
    );
    assert_eq!(
        result["change"]["permanent_holds"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn running_audit_and_capacity_remain_atomic_on_write_failure() {
    let h = Harness::new();
    h.seed_running_task();
    db(&h).execute_batch("CREATE TRIGGER reject_capacity BEFORE UPDATE ON state_meta BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    assert_refused(&h, 2, 3, 1);
    h.stop_running_task();
}

#[test]
fn finished_worker_with_unreconciled_receipt_cannot_be_counted_as_running() {
    let h = Harness::new();
    h.seed_running_task();
    h.stop_running_task();
    // Leave the durable reservation held and remove stop as an independent
    // refusal reason: the receipt itself must require reconciliation.
    db(&h)
        .execute("DELETE FROM intents WHERE kind='stop'", [])
        .unwrap();
    assert_refused(&h, 2, 3, 1);
}

#[test]
fn running_proof_requires_exact_owner_and_process_registrations() {
    for mutation in [
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,NULL,kind,payload FROM evidence WHERE kind='supervisor_ready'",
        "UPDATE evidence SET payload=json_set(payload,'$.nonce','wrong') WHERE kind='worktree_owner_protocol'",
        "UPDATE evidence SET payload=json_set(payload,'$.start_identity','wrong') WHERE kind='child_registered'",
        "UPDATE evidence SET payload='{}' WHERE kind='supervisor_ready'",
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='gate_sent'",
    ] {
        let h = Harness::new();
        h.seed_running_task();
        db(&h)
            .execute_batch("CREATE TABLE saved_evidence AS SELECT * FROM evidence;")
            .unwrap();
        db(&h).execute_batch(mutation).unwrap();
        assert_refused(&h, 2, 3, 1);
        db(&h)
            .execute_batch(
                "DELETE FROM evidence; INSERT INTO evidence SELECT * FROM saved_evidence;",
            )
            .unwrap();
        h.stop_running_task();
    }
}

#[test]
fn missing_live_owner_is_not_recreated_for_capacity_increase() {
    let h = Harness::new();
    h.seed_running_task();
    let path = h.state.join("worktree-task.lock");
    let saved = h.state.join("saved-owner.lock");
    fs::rename(&path, &saved).unwrap();
    assert_refused(&h, 2, 3, 1);
    assert!(!path.exists());
    fs::rename(saved, path).unwrap();
    h.stop_running_task();
}
