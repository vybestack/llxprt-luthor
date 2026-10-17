use super::*;
use luthor::{
    WorktreeOwner,
    state::{verify_amended_observation_plan, verify_amended_worker_plan},
};
#[test]
fn native_continuation_all_binding_gates_require_own_sealed_transition_and_predecessor_history() {
    let mut lane = fixture(false);
    let mut launcher = Launcher {
        count: 0,
        execute: false,
    };
    assert!(
        run(
            &mut lane,
            Mode::NaturalExitRetry,
            "attempt-task-a",
            "attempt-second",
            "ok",
            &mut launcher
        )
        .is_err()
    );
    let raw = launches::launch_intent(&lane.f.store, "attempt-second")
        .unwrap()
        .unwrap();
    let plan: LaunchPlan = serde_json::from_str(&raw).unwrap();
    let owner = WorktreeOwner::acquire_existing(lane.f.store.root(), "task-a").unwrap();
    let protocol = owner
        .protocol_evidence(lane.f.store.root(), "task-a", "attempt-second")
        .unwrap();
    launches::begin_supervision(
        &mut lane.f.store,
        "task-a",
        "attempt-second",
        &raw,
        &protocol,
    )
    .unwrap();
    let db = database(&lane);
    verify_amended_worker_plan(&db, lane.f.store.root(), &plan).unwrap();
    verify_amended_observation_plan(&db, lane.f.store.root(), &plan).unwrap();
    for table in ["evidence", "intents"] {
        let (kind, column) = if table == "evidence" {
            ("native_continuation", "payload")
        } else {
            ("native_continuation_seal", "detail")
        };
        let sql =
            format!("SELECT {column} FROM {table} WHERE kind=?1 AND attempt_id='attempt-second'");
        let original: String = db.query_row(&sql, [kind], |r| r.get(0)).unwrap();
        db.execute(
            &format!("UPDATE {table} SET {column}='{{}}' WHERE kind=?1"),
            [kind],
        )
        .unwrap();
        assert!(verify_amended_worker_plan(&db, lane.f.store.root(), &plan).is_err());
        assert!(verify_amended_observation_plan(&db, lane.f.store.root(), &plan).is_err());
        db.execute(
            &format!("UPDATE {table} SET {column}=?1 WHERE kind=?2"),
            rusqlite::params![original, kind],
        )
        .unwrap();
    }
    db.execute_batch("INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='native_continuation'").unwrap();
    assert!(verify_amended_worker_plan(&db, lane.f.store.root(), &plan).is_err());
    db.execute_batch("DELETE FROM evidence WHERE sequence=(SELECT MAX(sequence) FROM evidence)")
        .unwrap();
    verify_amended_worker_plan(&db, lane.f.store.root(), &plan).unwrap();
    let mut replay = plan.clone();
    replay.args = lane.plan.args.clone();
    assert!(verify_amended_worker_plan(&db, lane.f.store.root(), &replay).is_err());
    assert!(verify_amended_worker_plan(&db, lane.f.store.root(), &lane.plan).is_err());
    db.execute_batch("UPDATE evidence SET payload=json_set(payload,'$.exit_code',0) WHERE kind='attempt_exit' AND attempt_id='attempt-task-a'").unwrap();
    assert!(verify_amended_worker_plan(&db, lane.f.store.root(), &plan).is_err());
    assert!(verify_amended_observation_plan(&db, lane.f.store.root(), &plan).is_err());
    assert!(
        !lane
            .f
            .store
            .root()
            .join("attempts/attempt-second.receipt.json")
            .exists()
    );
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
}
