//! Additive issue-116 audit migration, never adoption of conflicting schemas.
use super::capacity_fixture::Fixture;
use luthor::state::{StateError, StateStore};
use serde_json::{Value, json};

#[test]
fn version_eight_audits_gain_empty_running_lists_without_changing_previous_fields() {
    let f = Fixture::new(2);
    f.db().execute_batch(
        r#"ALTER TABLE capacity_changes DROP COLUMN running_attempts;
         INSERT INTO capacity_changes(actor,reason,old_capacity,new_capacity,reservations,permanent_holds,created_at)
         VALUES('previous','original',1,2,1,'[{"task_id":"legacy","attempt_id":"old","reason":"held"}]','2000-01-01 00:00:00');
         PRAGMA user_version=8;"#,
    ).unwrap();
    let old: String = f.db().query_row("SELECT json_array(sequence,actor,reason,old_capacity,new_capacity,reservations,permanent_holds,created_at) FROM capacity_changes", [], |r| r.get(0)).unwrap();
    drop(StateStore::open(f.root(), 2).unwrap());
    let after: String = f.db().query_row("SELECT json_array(sequence,actor,reason,old_capacity,new_capacity,reservations,permanent_holds,created_at) FROM capacity_changes", [], |r| r.get(0)).unwrap();
    assert_eq!(old, after);
    let running: String = f
        .db()
        .query_row("SELECT running_attempts FROM capacity_changes", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&running).unwrap(), json!([]));
    assert_eq!(
        f.db()
            .pragma_query_value::<i32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        9
    );
}

#[test]
fn conflicting_version_eight_audit_schema_is_not_adopted_or_partially_migrated() {
    let f = Fixture::new(2);
    f.db().execute_batch("PRAGMA user_version=8;").unwrap();
    assert!(matches!(f.change(2, 3, 0), Err(StateError::Sql(_))));
    assert_eq!(f.audit_count(), 0);
    assert_eq!(
        f.db()
            .pragma_query_value::<i32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        8
    );
    assert_eq!(
        f.db()
            .query_row::<usize, _, _>(
                "SELECT value FROM state_meta WHERE key='capacity'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        2
    );
}
