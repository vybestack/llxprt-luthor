//! Issue 108: resize arithmetic must neither release nor continue historical holds.
use super::capacity_fixture::{Fixture, cli, success};
use luthor::{
    WorktreeOwner,
    state::{StateError, StateStore, journal, scheduling, task_records},
};
use serde_json::{Value, json};
use std::{fs, os::unix::process::CommandExt, process::Command};

fn historical(f: &Fixture, task: &str, number: u64) {
    f.task(task, number);
    f.launch(task, &format!("attempt-{task}"));
    let mut store = StateStore::open(f.root(), f.config.capacity).unwrap();
    task_records::hold_task(&mut store, task, "historical launch remains unknown").unwrap();
}

fn audit(f: &Fixture) -> Value {
    let payload: String = f
        .db()
        .query_row("SELECT permanent_holds FROM capacity_changes", [], |r| {
            r.get(0)
        })
        .unwrap();
    serde_json::from_str(&payload).unwrap()
}

#[test]
fn historical_only_increase_counts_named_holds_without_changing_history_or_authority() {
    let f = Fixture::new(2);
    historical(&f, "legacy", 1);
    let before = f.history();
    let lock = f.root().join("worktree-legacy.lock");
    let lock_bytes = fs::read(&lock).unwrap();
    let result = success(cli(&f.args(2, 3, 1)));
    assert_eq!(result["status"], "changed");
    let holds = &result["change"]["permanent_holds"];
    assert_eq!(holds.as_array().unwrap().len(), 1);
    assert_eq!(holds[0]["task_id"], "legacy");
    assert_eq!(holds[0]["attempt_id"], "attempt-legacy");
    assert!(!holds[0]["reason"].as_str().unwrap().is_empty());
    assert_eq!(audit(&f), *holds);
    assert_eq!(f.history(), before);
    assert_eq!(fs::read(lock).unwrap(), lock_bytes);
    let mut store = StateStore::open(f.root(), 3).unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&store, "legacy")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert!(
        store
            .never_dispatched_context("legacy", "attempt-legacy")
            .is_err()
    );
    assert!(scheduling::release_reservation(&mut store, "attempt-legacy").is_err());
    assert!(luthor::state::retry_plan_for_task(&store, "legacy", "attempt-legacy").is_err());
    drop(store);
    assert_eq!(f.history(), before);
    assert_eq!(
        f.view(&["status"])["capacity"],
        json!({"limit":3,"reserved":1})
    );
    assert_eq!(f.audit_count(), 1);
}

#[test]
fn historical_incomplete_dispatch_with_absent_registered_child_remains_reserved() {
    let f = Fixture::new(2);
    historical(&f, "broken-pipe", 1);
    let mut child = Command::new("/usr/bin/true")
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    assert!(child.wait().unwrap().success());
    f.evidence("broken-pipe", Some("attempt-broken-pipe"), "child_registered",
        &json!({"pid":pid,"group_id":pid,"boot_identity":"historical-boot","start_identity":"historical-start"}).to_string());
    let mut store = StateStore::open(f.root(), 2).unwrap();
    journal::record_intent(
        &mut store,
        "old-dispatch",
        "broken-pipe",
        Some("attempt-broken-pipe"),
        "supervisor_dispatch",
        "original-plan",
    )
    .unwrap();
    drop(store);
    let before = f.history();
    let result = success(cli(&f.args(2, 3, 1)));
    assert_eq!(
        result["change"]["permanent_holds"][0]["attempt_id"],
        "attempt-broken-pipe"
    );
    assert_eq!(audit(&f), result["change"]["permanent_holds"]);
    assert_eq!(f.history(), before);
    assert_eq!(
        f.view(&["status"])["capacity"],
        json!({"limit":3,"reserved":1})
    );
}

#[test]
fn historical_exception_never_allows_a_decrease_or_below_reservations() {
    let f = Fixture::new(3);
    historical(&f, "legacy-a", 1);
    historical(&f, "legacy-b", 2);
    let before = f.history();
    assert!(matches!(
        f.change(3, 1, 2),
        Err(StateError::Capacity {
            reserved: 2,
            capacity: 1
        })
    ));
    assert!(matches!(
        f.change(3, 2, 2),
        Err(StateError::CapacityChangeRefused(_))
    ));
    assert_eq!(f.history(), before);
    assert_eq!(f.audit_count(), 0);
    let result = success(cli(&f.args(3, 4, 2)));
    let holds = result["change"]["permanent_holds"].as_array().unwrap();
    assert_eq!(holds.len(), 2);
    assert_eq!(holds[0]["task_id"], "legacy-a");
    assert_eq!(holds[1]["task_id"], "legacy-b");
    assert_eq!(audit(&f), json!(holds));
    assert_eq!(f.history(), before);
}

#[test]
fn a_historical_hold_does_not_mask_another_active_or_unsettled_launch() {
    for settled_phase in [false, true] {
        let f = Fixture::new(2);
        historical(&f, "legacy", 1);
        f.task("modern", 2);
        f.launch("modern", "attempt-modern");
        if settled_phase {
            let mut store = StateStore::open(f.root(), 2).unwrap();
            task_records::hold_task(&mut store, "modern", "awaiting reconciliation").unwrap();
            drop(store);
            // A real modern initial attempt has preparation proof. Even malformed
            // proof must not be interpreted as historical absence.
            f.db().execute_batch("INSERT INTO launch_preparations(attempt_id,task_id,launch_sequence,owner_payload) SELECT 'attempt-modern','modern',sequence,'{}' FROM intents WHERE task_id='modern' AND kind='launch';").unwrap();
        }
        let before = f.history();
        let output = cli(&f.args(2, 3, 2));
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("active or unreconciled launch"));
        assert_eq!(f.history(), before);
        assert_eq!(f.audit_count(), 0);
        drop(StateStore::open(f.root(), 2).unwrap());
    }
}

#[test]
fn historical_exception_preserves_owner_and_live_process_refusals() {
    let f = Fixture::new(2);
    historical(&f, "legacy", 1);
    let owner = WorktreeOwner::acquire_existing(f.root(), "legacy").unwrap();
    let before = f.history();
    assert!(!cli(&f.args(2, 3, 1)).status.success());
    drop(owner);
    fs::remove_file(f.root().join("worktree-legacy.lock")).unwrap();
    assert!(f.change(2, 3, 1).is_err());
    // Restoring a lock does not override the unchanged process absence check.
    drop(WorktreeOwner::acquire(f.root(), "legacy").unwrap());
    f.evidence(
        "legacy",
        Some("attempt-legacy"),
        "supervisor_ready",
        &json!({"pid":std::process::id(),"boot_identity":"fixture","start_identity":"fixture"})
            .to_string(),
    );
    let with_process = f.history();
    assert!(f.change(2, 3, 1).is_err());
    assert_eq!(f.history(), with_process);
    assert_ne!(before, with_process);
    assert_eq!(f.audit_count(), 0);
}

#[test]
fn historical_audit_and_limit_roll_back_atomically_and_stale_counts_refuse() {
    let f = Fixture::new(2);
    historical(&f, "legacy", 1);
    let before = f.history();
    assert!(f.change(2, 3, 0).is_err());
    f.db().execute_batch("CREATE TRIGGER reject_capacity BEFORE UPDATE ON state_meta BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    assert!(matches!(f.change(2, 3, 1), Err(StateError::Sql(_))));
    assert_eq!(f.history(), before);
    assert_eq!(f.audit_count(), 0);
    drop(StateStore::open(f.root(), 2).unwrap());
}

#[test]
fn complete_historical_dispatch_inventory_must_be_reconciled_not_counted_as_permanent() {
    let f = Fixture::new(2);
    historical(&f, "unsettled", 1);
    let mut store = StateStore::open(f.root(), 2).unwrap();
    for kind in ["supervisor_dispatch", "gate_release"] {
        journal::record_intent(
            &mut store,
            kind,
            "unsettled",
            Some("attempt-unsettled"),
            kind,
            "original-plan",
        )
        .unwrap();
    }
    drop(store);
    for kind in [
        "worktree_owner_protocol",
        "supervisor_ready",
        "child_registered",
        "gate_sent",
    ] {
        // Presence of the entire inventory excludes this exception even when
        // the ordinary reconciliation path will reject malformed proof.
        f.evidence("unsettled", Some("attempt-unsettled"), kind, "{}");
    }
    let before = f.history();
    assert!(matches!(
        f.change(2, 3, 1),
        Err(StateError::CapacityChangeRefused(_))
    ));
    assert_eq!(f.history(), before);
    assert_eq!(f.audit_count(), 0);
}

#[test]
fn historical_classification_database_errors_and_invalid_process_proof_are_not_hidden() {
    let f = Fixture::new(2);
    historical(&f, "legacy", 1);
    f.evidence(
        "legacy",
        Some("attempt-legacy"),
        "child_registered",
        "malformed",
    );
    let before = f.history();
    assert!(matches!(
        f.change(2, 3, 1),
        Err(StateError::Serialization(_))
    ));
    assert_eq!(f.history(), before);
    assert_eq!(f.audit_count(), 0);
    f.db()
        .execute_batch("DROP TABLE launch_preparations;")
        .unwrap();
    assert!(matches!(f.change(2, 3, 1), Err(StateError::Sql(_))));
    assert_eq!(f.audit_count(), 0);
    drop(StateStore::open(f.root(), 2).unwrap());
}

#[test]
fn version_seven_capacity_audits_gain_empty_hold_lists_without_rewriting_old_values() {
    let f = Fixture::new(2);
    f.db()
        .execute_batch(
            "ALTER TABLE capacity_changes DROP COLUMN permanent_holds;
        INSERT INTO capacity_changes(actor,reason,old_capacity,new_capacity,reservations,created_at)
        VALUES('previous-operator','original audit',1,2,0,'2000-01-01 00:00:00');
        PRAGMA user_version=7;",
        )
        .unwrap();
    let old: String = f.db().query_row("SELECT json_array(sequence,actor,reason,old_capacity,new_capacity,reservations,created_at) FROM capacity_changes", [], |r| r.get(0)).unwrap();
    drop(StateStore::open(f.root(), 2).unwrap());
    assert_eq!(audit(&f), json!([]));
    let after: String = f.db().query_row("SELECT json_array(sequence,actor,reason,old_capacity,new_capacity,reservations,created_at) FROM capacity_changes", [], |r| r.get(0)).unwrap();
    assert_eq!(after, old);
    assert_eq!(
        f.db()
            .pragma_query_value::<i32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        8
    );
}

#[test]
fn conflicting_version_seven_hold_audit_schema_fails_without_adopting_it() {
    let f = Fixture::new(2);
    f.db().execute_batch("PRAGMA user_version=7;").unwrap();
    assert!(matches!(f.change(2, 3, 0), Err(StateError::Sql(_))));
    assert_eq!(f.audit_count(), 0);
    assert_eq!(
        f.db()
            .pragma_query_value::<i32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        7
    );
    let capacity: usize = f
        .db()
        .query_row(
            "SELECT value FROM state_meta WHERE key='capacity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(capacity, 2);
}
