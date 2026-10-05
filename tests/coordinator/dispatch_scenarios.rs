use super::{FakeGithub, FakeLauncher, FakeWriter, Fixture};
use luthor::state::{launches, scheduling, task_records};
use luthor::{coordinator::DispatchError, state::StateError};
use std::fs;

pub(crate) fn verified_claim_and_worktree_precede_fake_launch() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let plan = f
        .run("task-a", &c, &mut github, &mut writer, &mut launcher)
        .unwrap();
    assert_eq!(writer.calls, 1);
    assert_eq!(github.reads, 3);
    assert_eq!(github.prs.lookups, 3);
    assert_eq!(launcher.plans.as_slice(), std::slice::from_ref(&plan));
    assert_eq!(
        plan.worktree,
        fs::canonicalize(f.config.worktree_root.join("task-a")).unwrap()
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert!(
        launches::launch_intent(&f.store, "attempt-task-a")
            .unwrap()
            .is_some()
    );
    assert!(f.dir.path().join("checkout").exists());
}

pub(crate) fn existing_pr_blocks_assignment_and_holds_selection() {
    let mut f = Fixture::new(2);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    github.prs.present_on = Some(1);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::Claim(_))
    ));
    assert_eq!(writer.calls, 0);
    assert!(launcher.plans.is_empty());
    assert_eq!(
        task_records::held_reason(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("claim failed")
    );
    assert!(!f.config.worktree_root.join("task-a").exists());
}

pub(crate) fn changed_claim_blocks_before_worktree_or_launch() {
    let mut f = Fixture::new(2);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    github.change_on = Some(3);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::ChangedClaim)
    ));
    assert_eq!(writer.calls, 1);
    assert!(launcher.plans.is_empty());
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert!(f.config.worktree_root.join("task-a").exists());
}

pub(crate) fn new_pr_or_failed_lookup_blocks_before_reservation() {
    for fail in [false, true] {
        let mut f = Fixture::new(2);
        let c = f.candidate.clone();
        let mut github = FakeGithub::new(&c);
        if fail {
            github.prs.fail_on = Some(3);
        } else {
            github.prs.present_on = Some(3);
        }
        let mut writer = FakeWriter::default();
        let mut launcher = FakeLauncher::default();
        let result = f.run("task-a", &c, &mut github, &mut writer, &mut launcher);
        assert!(if fail {
            matches!(result, Err(DispatchError::PullRequest(_)))
        } else {
            matches!(result, Err(DispatchError::ExistingPr))
        });
        assert_eq!(writer.calls, 1);
        assert!(launcher.plans.is_empty());
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_records::task_phase(&f.store, "task-a")
                .unwrap()
                .as_deref(),
            Some("held")
        );
    }
}

pub(crate) fn failed_launch_retains_reservation_and_second_schedule_cannot_launch() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher {
        fail: true,
        ..Default::default()
    };
    assert!(matches!(
        f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::Supervisor(_))
    ));
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::held_reason(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("launch preparation or dispatch failed")
    );
    let mut other = c.clone();
    other.issue_node_id = "issue-2".into();
    other.item_id = "item-2".into();
    other.issue_number = 2;
    other.issue_url = "https://github.com/org/tracker/issues/2".into();
    assert!(matches!(
        f.run("task-b", &other, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::State(StateError::Capacity { .. }))
    ));
    assert_eq!(writer.calls, 1);
    assert_eq!(launcher.plans.len(), 1);
    assert_eq!(task_records::task_count(&f.store).unwrap(), 1);
}

pub(crate) fn distinct_tasks_keep_selection_and_repository_issue_uniqueness() {
    let mut f = Fixture::new(3);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    f.run("task-a", &c, &mut github, &mut writer, &mut launcher)
        .unwrap();
    assert!(matches!(
        f.run("task-a-copy", &c, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::State(StateError::DuplicateTask(_, _)))
    ));
    let mut second = c.clone();
    second.item_id = "item-2".into();
    second.issue_node_id = "issue-2".into();
    second.issue_number = 2;
    second.issue_url = "https://github.com/org/tracker/issues/2".into();
    let mut github2 = FakeGithub::new(&second);
    f.run("task-b", &second, &mut github2, &mut writer, &mut launcher)
        .unwrap();
    assert_eq!(
        task_records::selection_evidence(&f.store, "task-a")
            .unwrap()
            .unwrap()
            .candidate,
        c
    );
    assert_eq!(
        task_records::selection_evidence(&f.store, "task-b")
            .unwrap()
            .unwrap()
            .candidate,
        second
    );
    assert_eq!(launcher.plans.len(), 2);
    assert_ne!(launcher.plans[0].session_id, launcher.plans[1].session_id);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 2);
}
