use super::super::future_templates::{audit_json, dual_fixture, reseal};
use super::{
    database, executable_lane_from, observation::exited_lane_from, pr_completion::complete_from,
};
use luthor::state::{launches, scheduling, task_records};
use luthor::{
    state::{verify_amended_observation_plan, verify_amended_worker_plan},
    supervisor::{self, Reconciliation},
};

#[test]
fn future_template_dual_pr_completion_observation_validates_version_without_continuation_authority()
{
    let lane = executable_lane_from(dual_fixture());
    let selection = task_records::selection_evidence(&lane.f.store, "task-a")
        .unwrap()
        .unwrap();
    let saved = launches::launch_intent(&lane.f.store, "attempt-task-a")
        .unwrap()
        .unwrap();
    let (mut lane, plan, result) = complete_from(exited_lane_from(lane), "matching");
    assert!(matches!(result, Reconciliation::Completed { .. }));
    assert_eq!(
        task_records::task_phase(&lane.f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    assert_eq!(audit_json(&lane)["schema_version"], 3);
    verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).unwrap();
    assert!(verify_amended_worker_plan(&database(&lane), lane.f.store.root(), &plan).is_err());
    assert!(launches::resume_context(&lane.f.store, "task-a").is_err());
    assert!(
        luthor::state::retry_context_for_task(&lane.f.store, "task-a", "attempt-task-a").is_err()
    );
    assert_eq!(
        task_records::selection_evidence(&lane.f.store, "task-a")
            .unwrap()
            .unwrap(),
        selection
    );
    assert_eq!(
        launches::launch_intent(&lane.f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        saved
    );
    let mut audit = audit_json(&lane);
    audit["future_template_correction"]["resume"]["index"] = 999.into();
    reseal(&lane, &audit);
    assert!(verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).is_err());
    assert!(matches!(
        supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
        Reconciliation::Held { .. }
    ));
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn future_template_dual_ready_gate_rejects_resealed_policy_drift_and_missing_seal() {
    use std::io::Write;
    for mutation in ["policy", "seal"] {
        let mut lane = executable_lane_from(dual_fixture());
        let mut child = super::gates::ready_supervisor(&mut lane);
        if mutation == "policy" {
            let mut audit = audit_json(&lane);
            audit["future_template_correction"]["policy"] = "initial_only_v1".into();
            reseal(&lane, &audit);
        } else {
            database(&lane)
                .execute(
                    "DELETE FROM intents WHERE kind='initial_branch_removal_seal'",
                    [],
                )
                .unwrap();
        }
        child.stdin.take().unwrap().write_all(b"R").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success(), "{mutation}");
        assert!(
            std::fs::read(
                lane.f
                    .store
                    .root()
                    .join("attempts/attempt-task-a.stdout.log")
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
    }
}
