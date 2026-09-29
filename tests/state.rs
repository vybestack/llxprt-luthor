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
    store.reserve("t1", "a1", 1).unwrap();
    assert!(matches!(
        store.reserve("t1", "a2", 1),
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
