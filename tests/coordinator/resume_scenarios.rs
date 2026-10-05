use super::{FakeGithub, FakeLauncher, Fixture};
use luthor::state::{scheduling, task_records};
use luthor::{coordinator::DispatchError, state::StateError};

pub(crate) fn paused_resume_uses_stored_selection_and_launches_one_continuation() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut github = FakeGithub::new(&f.candidate);
    github.assigned = true;
    f.config.assignment_login = "other".into();
    f.config.resume.args.clear();
    let mut launcher = FakeLauncher::default();
    let plan = f.resume(&mut github, &mut launcher).unwrap();
    assert_eq!(github.reads, 1);
    assert_eq!(github.prs.lookups, 1);
    assert_eq!(launcher.plans.as_slice(), std::slice::from_ref(&plan));
    assert_eq!(plan.attempt_id, "attempt-next");
    assert_eq!(plan.session_id, "task-a");
    assert_eq!(plan.config_revision, "revision");
    let selection = task_records::selection_evidence(&f.store, "task-a")
        .unwrap()
        .unwrap();
    assert_eq!(selection.candidate.source, f.candidate.source);
    assert_eq!(selection.effective_config.assignment_login, "bot");
    assert!(plan.args.iter().any(|arg| arg.contains("continue")));
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-next")
    );
    assert_eq!(
        task_records::task_phase(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(launcher.plans.len(), 1);
}

pub(crate) fn resume_pr_present_or_failed_lookup_holds_without_new_attempt() {
    for fail in [false, true] {
        let mut f = Fixture::new(1);
        f.pause();
        let mut github = FakeGithub::new(&f.candidate);
        github.assigned = true;
        if fail {
            github.prs.fail_on = Some(1);
        } else {
            github.prs.present_on = Some(1);
        }
        let mut launcher = FakeLauncher::default();
        let result = f.resume(&mut github, &mut launcher);
        assert!(if fail {
            matches!(result, Err(DispatchError::PullRequest(_)))
        } else {
            matches!(result, Err(DispatchError::ExistingPr))
        });
        assert_eq!(
            task_records::held_reason(&f.store, "task-a")
                .unwrap()
                .as_deref(),
            Some(if fail {
                "resume PR read failed"
            } else {
                "resume PR present"
            })
        );
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_records::latest_attempt(&f.store, "task-a")
                .unwrap()
                .as_deref(),
            Some("attempt-task-a")
        );
        assert!(launcher.plans.is_empty());
    }
}

pub(crate) fn resume_changed_claim_holds_without_new_attempt() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut github = FakeGithub::new(&f.candidate);
    github.change_on = Some(1);
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::ChangedClaim)
    ));
    assert_eq!(github.prs.lookups, 0);
    assert_eq!(
        task_records::held_reason(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("resume claim changed")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-task-a")
    );
    assert!(launcher.plans.is_empty());
}

pub(crate) fn resume_without_paused_reconciled_state_cannot_create_attempt() {
    let mut f = Fixture::new(1);
    let mut github = FakeGithub::new(&f.candidate);
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(task_records::task_count(&f.store).unwrap(), 0);
    f.pause();
    task_records::set_task_phase(&mut f.store, "task-a", "held").unwrap();
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(github.reads, 0);
    assert_eq!(github.prs.lookups, 0);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-task-a")
    );
    assert!(launcher.plans.is_empty());
}

pub(crate) fn failed_resume_dispatch_retains_reservation_and_never_retries() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut github = FakeGithub::new(&f.candidate);
    github.assigned = true;
    let mut launcher = FakeLauncher {
        fail: true,
        ..Default::default()
    };
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::Supervisor(_))
    ));
    assert_eq!(launcher.plans.len(), 1);
    assert_eq!(
        task_records::held_reason(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("resume preparation or dispatch failed")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-a")
            .unwrap()
            .as_deref(),
        Some("attempt-next")
    );
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(launcher.plans.len(), 1);
}
