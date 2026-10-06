use super::{Lane, await_receipt, database, executable_lane, launch};
use luthor::state::{exit_observation, journal, scheduling, task_records};
use luthor::{
    OwnershipError, WorktreeOwner,
    state::{verify_amended_observation_plan, verify_amended_worker_plan},
    supervisor::{self, LaunchPlan, Reconciliation, RecoveryInspection},
};
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

pub(super) fn await_worktree_owner_release(lane: &Lane) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match WorktreeOwner::acquire_existing(lane.f.store.root(), "task-a") {
            Ok(owner) => {
                drop(owner);
                return;
            }
            Err(OwnershipError::Busy) => {
                assert!(
                    Instant::now() < deadline,
                    "worktree owner remained busy past deadline"
                );
                thread::sleep(Duration::from_millis(20));
            }
            Err(OwnershipError::Unavailable) => panic!("worktree owner is unavailable"),
        }
    }
}

pub(super) fn exited_lane() -> (Lane, LaunchPlan) {
    exited_lane_from(executable_lane())
}

pub(super) fn exited_lane_from(mut lane: Lane) -> (Lane, LaunchPlan) {
    launch(&mut lane, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    await_receipt(&lane);
    await_worktree_owner_release(&lane);
    let plan = serde_json::from_slice(
        &fs::read(
            lane.f
                .store
                .root()
                .join("attempts/attempt-task-a.plan.json"),
        )
        .unwrap(),
    )
    .unwrap();
    (lane, plan)
}

#[test]
fn amended_terminal_binding_rejects_missing_duplicate_tampered_and_replayed_history() {
    for sql in [
        "DELETE FROM evidence WHERE kind='initial_branch_removed'",
        "DELETE FROM intents WHERE kind='initial_branch_removal_seal'",
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='initial_branch_removed'",
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) SELECT 'duplicate',task_id,attempt_id,kind,detail FROM intents WHERE kind='initial_branch_removal_seal'",
        "UPDATE evidence SET payload=json_set(payload,'$.effective_plan.args[0]','forged') WHERE kind='initial_branch_removed'",
        "UPDATE intents SET detail=json_set(detail,'$.amendment_sequence',99999) WHERE kind='supervisor_dispatch'",
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) SELECT 'duplicate',task_id,attempt_id,kind,detail FROM intents WHERE kind='supervisor_dispatch'",
        "UPDATE intents SET detail=json_extract(detail,'$.effective_plan') WHERE kind='supervisor_dispatch'",
        "UPDATE intents SET detail=json_set(detail,'$.args[0]','forged') WHERE kind='launch'",
        "UPDATE evidence SET payload=json_set(payload,'$.effective_config.resume.args[0]','forged') WHERE kind='selection'",
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES('replay','task-a','launch_intended')",
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-a','attempt-task-a','unknown','{}')",
        "UPDATE attempts SET outcome='invented'",
        "UPDATE reservations SET status='reserved'",
    ] {
        let (mut lane, plan) = exited_lane();
        assert!(matches!(
            supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
            Reconciliation::Completed { .. }
        ));
        database(&lane).execute_batch(sql).unwrap();
        assert!(
            verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).is_err(),
            "{sql}"
        );
        assert!(
            matches!(
                supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a")
                    .unwrap(),
                Reconciliation::Held { .. }
            ),
            "{sql}"
        );
    }
}

#[test]
fn amended_attention_and_blocked_observation_remain_bound_without_becoming_launch_authorization() {
    let (mut lane, plan) = exited_lane();
    supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap();
    exit_observation::record_exit_pr_lookup(
        &mut lane.f.store,
        "task-a",
        "attempt-task-a",
        &super::super::pr(),
    )
    .unwrap();
    verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).unwrap();
    assert!(verify_amended_worker_plan(&database(&lane), lane.f.store.root(), &plan).is_err());
    assert_eq!(
        task_records::task_phase(&lane.f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attention")
    );
    assert!(matches!(
        supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(17),
            signal: None
        }
    ));
}

#[test]
fn amended_missing_receipt_requires_registered_absence_and_never_invents_exit() {
    let (mut lane, plan) = exited_lane();
    fs::remove_file(
        lane.f
            .store
            .root()
            .join("attempts/attempt-task-a.receipt.json"),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let inspection =
            supervisor::inspect_recovery_quiescence(&lane.f.store, "task-a", "attempt-task-a")
                .unwrap();
        if inspection == RecoveryInspection::Quiescent {
            break;
        }
        assert!(Instant::now() < deadline, "{inspection:?}");
        thread::sleep(Duration::from_millis(20));
    }
    verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).unwrap();
    assert!(matches!(
        supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
        Reconciliation::Held { .. }
    ));
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
    assert!(
        journal::evidence_payloads(&lane.f.store, "task-a", "attempt-task-a", "attempt_exit")
            .unwrap()
            .is_empty()
    );
    database(&lane)
        .execute("DELETE FROM evidence WHERE kind='child_registered'", [])
        .unwrap();
    assert!(matches!(
        supervisor::inspect_recovery_quiescence(&lane.f.store, "task-a", "attempt-task-a").unwrap(),
        RecoveryInspection::Held(_)
    ));
}
