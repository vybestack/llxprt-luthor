use super::*;
use luthor::state::launches::launch_intent;
use luthor::state::scheduling::reservation_count;
use luthor::state::task_records::{
    existing_issue, held_reason, latest_attempt, source_claim_intent, task_count, task_phase,
};
use luthor::state::worktree_records::worktree_record;

#[cfg(unix)]
fn unsafe_attempt_storage(f: &Fixture, failure: &str) -> std::path::PathBuf {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};

    let attempts = f.config.state_root.join("attempts");
    fs::DirBuilder::new().mode(0o755).create(&attempts).unwrap();
    match failure {
        "permissions" => fs::set_permissions(&attempts, fs::Permissions::from_mode(0o755)).unwrap(),
        "symlink" | "dangling-symlink" => {
            fs::remove_dir(&attempts).unwrap();
            let target = f.dir.path().join("private-target");
            if failure == "symlink" {
                fs::DirBuilder::new().mode(0o700).create(&target).unwrap();
            }
            symlink(target, &attempts).unwrap();
        }
        "wrong-type" => {
            fs::remove_dir(&attempts).unwrap();
            fs::write(&attempts, "not a directory").unwrap();
            fs::set_permissions(&attempts, fs::Permissions::from_mode(0o700)).unwrap();
        }
        _ => unreachable!(),
    }
    attempts
}

#[test]
#[cfg(unix)]
fn long_stop_socket_path_blocks_dispatch_before_claim_or_reservation() {
    let mut f = Fixture::new(1);
    let long_root = f.dir.path().join("s".repeat(120));
    f.config.state_root = long_root;
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let result = f.run("task-a", &c, &mut github, &mut writer, &mut launcher);
    assert!(matches!(
        result,
        Err(DispatchError::Supervisor(
            SupervisorError::StopSocketPathTooLong
        ))
    ));
    assert_eq!(writer.calls, 0);
    assert!(launcher.plans.is_empty());
    assert_eq!(reservation_count(&f.store).unwrap(), 0);
    assert!(!f.config.worktree_root.exists());
    assert_eq!(github.reads, 0);
    assert_eq!(github.prs.lookups, 0);
    assert!(latest_attempt(&f.store, "task-a").unwrap().is_none());
    assert!(source_claim_intent(&f.store, "task-a").unwrap().is_none());
    assert_eq!(
        held_reason(&f.store, "task-a").unwrap().as_deref(),
        Some("launch preflight socket path too long")
    );
}

#[test]
fn worktree_preflight_failures_precede_assignment_and_attempts() {
    for failure in ["base", "origin", "branch"] {
        let mut f = Fixture::new(1);
        let checkout = f.candidate.mapping.checkout.clone();
        match failure {
            "base" => git(&checkout, &["branch", "-m", "missing-base"]),
            "origin" => git(
                &checkout,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "git@github.com:org/wrong.git",
                ],
            ),
            "branch" => git(&checkout, &["branch", "luthor/task-a"]),
            _ => unreachable!(),
        }
        let c = f.candidate.clone();
        let mut github = FakeGithub::new(&c);
        let mut writer = FakeWriter::default();
        let mut launcher = FakeLauncher::default();
        assert!(
            matches!(
                f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
                Err(DispatchError::Worktree(_))
            ),
            "{failure}"
        );
        assert_eq!(writer.calls, 0, "{failure}");
        assert!(!f.config.worktree_root.exists(), "{failure}");
    }
}

#[cfg(unix)]
#[test]
fn unsafe_attempt_storage_fails_before_assignment_worktree_or_launch() {
    use std::os::unix::fs::PermissionsExt;

    for failure in ["permissions", "symlink", "dangling-symlink", "wrong-type"] {
        let mut f = Fixture::new(2);
        let attempts = unsafe_attempt_storage(&f, failure);
        let before = fs::symlink_metadata(&attempts).unwrap();

        let c = f.candidate.clone();
        let mut github = FakeGithub::new(&c);
        let mut writer = FakeWriter::default();
        let mut launcher = FakeLauncher::default();
        assert!(
            matches!(
                f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
                Err(DispatchError::Supervisor(SupervisorError::Conflict))
            ),
            "{failure}"
        );
        assert_eq!(writer.calls, 0, "{failure}");
        assert!(launcher.plans.is_empty(), "{failure}");
        assert_eq!(reservation_count(&f.store).unwrap(), 0, "{failure}");
        assert!(
            launch_intent(&f.store, "attempt-task-a").unwrap().is_none(),
            "{failure}"
        );
        assert!(latest_attempt(&f.store, "task-a").unwrap().is_none());
        assert!(source_claim_intent(&f.store, "task-a").unwrap().is_none());
        assert!(worktree_record(&f.store, "task-a").unwrap().is_none());
        assert_eq!(
            task_phase(&f.store, "task-a").unwrap().as_deref(),
            Some("held")
        );
        assert_eq!(
            held_reason(&f.store, "task-a").unwrap().as_deref(),
            Some("launch preflight conflict")
        );
        assert!(!f.config.worktree_root.exists(), "{failure}");
        assert_eq!(github.reads, 0, "{failure}");
        assert_eq!(github.prs.lookups, 0, "{failure}");
        let after = fs::symlink_metadata(&attempts).unwrap();
        assert_eq!(before.file_type(), after.file_type());
        assert_eq!(before.permissions().mode(), after.permissions().mode());
        match failure {
            "permissions" => assert_eq!(fs::read_dir(&attempts).unwrap().count(), 0),
            "symlink" | "dangling-symlink" => {
                assert_eq!(
                    fs::read_link(&attempts).unwrap(),
                    f.dir.path().join("private-target")
                );
                if failure == "symlink" {
                    assert_eq!(
                        fs::read_dir(f.dir.path().join("private-target"))
                            .unwrap()
                            .count(),
                        0
                    );
                } else {
                    assert!(!f.dir.path().join("private-target").exists());
                }
            }
            "wrong-type" => assert_eq!(fs::read_to_string(&attempts).unwrap(), "not a directory"),
            _ => unreachable!(),
        }
    }
}

#[cfg(unix)]
#[test]
fn unsafe_attempt_storage_stops_scheduler_before_second_candidate() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let mut f = Fixture::new(2);
    let attempts = f.config.state_root.join("attempts");
    fs::DirBuilder::new().mode(0o755).create(&attempts).unwrap();
    fs::set_permissions(&attempts, fs::Permissions::from_mode(0o755)).unwrap();
    let first = f.candidate.clone();
    let mut second = first.clone();
    second.issue_node_id = "issue-2".into();
    second.item_id = "item-2".into();
    second.issue_number = 2;
    second.issue_url = "https://github.com/org/tracker/issues/2".into();
    let mut projects = FakeGithub::new(&first);
    let mut prs = FakePr::default();
    let mut assignments = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let mut ids = FixedIds::default();
    let result = schedule_candidates(
        &mut f.store,
        vec![first, second.clone()],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut projects,
            prs: &mut prs,
            assignments: &mut assignments,
            launcher: &mut launcher,
            ids: &mut ids,
        },
    );
    assert!(matches!(
        result,
        Err(luthor::coordinator::ScheduleError::Dispatch(
            DispatchError::Supervisor(SupervisorError::Conflict)
        ))
    ));
    assert_eq!(ids.0, 2);
    assert_eq!(task_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_phase(&f.store, "task-1").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(
        held_reason(&f.store, "task-1").unwrap().as_deref(),
        Some("launch preflight conflict")
    );
    assert!(!existing_issue(&f.store, &second.tracker_repo_id, &second.issue_node_id).unwrap());
    assert!(latest_attempt(&f.store, "task-1").unwrap().is_none());
    assert!(launch_intent(&f.store, "attempt-2").unwrap().is_none());
    assert!(source_claim_intent(&f.store, "task-1").unwrap().is_none());
    assert!(worktree_record(&f.store, "task-1").unwrap().is_none());
    assert_eq!(reservation_count(&f.store).unwrap(), 0);
    assert_eq!(projects.reads, 0);
    assert_eq!(prs.lookups, 0);
    assert_eq!(assignments.calls, 0);
    assert!(launcher.plans.is_empty());
    assert!(!f.config.worktree_root.exists());
    assert_eq!(fs::read_dir(&attempts).unwrap().count(), 0);
    assert_eq!(
        fs::metadata(&attempts).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn unavailable_attempt_storage_records_bounded_preflight_diagnostic() {
    let mut f = Fixture::new(1);
    f.config.state_root = f.config.state_root.join("missing-secret-path");
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::Supervisor(SupervisorError::Io(_)))
    ));
    assert_eq!(
        held_reason(&f.store, "task-a").unwrap().as_deref(),
        Some("launch preflight storage unavailable")
    );
    assert_eq!(
        task_phase(&f.store, "task-a").unwrap().as_deref(),
        Some("held")
    );
    assert!(latest_attempt(&f.store, "task-a").unwrap().is_none());
    assert!(source_claim_intent(&f.store, "task-a").unwrap().is_none());
    assert!(worktree_record(&f.store, "task-a").unwrap().is_none());
    assert!(launch_intent(&f.store, "attempt-task-a").unwrap().is_none());
    assert_eq!(reservation_count(&f.store).unwrap(), 0);
    assert_eq!(github.reads, 0);
    assert_eq!(github.prs.lookups, 0);
    assert_eq!(writer.calls, 0);
    assert!(launcher.plans.is_empty());
    assert!(!f.config.worktree_root.exists());
    assert!(!f.config.state_root.exists());
}

#[test]
fn supervisor_launch_errors_do_not_record_preflight_diagnostics() {
    for error in [
        SupervisorError::Conflict,
        SupervisorError::Io(std::io::Error::other("secret launch error")),
    ] {
        let mut f = Fixture::new(1);
        let c = f.candidate.clone();
        let mut github = FakeGithub::new(&c);
        let mut writer = FakeWriter::default();
        let mut launcher = FakeLauncher {
            failure: Some(error),
            ..Default::default()
        };
        assert!(matches!(
            f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
            Err(DispatchError::Supervisor(_))
        ));
        assert_eq!(
            held_reason(&f.store, "task-a").unwrap().as_deref(),
            Some("launch preparation or dispatch failed")
        );
        assert_eq!(writer.calls, 1);
        assert_eq!(launcher.plans.len(), 1);
        assert!(launch_intent(&f.store, "attempt-task-a").unwrap().is_some());
        assert_eq!(reservation_count(&f.store).unwrap(), 1);
    }
}

#[cfg(unix)]
#[test]
fn missing_attempt_storage_is_created_private_before_normal_dispatch() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mut f = Fixture::new(1);
    let attempts = f.config.state_root.join("attempts");
    assert!(!attempts.exists());
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let plan = f
        .run("task-a", &c, &mut github, &mut writer, &mut launcher)
        .unwrap();
    let metadata = fs::symlink_metadata(attempts).unwrap();
    assert!(metadata.file_type().is_dir());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(writer.calls, 1);
    assert_eq!(launcher.plans, [plan]);
    assert!(launch_intent(&f.store, "attempt-task-a").unwrap().is_some());
    assert_eq!(reservation_count(&f.store).unwrap(), 1);
}
