use super::*;
use crate::pin_adoption_fixture as adoption;
use luthor::state::{launches, scheduling, task_records};

#[test]
fn pin_adoption_enables_legacy_retry_and_real_worker_gate_after_hold() {
    let (_dir, config, mut store) = retry_fixture();
    let build = adoption::legacy(&config, &store);
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let mut projects = OtherProject(selection.candidate, 1);
    let mut prs = ExitPr::default();
    let mut launcher = OtherLauncher::default();
    assert!(matches!(
        retry(&mut store, &config, &mut projects, &mut prs, &mut launcher),
        Err(luthor::coordinator::DispatchError::Supervisor(
            SupervisorError::WorkerBuildHeld(_)
        ))
    ));
    assert_eq!(launcher.0, 0);
    let old_plan = launches::launch_intent(&store, "attempt-real")
        .unwrap()
        .unwrap();
    let history = adoption::rows(&config);
    adoption::adopt(&mut store, &build);
    assert_eq!(adoption::rows(&config), history);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("attention")
    );
    drop(store);
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    let next = retry(&mut store, &config, &mut projects, &mut prs, &mut launcher).unwrap();
    assert_eq!(next.worker_build, Some(build.clone()));
    assert_eq!(next.executable, build.path);
    assert_eq!(next.session_id, "task");
    let owner = luthor::WorktreeOwner::acquire_existing(store.root(), "task").unwrap();
    execute_with_binary(
        &mut store,
        &next,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    crate::worker_exit::await_dispatched_worker_exit(&config, "task", &next.attempt_id);
    assert!(matches!(
        luthor::coordinator::reconcile_with_pr(
            &mut store,
            "task",
            &next.attempt_id,
            &mut projects,
            &mut prs
        )
        .unwrap(),
        Reconciliation::Completed {
            exit_code: Some(2),
            signal: None
        }
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert_eq!(
        launches::launch_intent(&store, "attempt-real")
            .unwrap()
            .unwrap(),
        old_plan
    );
}
