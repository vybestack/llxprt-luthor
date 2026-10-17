use super::*;
#[test]
fn native_continuation_pr_completion_remains_observable_and_is_never_retryable() {
    let mut lane = fixture(false);
    let mut launcher = Launcher {
        count: 0,
        execute: true,
    };
    let plan = run(
        &mut lane,
        Mode::NaturalExitRetry,
        "attempt-task-a",
        "attempt-second",
        "ok",
        &mut launcher,
    )
    .unwrap();
    crate::coordinator::waiting::receipt(lane.f.store.root(), "attempt-second");
    crate::coordinator::waiting::owner_release(lane.f.store.root(), "task-a");
    let mut pr = super::super::pr_completion::PrReader {
        inner: crate::FakePr {
            present_on: Some(1),
            ..Default::default()
        },
        head: luthor::worktree::verify_existing_worktree(&lane.f.store, "task-a")
            .unwrap()
            .head,
        mutation: "matching",
    };
    assert!(matches!(
        reconcile_with_pr(
            &mut lane.f.store,
            "task-a",
            "attempt-second",
            &mut lane.github,
            &mut pr
        )
        .unwrap(),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(
        task_records::task_phase(&lane.f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    luthor::state::verify_amended_observation_plan(&database(&lane), lane.f.store.root(), &plan)
        .unwrap();
    assert!(
        run(
            &mut lane,
            Mode::NaturalExitRetry,
            "attempt-second",
            "attempt-third",
            "ok",
            &mut launcher
        )
        .is_err()
    );
    assert_eq!(launcher.count, 1);
    let db = database(&lane);
    let count: i64=db.query_row("SELECT COUNT(*) FROM intents WHERE kind='amended_pr_completion' AND attempt_id='attempt-second'",[],|r|r.get(0)).unwrap();
    assert_eq!(count, 1);
    db.execute_batch(
        "DELETE FROM intents WHERE kind='amended_pr_completion' AND attempt_id='attempt-second'",
    )
    .unwrap();
    assert!(
        luthor::state::verify_amended_observation_plan(
            &database(&lane),
            lane.f.store.root(),
            &plan
        )
        .is_err()
    );
}
