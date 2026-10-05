use super::{Lane, database, observation::exited_lane, pr_completion::PrReader};
use luthor::{
    coordinator::{self, AttemptReview},
    state::{StateError, verify_amended_observation_plan},
    supervisor::{self, LaunchPlan, RecoveryInspection},
};
use std::{
    fs, thread,
    time::{Duration, Instant},
};

fn missing_receipt() -> (Lane, LaunchPlan) {
    let (lane, plan) = exited_lane();
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
            return (lane, plan);
        }
        assert!(Instant::now() < deadline, "{inspection:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn history(lane: &Lane) -> Vec<String> {
    let db = database(lane);
    [
        "SELECT json_group_array(json_array(rowid,id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision,created_at)) FROM tasks ORDER BY rowid",
        "SELECT json_group_array(json_array(rowid,id,task_id,lifecycle,outcome,created_at)) FROM attempts ORDER BY rowid",
        "SELECT json_group_array(json_array(rowid,task_id,attempt_id,status,created_at)) FROM reservations ORDER BY rowid",
        "SELECT json_group_array(json_array(sequence,id,task_id,attempt_id,kind,detail,created_at)) FROM intents ORDER BY sequence",
        "SELECT json_group_array(json_array(sequence,task_id,attempt_id,kind,payload,created_at)) FROM evidence ORDER BY sequence",
    ].into_iter().map(|sql| db.query_row(sql, [], |r| r.get(0)).unwrap()).collect()
}

#[test]
fn safety_amended_operator_missing_receipt_refuses_absent_and_matching_pr_without_writes() {
    for matching in [false, true] {
        let (mut lane, plan) = missing_receipt();
        let before = history(&lane);
        let mut prs = PrReader {
            inner: crate::FakePr {
                present_on: matching.then_some(1),
                ..Default::default()
            },
            mutation: "matching",
        };
        let result = coordinator::operator_recover_missing_receipt(
            &mut lane.f.store,
            "task-a",
            "attempt-task-a",
            "bot",
            "lost receipt",
            &mut lane.github,
            &mut prs,
        );
        assert!(
            matches!(
                result,
                Err(supervisor::SupervisorError::State(
                    StateError::LaunchBlocked
                ))
            ),
            "{matching}: {result:?}"
        );
        assert_eq!(
            prs.inner.lookups, 1,
            "exercise actual recovery commit after PR lookup"
        );
        assert_eq!(history(&lane), before);
        verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).unwrap();
        assert_eq!(lane.f.store.reservation_count().unwrap(), 1);
        assert_eq!(
            lane.f.store.task_phase("task-a").unwrap().as_deref(),
            Some("held")
        );
        assert!(lane.github.assigned);
        let report =
            coordinator::startup_reconcile_all(&mut lane.f.store, &mut lane.github, &mut prs)
                .unwrap();
        assert_eq!(report.attempts.len(), 1);
        assert!(matches!(report.attempts[0].review, AttemptReview::Held(_)));
        assert_eq!(history(&lane), before);
        verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan).unwrap();
        assert!(matches!(
            supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
            supervisor::Reconciliation::Held { .. }
        ));
        assert_eq!(history(&lane), before);
        assert!(lane.github.assigned);
    }
}
