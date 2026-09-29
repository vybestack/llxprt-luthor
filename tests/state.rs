#[test]
fn migrates_v1_database_atomically_and_preserves_records_and_capacity() {
    use rusqlite::Connection;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.sqlite3");
    let connection = Connection::open(&db).unwrap();
    connection.execute_batch("CREATE TABLE tasks (id TEXT PRIMARY KEY, tracker_repo_id TEXT NOT NULL, issue_node_id TEXT NOT NULL, repository TEXT NOT NULL, issue_number INTEGER NOT NULL, state TEXT NOT NULL, config_revision TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(tracker_repo_id, issue_node_id)); CREATE TABLE attempts (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), lifecycle TEXT NOT NULL, outcome TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE intents (sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE reservations (task_id TEXT PRIMARY KEY REFERENCES tasks(id), attempt_id TEXT NOT NULL UNIQUE, status TEXT NOT NULL CHECK(status IN ('reserved','released')), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE evidence (sequence INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision) VALUES('task','repo-id','issue-id','org/repo',7,'preparing','rev'); INSERT INTO attempts(id,task_id,lifecycle) VALUES('attempt','task','launch_intended'); INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('intent','task','attempt','launch','detail'); INSERT INTO reservations(task_id,attempt_id,status) VALUES('task','attempt','reserved'); INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt','project','item'); PRAGMA user_version=1;").unwrap();
    drop(connection);
    drop(StateStore::open(dir.path(), 2).unwrap());
    let connection = Connection::open(&db).unwrap();
    let version: i32 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
    for (table, expected) in [
        ("tasks", 1),
        ("attempts", 1),
        ("intents", 1),
        ("reservations", 1),
        ("evidence", 1),
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, expected, "{table}");
    }
    assert_eq!(
        connection
            .query_row::<i64, _, _>(
                "SELECT value FROM state_meta WHERE key='capacity'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        2
    );
    drop(connection);
    drop(StateStore::open(dir.path(), 2).unwrap());
    assert!(matches!(
        StateStore::open(dir.path(), 3),
        Err(StateError::CapacityMismatch { .. })
    ));
}

#[test]
fn failed_v1_migration_rolls_back_schema_and_version() {
    use rusqlite::Connection;
    let dir = tempfile::tempdir().unwrap();
    let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    connection.execute_batch("CREATE TABLE tasks (id TEXT PRIMARY KEY, tracker_repo_id TEXT NOT NULL, issue_node_id TEXT NOT NULL, repository TEXT NOT NULL, issue_number INTEGER NOT NULL, state TEXT NOT NULL, config_revision TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(tracker_repo_id, issue_node_id)); CREATE TABLE attempts (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), lifecycle TEXT NOT NULL, outcome TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE intents (sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE reservations (task_id TEXT PRIMARY KEY REFERENCES tasks(id), attempt_id TEXT NOT NULL UNIQUE, status TEXT NOT NULL CHECK(status IN ('reserved','released')), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE evidence (sequence INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE state_meta(key TEXT PRIMARY KEY, value INTEGER NOT NULL); INSERT INTO state_meta VALUES('capacity',1); PRAGMA user_version=1;").unwrap();
    drop(connection);
    assert!(StateStore::open(dir.path(), 2).is_err());
    let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    let version: i32 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    assert_eq!(
        connection
            .query_row::<i64, _, _>("SELECT COUNT(*) FROM state_meta", [], |r| r.get(0))
            .unwrap(),
        1
    );
}

use rusqlite::Connection;

#[test]
fn persisted_capacity_is_authoritative_and_schema_version_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        StateStore::open(dir.path(), 0),
        Err(StateError::InvalidCapacity)
    ));
    drop(StateStore::open(dir.path(), 1).unwrap());
    let mismatch = match StateStore::open(dir.path(), 2) {
        Ok(_) => panic!("capacity mismatch was accepted"),
        Err(error) => error,
    };
    assert!(
        matches!(mismatch, StateError::CapacityMismatch { .. }),
        "unexpected reopen error: {mismatch:?}"
    );
    Connection::open(dir.path().join("state.sqlite3"))
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    assert!(matches!(
        StateStore::open(dir.path(), 1),
        Err(StateError::UnsupportedDatabaseVersion(99))
    ));
}

#[test]
fn failed_attempt_insert_rolls_back_its_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 2).unwrap();
    store
        .create_task("t1", "r", "i1", "repo", 1, "rev")
        .unwrap();
    store
        .create_task("t2", "r", "i2", "repo", 2, "rev")
        .unwrap();
    store.reserve("t1", "a1").unwrap();
    assert!(store.reserve("t2", "a1").is_err());
    assert_eq!(store.reservation_count().unwrap(), 1);
}

use luthor::state::{StateError, StateStore};

#[test]
fn persists_identity_evidence_and_reservations_transactionally() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task("t1", "100", "ISSUE1", "org/tracker", 1, "rev")
        .unwrap();
    assert!(matches!(
        store.create_task("t2", "100", "ISSUE1", "org/tracker", 1, "rev"),
        Err(StateError::DuplicateTask(_, _))
    ));
    store
        .record_evidence("t1", None, "project", "item-1")
        .unwrap();
    store
        .record_evidence("t1", None, "direct_issue", "issue-1")
        .unwrap();
    assert_eq!(
        store.evidence_kinds("t1").unwrap(),
        vec!["project", "direct_issue"]
    );
    store.reserve("t1", "a1").unwrap();
    assert!(matches!(
        store.reserve("t1", "a2"),
        Err(StateError::Capacity { .. })
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert_eq!(store.release_reservation("a1").unwrap(), 1);
    assert_eq!(store.reservation_count().unwrap(), 0);
}

#[test]
fn coordinator_lock_is_exclusive_across_processes_and_scoped_to_root() {
    use std::process::Command;
    let held = tempfile::tempdir().unwrap();
    let independent = tempfile::tempdir().unwrap();
    let store = StateStore::open(held.path(), 1).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("coordinator_child_cannot_open_held_root")
        .arg("--nocapture")
        .env("LUTHOR_LOCK_CHILD_ROOT", held.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(StateStore::open(independent.path(), 1).is_ok());
    drop(store);
}

#[test]
fn coordinator_child_cannot_open_held_root() {
    if let Ok(root) = std::env::var("LUTHOR_LOCK_CHILD_ROOT") {
        assert!(StateStore::open(root, 1).is_err());
    }
}

#[test]
fn serializes_coordinator_for_state_directory() {
    let dir = tempfile::tempdir().unwrap();
    let first = StateStore::open(dir.path(), 2).unwrap();
    assert!(StateStore::open(dir.path(), 2).is_err());
    drop(first);
    assert!(StateStore::open(dir.path(), 2).is_ok());
}

#[test]
fn duplicate_identity_does_not_leave_partial_task() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task("t1", "100", "ISSUE1", "org/tracker", 1, "rev")
        .unwrap();
    let _ = store.create_task("t2", "100", "ISSUE1", "org/tracker", 1, "rev");
    assert_eq!(store.task_count().unwrap(), 1);
}
