use super::{
    gates,
    setup::{commit, drift, lane, script},
};
use std::{fs, io::Write};

#[test]
fn safety_real_supervisor_ready_release_refuses_drift_before_forwarding_worker_gate() {
    for amended in [false, true] {
        for change in ["tracked", "untracked", "descendant"] {
            let mut lane = lane(amended);
            let plan = commit(&mut lane, amended);
            let path = lane
                .f
                .store
                .root()
                .join("attempts/attempt-task-a.plan.json");
            fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
            script::private(&path);
            let mut child = gates::ready_committed_supervisor(&mut lane);
            drift(&plan, change);
            child.stdin.take().unwrap().write_all(b"R").unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(!output.status.success(), "{amended}: {change}");
            assert!(
                fs::read(
                    lane.f
                        .store
                        .root()
                        .join("attempts/attempt-task-a.stdout.log")
                )
                .unwrap()
                .is_empty()
            );
            assert!(
                !lane
                    .f
                    .store
                    .root()
                    .join("attempts/attempt-task-a.receipt.json")
                    .exists()
            );
            assert_eq!(lane.f.store.reservation_count().unwrap(), 1);
        }
    }
}

#[test]
fn safety_already_run_amended_observation_still_accepts_dirty_and_descendant_worktrees() {
    for change in ["tracked", "untracked", "descendant"] {
        let (mut lane, plan) = super::super::observation::exited_lane();
        drift(&plan, change);
        luthor::state::verify_amended_observation_plan(
            &super::database(&lane),
            lane.f.store.root(),
            &plan,
        )
        .unwrap();
        assert!(matches!(
            luthor::supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a")
                .unwrap(),
            luthor::supervisor::Reconciliation::Completed {
                exit_code: Some(17),
                signal: None
            }
        ));
        assert_eq!(lane.f.store.reservation_count().unwrap(), 0);
    }
}
