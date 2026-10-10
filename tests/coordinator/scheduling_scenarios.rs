use super::{FakeGithub, FakeLauncher, FakePr, FakeWriter, FixedIds, Fixture};
use luthor::state::{scheduling, task_records};
use luthor::{
    coordinator::{
        AttemptReview, ScheduleDependencies, schedule_candidates,
        schedule_candidates_after_startup, startup_reconcile_all,
    },
    state::StateError,
};
use std::os::unix::fs::PermissionsExt;

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

fn assert_private_root_and_branch_free_templates(f: &Fixture) {
    assert_eq!(
        std::fs::metadata(&f.config.state_root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(
        f.config
            .initial
            .args
            .iter()
            .chain(&f.config.resume.args)
            .all(|arg| arg != "--branch")
    );
}

fn assert_unproven_launch_has_no_child_receipts_or_branch_args(f: &Fixture) {
    let attempts = f.config.state_root.join("attempts");
    assert!(!attempts.join("attempt-task-a.child.json").exists());
    assert!(!attempts.join("attempt-task-a.receipt.json").exists());
    assert!(!attempts.join("attempt-task-a.dispatch.json").exists());
    assert!(
        f.config
            .initial
            .args
            .iter()
            .chain(&f.config.resume.args)
            .all(|arg| arg != "--branch")
    );
}

pub(crate) fn scheduler_dispatches_second_issue_while_unproven_launch_intent_stays_reserved() {
    let mut f = Fixture::new(2);
    let first = f.candidate.clone();
    let mut first_github = FakeGithub::new(&first);
    let mut first_writer = FakeWriter::default();
    let mut failed_launcher = FakeLauncher {
        fail: true,
        ..Default::default()
    };
    assert!(
        f.run(
            "task-a",
            &first,
            &mut first_github,
            &mut first_writer,
            &mut failed_launcher
        )
        .is_err()
    );
    let attempt_before = task_records::latest_attempt(&f.store, "task-a").unwrap();
    assert_eq!(attempt_before.as_deref(), Some("attempt-task-a"));
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_private_root_and_branch_free_templates(&f);
    let original_launch = luthor::state::launches::launch_intent(&f.store, "attempt-task-a")
        .unwrap()
        .unwrap();

    let mut second = first.clone();
    second.issue_node_id = "issue-331".into();
    second.item_id = "item-331".into();
    second.issue_number = 331;
    second.issue_url = "https://github.com/org/tracker/issues/331".into();
    let mut startup_projects = FakeGithub::new(&first);
    let mut startup_prs = FakePr::default();
    let startup =
        startup_reconcile_all(&mut f.store, &mut startup_projects, &mut startup_prs).unwrap();
    assert_eq!(startup.attempts.len(), 1);
    assert!(matches!(startup.attempts[0].review, AttemptReview::Held(_)));
    assert_eq!(
        luthor::state::launches::launch_intent(&f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        original_launch
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_unproven_launch_has_no_child_receipts_or_branch_args(&f);
    let mut github = FakeGithub::new(&second);
    let mut prs = FakePr::default();
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let report = schedule_candidates_after_startup(
        &mut f.store,
        vec![second],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut github,
            prs: &mut prs,
            assignments: &mut writer,
            launcher: &mut launcher,
            ids: &mut FixedIds::default(),
        },
        startup,
    )
    .unwrap();
    assert_eq!(
        report.launched.len(),
        1,
        "issue 331 must reach the fake launcher"
    );
    assert_eq!(launcher.plans.len(), 1);
    assert!(luthor::state::task_records::existing_target(&f.store, "org/tracker", 331).unwrap());
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a").unwrap(),
        attempt_before
    );

    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 2);
}
#[cfg(target_os = "macos")]
fn seed_direct_root(f: &mut Fixture, root: &std::path::Path) -> String {
    use rusqlite::{Connection, OpenFlags};
    let candidate = f.candidate.clone();
    f.config.state_root = root.to_path_buf();
    f.store = luthor::state::StateStore::open(root, 2).unwrap();
    let mut github = FakeGithub::new(&candidate);
    let mut launcher = FakeLauncher {
        fail: true,
        ..Default::default()
    };
    assert!(
        f.run(
            "task-a",
            &candidate,
            &mut github,
            &mut FakeWriter::default(),
            &mut launcher
        )
        .is_err()
    );
    let original = luthor::state::launches::launch_intent(&f.store, "attempt-task-a")
        .unwrap()
        .unwrap();
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_unproven_launch_has_no_child_receipts_or_branch_args(f);
    let conn =
        Connection::open_with_flags(root.join("state.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM reservations WHERE status='reserved'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    original
}

#[cfg(target_os = "macos")]
fn assert_alias_refuses_launch(f: &mut Fixture, alias: &std::path::Path, original: &String) {
    f.config.state_root = alias.to_path_buf();
    let detached_root = f.dir.path().join("detached-state");
    f.store = luthor::state::StateStore::open(&detached_root, 2).unwrap();
    f.store = luthor::state::StateStore::open(alias, 2).unwrap();
    assert_eq!(
        luthor::state::launches::launch_intent(&f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        *original
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    let candidate = f.candidate.clone();
    let mut github = FakeGithub::new(&candidate);
    let mut launcher = FakeLauncher::default();
    assert!(
        f.run(
            "task-alias",
            &candidate,
            &mut github,
            &mut FakeWriter::default(),
            &mut launcher
        )
        .is_err()
    );
    assert_eq!(github.reads, 0);
    assert!(launcher.plans.is_empty());
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-task-a")
    );
}

#[cfg(target_os = "macos")]
fn assert_direct_root_holds_and_schedules_331(
    f: &mut Fixture,
    root: &std::path::Path,
    original: &String,
) {
    f.config.state_root = root.to_path_buf();
    let detached_root = f.dir.path().join("detached-state");
    f.store = luthor::state::StateStore::open(&detached_root, 2).unwrap();
    f.store = luthor::state::StateStore::open(root, 2).unwrap();
    assert_eq!(
        luthor::state::launches::launch_intent(&f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        *original
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    let mut projects = FakeGithub::new(&f.candidate);
    let mut prs = FakePr::default();
    let startup = startup_reconcile_all(&mut f.store, &mut projects, &mut prs).unwrap();
    assert!(matches!(startup.attempts[0].review, AttemptReview::Held(_)));
    assert_eq!(
        luthor::state::launches::launch_intent(&f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        *original
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    let mut second = f.candidate.clone();
    second.issue_node_id = "issue-331".into();
    second.item_id = "item-331".into();
    second.issue_number = 331;
    second.issue_url = "https://github.com/org/tracker/issues/331".into();
    let mut github = FakeGithub::new(&second);
    let mut prs = FakePr::default();
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let report = schedule_candidates_after_startup(
        &mut f.store,
        vec![second],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut github,
            prs: &mut prs,
            assignments: &mut writer,
            launcher: &mut launcher,
            ids: &mut FixedIds::default(),
        },
        startup,
    )
    .unwrap();
    assert_eq!(report.launched.len(), 1);
    assert_eq!(launcher.plans.len(), 1);
    assert!(task_records::existing_target(&f.store, "org/tracker", 331).unwrap());
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-task-a")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 2);
    assert_eq!(
        luthor::state::launches::launch_intent(&f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        *original
    );
}

#[cfg(target_os = "macos")]
pub(crate) fn reserved_launch_survives_darwin_final_component_alias_reopen_and_schedules_second_slot()
 {
    use std::os::unix::fs::symlink;
    let mut f = Fixture::new(2);
    let private_root = tempfile::tempdir_in("/private/var/tmp").unwrap();
    let root = private_root.path().join("state");
    let alias = private_root.path().join("state-alias");
    std::fs::create_dir(&root).unwrap();
    symlink(&root, &alias).unwrap();
    assert_eq!(std::fs::canonicalize(&alias).unwrap(), root);
    let original = seed_direct_root(&mut f, &root);
    assert_alias_refuses_launch(&mut f, &alias, &original);
    assert_direct_root_holds_and_schedules_331(&mut f, &root, &original);
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
