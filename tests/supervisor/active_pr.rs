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
    sleeping_behavior(&plan);
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
pub(crate) fn live_matching_pr_finishes_naturally_and_independently_rechecks() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    let release = dir.path().join("finish");
    fs::write(
        plan.executable.with_extension("behavior"),
        format!(
            "#!/bin/sh
while ! test -f '{}'; do sleep 0.01; done
printf 'final summary\n'
",
            release.display()
        ),
    )
    .unwrap();
    let _worker_guard = launch_active_pr_worker(&config, &mut store, &plan);
    let selection = selection_evidence(&store, "task").unwrap().unwrap();
    let identity = worktree_record(&store, "task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url.clone(), identity.branch)),
        sha: Some(identity.head),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(selection.candidate, 1);
    for _ in 0..2 {
        assert_eq!(
            luthor::coordinator::reconcile_with_pr(
                &mut store,
                "task",
                "attempt-real",
                &mut projects,
                &mut prs
            )
            .unwrap(),
            Reconciliation::Running
        );
        assert!(
            stop_intent(&store, "task", "attempt-real")
                .unwrap()
                .is_none()
        );
        assert_eq!(reservation_count(&store).unwrap(), 1);
        assert!(ensure_dispatch_capacity(&store).is_err());
        assert!(
            !evidence_kinds(&store, "task")
                .unwrap()
                .contains(&"verified_open_pr".into())
        );
    }
    fs::write(release, "finish").unwrap();
    await_natural_exit(&config, &store);
    assert_eq!(
        luthor::coordinator::reconcile_with_pr(
            &mut store,
            "task",
            "attempt-real",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        Reconciliation::Completed {
            exit_code: Some(0),
            signal: None
        }
    );
    assert_natural_completion(&config, &store, &projects, &prs);
}

fn assert_natural_completion(
    config: &Config,
    store: &StateStore,
    projects: &OtherProject,
    prs: &ExitPr,
) {
    assert_eq!(projects.1, 4, "claim independently rechecked on exit");
    assert_eq!(prs.reads, 3, "PR independently rechecked on exit");
    assert_eq!(
        task_phase(store, "task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert_eq!(reservation_count(store).unwrap(), 0);
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt_path(config)).unwrap()).unwrap();
    assert!(receipt.stop_signals.is_empty());
    assert_eq!(
        fs::read_to_string(receipt.stdout_path).unwrap(),
        "final summary\n"
    );
}

fn await_natural_exit(config: &Config, store: &StateStore) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !receipt_path(config).exists() {
        assert!(Instant::now() < deadline, "worker did not exit naturally");
        thread::sleep(Duration::from_millis(10));
    }
    crate::supervisor_support::stop_views::completion::wait_for_fixture_exit(config, store);
}

fn sleeping_behavior(plan: &luthor::supervisor::LaunchPlan) {
    fs::write(
        plan.executable.with_extension("behavior"),
        "#!/bin/sh\nexec /bin/sleep 15\n",
    )
    .unwrap();
}
