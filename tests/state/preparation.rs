use super::*;
use luthor::state::{journal, scheduling, task_records};

#[test]
fn v4_migration_adds_no_historical_preparation_and_preserves_exact_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 2).unwrap();
    task_records::create_task(&mut store, "old", &candidate("i1", 1), "rev", &config()).unwrap();
    scheduling::reserve(&mut store, "old", "old-attempt").unwrap();
    let original = "{ \"original\": \"unproved legacy launch bytes\" }\n";
    journal::record_intent(
        &mut store,
        "launch-old",
        "old",
        Some("old-attempt"),
        "launch",
        original,
    )
    .unwrap();
    drop(store);
    let db = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE launch_preparations; PRAGMA user_version=4;")
        .unwrap();
    let before = history(&db);
    drop(StateStore::open(dir.path(), 2).unwrap());
    assert_eq!(history(&db), before);
    assert_eq!(
        db.pragma_query_value::<i32, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        5
    );
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT COUNT(*) FROM launch_preparations", [], |row| row
            .get(0))
            .unwrap(),
        0
    );
    let mut reopened = StateStore::open(dir.path(), 2).unwrap();
    assert_eq!(scheduling::reservation_count(&reopened).unwrap(), 1);
    assert!(scheduling::release_reservation(&mut reopened, "old-attempt").is_err());
    drop(reopened);
    assert_eq!(history(&db), before);
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn conflicting_v4_preparation_schema_fails_without_accepting_or_backfilling_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 2).unwrap();
    task_records::create_task(&mut store, "old", &candidate("i1", 1), "rev", &config()).unwrap();
    scheduling::reserve(&mut store, "old", "old-attempt").unwrap();
    drop(store);
    let db = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    db.execute_batch(
        "DROP TABLE launch_preparations;
        CREATE TABLE launch_preparations(conflict TEXT); PRAGMA user_version=4;",
    )
    .unwrap();
    let before = history(&db);
    assert!(matches!(
        StateStore::open(dir.path(), 2),
        Err(StateError::Sql(_))
    ));
    assert_eq!(history(&db), before);
    assert_eq!(
        db.pragma_query_value::<i32, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        4
    );
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT COUNT(*) FROM launch_preparations", [], |row| row
            .get(0))
            .unwrap(),
        0
    );
}

fn history(db: &Connection) -> Vec<String> {
    [
        "SELECT json_group_array(json_array(id,state,created_at)) FROM tasks",
        "SELECT json_group_array(json_array(id,task_id,lifecycle,outcome,created_at)) FROM attempts",
        "SELECT json_group_array(json_array(attempt_id,task_id,status,created_at)) FROM reservations",
        "SELECT json_group_array(json_array(sequence,id,task_id,attempt_id,kind,detail,created_at)) FROM intents ORDER BY sequence",
        "SELECT json_group_array(json_array(sequence,task_id,attempt_id,kind,payload,created_at)) FROM evidence ORDER BY sequence",
        "SELECT json_group_array(json_array(name,seq)) FROM sqlite_sequence ORDER BY name",
    ].iter().map(|sql| db.query_row(sql, [], |row| row.get(0)).unwrap()).collect()
}
