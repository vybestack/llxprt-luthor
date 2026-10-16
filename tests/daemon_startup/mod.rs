//! Startup barriers must identify every failed attempt and unresolved source
//! without dumping private intent details or changing fail-closed scheduling.
use super::{fixture, run, tempdir};
use luthor::{config::Config, state::StateStore};
use rusqlite::params;
use serde_json::{Value, json};
use std::{fs, path::Path};

const PRIVATE_DETAIL: &str = "private-source-intent-must-not-be-printed";

fn seed_attempt(store: &StateStore, task: &str, attempt: &str, stop: bool) {
    let db = rusqlite::Connection::open(store.root().join("state.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision)
         VALUES(?1,'REPO_NODE',?1,'org/tracker',7,'held','test-revision')",
        [task],
    ).unwrap();
    db.execute(
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES(?1,?2,'launch_intended')",
        params![attempt, task],
    )
    .unwrap();
    db.execute(
        "INSERT INTO reservations(attempt_id,task_id,status) VALUES(?1,?2,'reserved')",
        params![attempt, task],
    )
    .unwrap();
    if stop {
        db.execute(
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'stop',?4)",
            params![format!("stop-{task}"), task, attempt, PRIVATE_DETAIL],
        )
        .unwrap();
    }
}

fn open_store(config: &str) -> StateStore {
    let config = Config::from_json(&fs::read_to_string(config).unwrap()).unwrap();
    StateStore::open(&config.state_root, config.capacity).unwrap()
}

fn blocked_report(config: &str, path: &str, root: &Path) -> Value {
    fs::write(root.join("calls"), "").unwrap();
    let output = run(config, path, &["--issues", "7", "--execute"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "luthor: startup reconciliation blocked scheduling"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains(PRIVATE_DETAIL));
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["mode"], "execute");
    assert_eq!(report["blocked"], true);
    assert_eq!(
        fs::read_to_string(root.join("calls")).unwrap(),
        "api rate_limit --include\n"
    );
    assert!(!root.join("assigned").exists());
    assert!(!root.join("worktrees").exists());
    report
}

#[test]
fn blocked_startup_reports_all_attempt_errors_and_source_hold_kinds() {
    let root = tempdir();
    let (config, path) = fixture(root.path(), "acoliver");
    let store = open_store(&config);
    // Invalid durable attempt identities exercise actual AttemptReview::Error
    // values, not a synthetic JSON serializer or a generic top-level failure.
    seed_attempt(&store, "task-first", "invalid/first", false);
    seed_attempt(&store, "task-second", "invalid/second", true);
    drop(store);
    let report = blocked_report(&config, &path, root.path());
    assert_eq!(
        report["reconciliation"],
        json!([
            {"task_id":"task-first","attempt_id":"invalid/first","status":"error",
             "error":"launch plan does not match verified task or worktree"},
            {"task_id":"task-second","attempt_id":"invalid/second","status":"error",
             "error":"launch plan does not match verified task or worktree"}
        ])
    );
    assert_eq!(
        report["source_holds"],
        json!([{"task_id":"task-second","kind":"stop"}])
    );
    let store = open_store(&config);
    assert_eq!(
        luthor::state::scheduling::reservation_count(&store).unwrap(),
        2
    );
    assert_eq!(
        luthor::state::scheduling::pending_attempts(&store)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn source_stop_alone_reports_the_barrier_without_fabricating_an_attempt_error() {
    let root = tempdir();
    let (config, path) = fixture(root.path(), "acoliver");
    let store = open_store(&config);
    seed_attempt(&store, "task-stopped", "attempt-stopped", true);
    drop(store);
    let report = blocked_report(&config, &path, root.path());
    assert_eq!(
        report["reconciliation"],
        json!([
            {"task_id":"task-stopped","attempt_id":"attempt-stopped","status":"held"}
        ])
    );
    assert_eq!(
        report["source_holds"],
        json!([{"task_id":"task-stopped","kind":"stop"}])
    );
    assert_eq!(
        luthor::state::scheduling::reservation_count(&open_store(&config)).unwrap(),
        1
    );
}
