use rusqlite::Connection;

#[test]
fn persisted_capacity_is_authoritative_and_schema_version_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        StateStore::open(dir.path(), 0),
        Err(StateError::InvalidCapacity)
    ));
    drop(StateStore::open(dir.path(), 1).unwrap());
    assert!(matches!(
        StateStore::open(dir.path(), 2),
        Err(StateError::CapacityMismatch { .. })
    ));
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
