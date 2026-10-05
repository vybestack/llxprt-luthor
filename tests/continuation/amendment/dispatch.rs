use super::{amend, database, fixture};

#[test]
fn amended_dispatch_requires_audit_current_config_and_atomic_marker() {
    let mut lane = fixture();
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    assert!(
        lane.f
            .store
            .begin_amended_supervision(&context, &lane.f.config, "corrected-revision")
            .is_err()
    );
    amend(&mut lane).unwrap();
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    assert!(
        lane.f
            .store
            .begin_amended_supervision(&context, &lane.f.config, "wrong-revision")
            .is_err()
    );
    let db = database(&lane);
    db.execute_batch("CREATE TRIGGER dispatch_failure BEFORE INSERT ON intents WHEN NEW.kind='supervisor_dispatch' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        lane.f
            .store
            .begin_amended_supervision(&context, &lane.f.config, "corrected-revision")
            .is_err()
    );
    assert_eq!(
        lane.f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap(),
        context
    );
    let dispatches: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM intents WHERE kind='supervisor_dispatch'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dispatches, 0);
    db.execute_batch("DROP TRIGGER dispatch_failure;").unwrap();
    db.execute("INSERT INTO evidence(task_id,kind,payload) VALUES('task-a','held_reason','changed after inspection')", []).unwrap();
    assert!(
        lane.f
            .store
            .begin_amended_supervision(&context, &lane.f.config, "corrected-revision")
            .is_err()
    );
    let dispatches: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM intents WHERE kind='supervisor_dispatch'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dispatches, 0);
    assert_eq!(lane.f.store.reservation_count().unwrap(), 1);
}
