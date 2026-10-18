mod capacity_audit;
mod capacity_fixture;
mod capacity_holds;
use capacity_fixture::{Fixture, cli, success};
use luthor::{
    WorktreeOwner,
    state::{
        StateError, StateStore,
        capacity::{self, CapacityChangeRequest},
        scheduling,
    },
};
use serde_json::{Value, json};
use std::fs;

#[test]
fn quiescent_one_to_two_preserves_six_tasks_attempts_receipts_and_pr_proof_on_restart() {
    let f = Fixture::new(1);
    for n in 1..=6 {
        f.completed(&format!("task-{n}"), n);
    }
    let before = f.history();
    let shown = f.view(&["show", "task-6"]);
    let files: Vec<_> = fs::read_dir(f.root().join("attempts"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    assert!(matches!(
        StateStore::open(f.root(), 2),
        Err(StateError::CapacityMismatch {
            configured: 2,
            persisted: 1
        })
    ));
    assert_eq!(
        f.view(&["status"])["capacity"],
        json!({"reserved":0,"limit":1})
    );
    let result = success(cli(&f.args(1, 2, 0)));
    assert_eq!(result["status"], "changed");
    assert_eq!(result["change"]["actor"], "operator");
    assert_eq!(result["change"]["old_capacity"], 1);
    assert_eq!(result["change"]["new_capacity"], 2);
    assert_eq!(result["change"]["reservations"], 0);
    assert!(
        result["change"]["created_at"]
            .as_str()
            .unwrap()
            .contains(' ')
    );
    drop(StateStore::open(f.root(), 2).unwrap());
    assert!(matches!(
        StateStore::open(f.root(), 1),
        Err(StateError::CapacityMismatch {
            configured: 1,
            persisted: 2
        })
    ));
    assert_eq!(f.history(), before);
    for (path, bytes) in files {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    let after = f.view(&["show", "task-6"]);
    for key in [
        "task",
        "attempts",
        "evidence",
        "intents",
        "session",
        "verified_pr",
        "latest_attempt_outcome",
    ] {
        assert_eq!(after[key], shown[key], "changed {key}");
    }
    let status = f.view(&["status"]);
    assert_eq!(status["capacity"], json!({"reserved":0,"limit":2}));
    assert_eq!(status["tasks"].as_array().unwrap().len(), 6);
    assert_eq!(status["tasks"][5]["verified_pr"]["id"], 6);
    let audit: Value = f.db().query_row("SELECT json_object('actor',actor,'reason',reason,'old_capacity',old_capacity,'new_capacity',new_capacity,'reservations',reservations) FROM capacity_changes", [], |r| r.get::<_,String>(0)).map(|s| serde_json::from_str(&s).unwrap()).unwrap();
    assert_eq!(
        audit,
        json!({"actor":"operator","reason":"resize the existing queue","old_capacity":1,"new_capacity":2,"reservations":0})
    );
    assert_eq!(f.audit_count(), 1);
}

#[test]
fn active_launches_are_refused_even_when_increasing_and_history_is_untouched() {
    let f = Fixture::new(2);
    f.task("active", 1);
    f.launch("active", "attempt-active");
    let before = f.history();
    for new in [1, 3] {
        assert!(matches!(
            f.change(2, new, 1),
            Err(StateError::CapacityChangeRefused(_))
        ));
        assert_eq!(f.history(), before);
        assert_eq!(f.audit_count(), 0);
        drop(StateStore::open(f.root(), 2).unwrap());
    }
}

#[test]
fn reservation_expectations_and_decrease_bounds_are_verified_without_freeing_slots() {
    let f = Fixture::new(3);
    for n in 1..=3 {
        f.task(&format!("task-{n}"), n);
    }
    let mut store = StateStore::open(f.root(), 3).unwrap();
    for n in 1..=2 {
        scheduling::reserve(&mut store, &format!("task-{n}"), &format!("attempt-{n}")).unwrap();
    }
    drop(store);
    let before = f.history();
    assert!(matches!(
        f.change(3, 2, 0),
        Err(StateError::CapacityChangeRefused(_))
    ));
    assert!(matches!(
        f.change(3, 1, 2),
        Err(StateError::Capacity {
            reserved: 2,
            capacity: 1
        })
    ));
    assert_eq!(f.history(), before);
    assert_eq!(f.audit_count(), 0);
    let audit = f.change(3, 2, 2).unwrap();
    assert_eq!(audit.reservations, 2);
    assert_eq!(f.history(), before);
    let mut store = StateStore::open(f.root(), 2).unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 2);
    assert!(matches!(
        scheduling::reserve(&mut store, "task-3", "attempt-3"),
        Err(StateError::Capacity {
            reserved: 2,
            capacity: 2
        })
    ));
    drop(store);
    f.change(2, 4, 2).unwrap();
    let mut store = StateStore::open(f.root(), 4).unwrap();
    scheduling::reserve(&mut store, "task-3", "attempt-3").unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 3);
    assert_eq!(f.audit_count(), 2);
}

#[test]
fn stale_previous_capacity_is_refused_before_any_audit_or_capacity_change() {
    let f = Fixture::new(1);
    let args = f.args(1, 2, 0);
    success(cli(&args));
    let output = cli(&args);
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "refused");
    assert!(
        result["reason"]
            .as_str()
            .unwrap()
            .contains("does not match persisted capacity 2")
    );
    assert_eq!(f.audit_count(), 1);
    drop(StateStore::open(f.root(), 2).unwrap());
}

#[test]
fn coordinator_lock_blocks_resize_in_another_process() {
    let f = Fixture::new(1);
    let store = StateStore::open(f.root(), 1).unwrap();
    let args = f.args(1, 2, 0);
    let output = cli(&args);
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["reason"], "coordinator busy; try again");
    assert_eq!(f.audit_count(), 0);
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    drop(store);
    success(cli(&args));
}

#[test]
fn conflicting_worktree_owner_refuses_resize_until_owner_releases() {
    let f = Fixture::new(1);
    f.completed("task", 1);
    let owner = WorktreeOwner::acquire_existing(f.root(), "task").unwrap();
    let before = f.history();
    let output = cli(&f.args(1, 2, 0));
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        result["reason"]
            .as_str()
            .unwrap()
            .contains("worktree owner is live or unavailable")
    );
    assert_eq!(f.audit_count(), 0);
    assert_eq!(f.history(), before);
    drop(owner);
    f.change(1, 2, 0).unwrap();
}

#[test]
fn missing_historical_owner_proof_and_live_registered_processes_fail_closed() {
    let f = Fixture::new(1);
    f.completed("task", 1);
    let owner = f.root().join("worktree-task.lock");
    let saved = fs::read(&owner).unwrap();
    fs::remove_file(&owner).unwrap();
    assert!(f.change(1, 2, 0).is_err());
    fs::write(&owner, saved).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&owner, fs::Permissions::from_mode(0o600)).unwrap();
    f.evidence(
        "task",
        Some("attempt-task"),
        "supervisor_ready",
        &json!({
            "pid":std::process::id(), "boot_identity":"fixture", "start_identity":"fixture"
        })
        .to_string(),
    );
    let error = f.change(1, 2, 0).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("registered process may be active")
    );
    assert_eq!(f.audit_count(), 0);
}

#[test]
fn unknown_database_versions_are_rejected_without_altering_history_or_limit() {
    for version in [-1, 10, 99] {
        let f = Fixture::new(1);
        f.completed("task", 1);
        let before = f.history();
        f.db().pragma_update(None, "user_version", version).unwrap();
        assert!(
            matches!(f.change(1, 2, 0), Err(StateError::UnsupportedDatabaseVersion(v)) if v==version)
        );
        assert_eq!(f.history(), before);
        assert_eq!(f.audit_count(), 0);
        let db = f.db();
        assert_eq!(
            db.pragma_query_value::<i32, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            version
        );
        assert_eq!(
            db.query_row::<i64, _, _>(
                "SELECT value FROM state_meta WHERE key='capacity'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
            1
        );
    }
}

#[test]
fn explicit_authorization_positive_limits_and_actor_are_required() {
    let f = Fixture::new(1);
    let mut args = f.args(1, 2, 0);
    args.pop();
    let output = cli(&args);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--execute"));
    for (new, actor, reason) in [
        (0, "operator", "reason"),
        (usize::MAX, "operator", "reason"),
        (2, " ", "reason"),
        (2, "operator", " "),
        (1, "operator", "reason"),
    ] {
        assert!(
            capacity::change(
                f.root(),
                CapacityChangeRequest {
                    expected_capacity: 1,
                    expected_reservations: 0,
                    new_capacity: new,
                    actor,
                    reason,
                }
            )
            .is_err()
        );
    }
    assert_eq!(f.audit_count(), 0);
    drop(StateStore::open(f.root(), 1).unwrap());
}

#[test]
fn resize_does_not_create_a_replacement_database() {
    let f = Fixture::new(1);
    let missing = f.dir.path().join("new-root");
    assert!(
        capacity::change(
            &missing,
            CapacityChangeRequest {
                expected_capacity: 1,
                expected_reservations: 0,
                new_capacity: 2,
                actor: "operator",
                reason: "must use existing queue",
            }
        )
        .is_err()
    );
    assert!(!missing.exists());
    fs::create_dir(&missing).unwrap();
    assert!(
        capacity::change(
            &missing,
            CapacityChangeRequest {
                expected_capacity: 1,
                expected_reservations: 0,
                new_capacity: 2,
                actor: "operator",
                reason: "must use existing queue",
            }
        )
        .is_err()
    );
    assert!(!missing.join("state.sqlite3").exists());
}

#[test]
fn audit_and_capacity_update_roll_back_together_on_database_write_failure() {
    let f = Fixture::new(1);
    f.completed("task", 1);
    let before = f.history();
    f.db().execute_batch("CREATE TRIGGER reject_capacity BEFORE UPDATE ON state_meta BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    assert!(matches!(f.change(1, 2, 0), Err(StateError::Sql(_))));
    assert_eq!(f.audit_count(), 0);
    assert_eq!(f.history(), before);
    drop(StateStore::open(f.root(), 1).unwrap());
    f.db()
        .execute_batch("DROP TRIGGER reject_capacity;")
        .unwrap();
    f.change(1, 2, 0).unwrap();
}

#[test]
fn version_six_migration_retains_history_and_audit_schema_conflicts_fail_atomically() {
    let f = Fixture::new(1);
    f.completed("task", 1);
    let before = f.history();
    f.db()
        .execute_batch("DROP TABLE capacity_changes; PRAGMA user_version=6;")
        .unwrap();
    f.change(1, 2, 0).unwrap();
    assert_eq!(f.history(), before);
    assert_eq!(f.audit_count(), 1);
    let conflict = Fixture::new(1);
    conflict.db().execute_batch("DROP TABLE capacity_changes; CREATE TABLE capacity_changes(conflict TEXT); PRAGMA user_version=6;").unwrap();
    assert!(matches!(conflict.change(1, 2, 0), Err(StateError::Sql(_))));
    assert_eq!(
        conflict
            .db()
            .pragma_query_value::<i32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        6
    );
    assert_eq!(
        conflict
            .db()
            .query_row::<i64, _, _>(
                "SELECT value FROM state_meta WHERE key='capacity'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn released_reservation_is_not_substitute_for_verified_launch_exit() {
    for mutation in [
        "UPDATE reservations SET status='released'",
        "UPDATE evidence SET payload='malformed' WHERE kind='attempt_exit'",
        "UPDATE attempts SET outcome='invented outcome'",
        "DELETE FROM evidence WHERE kind='attempt_exit'",
    ] {
        let f = Fixture::new(1);
        f.completed("task", 1);
        if mutation.starts_with("UPDATE reservations") {
            f.db()
                .execute_batch("UPDATE attempts SET lifecycle='launch_intended',outcome=NULL;")
                .unwrap();
        }
        f.db().execute_batch(mutation).unwrap();
        let before = f.history();
        assert!(f.change(1, 2, 0).is_err(), "accepted {mutation}");
        assert_eq!(f.audit_count(), 0);
        assert_eq!(f.history(), before);
    }
}

#[test]
fn unversioned_database_and_inconsistent_reservation_identity_are_not_adopted() {
    let f = Fixture::new(1);
    f.db().pragma_update(None, "user_version", 1).unwrap();
    assert!(matches!(
        f.change(1, 2, 0),
        Err(StateError::UnsupportedDatabaseVersion(1))
    ));
    assert_eq!(f.audit_count(), 0);
    let f = Fixture::new(1);
    f.task("task", 1);
    f.db().execute_batch("INSERT INTO reservations(attempt_id,task_id,status) VALUES('missing-attempt','task','reserved');").unwrap();
    let before = f.history();
    assert!(matches!(
        f.change(1, 2, 1),
        Err(StateError::CapacityChangeRefused(_))
    ));
    assert_eq!(f.history(), before);
    assert_eq!(f.audit_count(), 0);
}
