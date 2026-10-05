use super::{FakeGithub, FakeLauncher, FakePr, FakeWriter, FixedIds, Fixture};
use luthor::state::{scheduling, task_records};
use luthor::{
    coordinator::{
        AttemptReview, ScheduleDependencies, schedule_candidates,
        schedule_candidates_after_startup, startup_reconcile_all,
    },
    state::StateError,
};

pub(crate) fn startup_reconcile_holds_missing_receipt_without_relaunching() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    f.run("task-a", &c, &mut github, &mut writer, &mut launcher)
        .unwrap();
    let launches = launcher.plans.len();
    let mut projects = FakeGithub::new(&f.candidate);
    let report = startup_reconcile_all(&mut f.store, &mut projects, &mut github.prs).unwrap();
    assert_eq!(report.attempts.len(), 1);
    assert!(matches!(report.attempts[0].review, AttemptReview::Held(_)));
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(launcher.plans.len(), launches);
}

pub(crate) fn unproven_stopped_attempt_never_reads_pr_or_releases_task() {
    let mut f = Fixture::new(1);
    f.stopped_but_unproven();
    let mut projects = FakeGithub::new(&f.candidate);
    let mut prs = FakePr::default();
    let result = luthor::coordinator::reconcile_with_pr(
        &mut f.store,
        "task-a",
        "attempt-task-a",
        &mut projects,
        &mut prs,
    )
    .unwrap();
    assert!(matches!(
        result,
        luthor::supervisor::Reconciliation::Held { .. }
    ));
    assert_eq!(prs.lookups, 0);
    assert_eq!(
        task_records::task_phase(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert!(matches!(
        scheduling::ensure_dispatch_capacity(&f.store),
        Err(StateError::Capacity { .. })
    ));
}

pub(crate) fn scheduler_dispatches_other_task_after_verified_pause_without_resuming_paused_task() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut other = f.candidate.clone();
    other.issue_node_id = "issue-2".into();
    other.item_id = "item-2".into();
    other.issue_number = 2;
    other.issue_url = "https://github.com/org/tracker/issues/2".into();
    let mut github = FakeGithub::new(&other);
    let mut prs = FakePr::default();
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let report = schedule_candidates(
        &mut f.store,
        vec![other],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut github,
            prs: &mut prs,
            assignments: &mut writer,
            launcher: &mut launcher,
            ids: &mut FixedIds::default(),
        },
    )
    .unwrap();
    assert!(!report.capacity_full);
    assert_eq!(report.launched.len(), 1);
    assert_eq!(
        task_records::task_phase(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("paused")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert!(matches!(
        scheduling::ensure_dispatch_capacity(&f.store),
        Err(luthor::state::StateError::Capacity { .. })
    ));
}

pub(crate) fn scheduler_uses_precomputed_startup_without_reconciling_again() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    f.run("task-a", &c, &mut github, &mut writer, &mut launcher)
        .unwrap();
    let latest = task_records::latest_attempt(&f.store, "task-a").unwrap();
    let mut projects = FakeGithub::new(&f.candidate);
    let mut prs = FakePr::default();
    let mut assignments = FakeWriter::default();
    let mut scheduler_launcher = FakeLauncher::default();
    let report = schedule_candidates_after_startup(
        &mut f.store,
        vec![],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut projects,
            prs: &mut prs,
            assignments: &mut assignments,
            launcher: &mut scheduler_launcher,
            ids: &mut FixedIds::default(),
        },
        Default::default(),
    )
    .unwrap();
    assert_eq!(report.startup, Default::default());
    assert!(report.launched.is_empty());
    assert_eq!(projects.reads, 0);
    assert_eq!(prs.lookups, 0);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a").unwrap(),
        latest
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
}

pub(crate) fn scheduler_with_precomputed_blocked_startup_does_not_select_candidates() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let mut projects = FakeGithub::new(&c);
    let mut prs = FakePr::default();
    let mut assignments = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let startup = luthor::coordinator::StartupReport {
        source_holds: vec![luthor::coordinator::SourceHold {
            task_id: "task-source".into(),
            kind: "claim".into(),
        }],
        ..Default::default()
    };
    let report = schedule_candidates_after_startup(
        &mut f.store,
        vec![c],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut projects,
            prs: &mut prs,
            assignments: &mut assignments,
            launcher: &mut launcher,
            ids: &mut FixedIds::default(),
        },
        startup.clone(),
    )
    .unwrap();
    assert_eq!(report.startup, startup);
    assert!(report.launched.is_empty());
    assert_eq!(projects.reads, 0);
    assert_eq!(prs.lookups, 0);
    assert_eq!(assignments.calls, 0);
}
