use super::*;
use luthor::WorktreeOwner;
use luthor::state::journal::{evidence_kinds, stop_intent};
use luthor::state::scheduling::{ensure_dispatch_capacity, reservation_count};
use luthor::state::task_records::{selection_evidence, task_phase};
use luthor::state::worktree_records::worktree_record;

#[cfg(unix)]
pub(crate) fn live_running_lost_claim_requests_stop_and_holds_reservation_until_exit() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    fs::write(&plan.executable, "#!/bin/sh\nexec /bin/sleep 15\n").unwrap();
    let child_path = config.state_root.join("attempts/attempt-real.child.json");
    let _guard = FixtureGroupGuard(child_path);
    let owner = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    execute_with_binary(
        &mut store,
        &plan,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);

    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Running
    );
    let selection = selection_evidence(&store, "task").unwrap().unwrap();
    let mut projects = OtherProject(selection.candidate, 0);
    let mut prs = ExitPr::default();
    let observed = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut projects,
        &mut prs,
    )
    .unwrap();

    assert_eq!(
        projects.1, 1,
        "claim read count after active reconciliation"
    );
    assert!(
        matches!(observed, Reconciliation::Held { .. }),
        "unexpected active reconciliation: {observed:?}"
    );
    assert_eq!(projects.1, 1, "the issue claim must be read once");
    assert_eq!(
        prs.reads, 1,
        "the PR lookup must occur while the worker runs"
    );
    assert!(
        stop_intent(&store, "task", "attempt-real")
            .unwrap()
            .is_some()
    );
    assert_eq!(reservation_count(&store).unwrap(), 1);
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { .. }
    ));
    assert_eq!(reservation_count(&store).unwrap(), 1);
    assert!(
        !evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"attempt_exit".into())
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let completed = loop {
        let result = reconcile_attempt(&mut store, "task", "attempt-real").unwrap();
        if matches!(result, Reconciliation::Completed { .. }) {
            break result;
        }
        assert!(Instant::now() < deadline, "worker did not exit after stop");
        thread::sleep(Duration::from_millis(10));
    };
    assert!(matches!(completed, Reconciliation::Completed { .. }));
    assert_eq!(reservation_count(&store).unwrap(), 0);
}

#[cfg(unix)]
fn launch_active_pr_worker(
    config: &Config,
    store: &mut StateStore,
    plan: &luthor::supervisor::LaunchPlan,
) -> FixtureGroupGuard {
    let child_path = config.state_root.join("attempts/attempt-real.child.json");
    let guard = FixtureGroupGuard(child_path);
    let owner = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    execute_with_binary(store, plan, Path::new(env!("CARGO_BIN_EXE_luthor")), &owner).unwrap();
    drop(owner);
    guard
}

#[cfg(unix)]
pub(crate) fn live_matching_pr_requests_stop_but_waits_for_exit_and_independent_rechecks() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    fs::write(&plan.executable, "#!/bin/sh\nexec /bin/sleep 15\n").unwrap();
    let _worker_guard = launch_active_pr_worker(&config, &mut store, &plan);

    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Running
    );
    let selection = selection_evidence(&store, "task").unwrap().unwrap();
    let identity = worktree_record(&store, "task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url.clone(), identity.branch)),
        author: Some("operator".into()),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(selection.candidate, 1);
    assert!(matches!(
        luthor::coordinator::reconcile_with_pr(
            &mut store,
            "task",
            "attempt-real",
            &mut projects,
            &mut prs,
        )
        .unwrap(),
        Reconciliation::Held { .. }
    ));

    assert_eq!(projects.1, 2, "active claim was not checked");
    assert_eq!(prs.reads, 1, "matching PR was not looked up while Running");
    assert!(
        stop_intent(&store, "task", "attempt-real")
            .unwrap()
            .is_some()
    );
    assert_eq!(task_phase(&store, "task").unwrap().as_deref(), Some("held"));
    assert_eq!(reservation_count(&store).unwrap(), 1);
    assert!(
        !evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"attempt_exit".into())
    );
    assert!(
        !evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"verified_open_pr".into())
    );
    assert!(matches!(
        ensure_dispatch_capacity(&store),
        Err(StateError::Capacity { .. })
    ));

    let deadline = Instant::now() + Duration::from_secs(5);
    let completed = loop {
        let result = reconcile_attempt(&mut store, "task", "attempt-real").unwrap();
        if matches!(result, Reconciliation::Completed { .. }) {
            break result;
        }
        assert!(Instant::now() < deadline, "worker did not exit after stop");
        thread::sleep(Duration::from_millis(10));
    };
    assert!(matches!(completed, Reconciliation::Completed { .. }));
    assert_eq!(reservation_count(&store).unwrap(), 0);
    assert!(
        !evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"verified_open_pr".into())
    );
    assert_ne!(
        task_phase(&store, "task").unwrap().as_deref(),
        Some("pr_complete")
    );

    assert_independent_pr_recheck(&config, &mut store, &mut projects, &mut prs);
}

#[cfg(unix)]
fn assert_independent_pr_recheck(
    config: &Config,
    store: &mut StateStore,
    projects: &mut OtherProject,
    prs: &mut ExitPr,
) {
    let result =
        luthor::coordinator::reconcile_with_pr(store, "task", "attempt-real", projects, prs)
            .unwrap();
    assert!(matches!(result, Reconciliation::Completed { .. }));
    assert_eq!(
        projects.1, 3,
        "claim was not independently rechecked after stop"
    );
    assert_eq!(
        prs.reads, 2,
        "PR was not independently rechecked after stop"
    );
    assert_eq!(pause_proof(config).status, PausePrStatus::Open);
    assert!(
        evidence_kinds(store, "task")
            .unwrap()
            .contains(&"verified_open_pr".into())
    );
    assert_eq!(
        task_phase(store, "task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert_eq!(reservation_count(store).unwrap(), 0);
}
