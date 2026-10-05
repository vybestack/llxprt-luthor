use super::{Lane, amend, database, dual_fixture, fixture};
use luthor::state::{exit_observation, journal, launches, scheduling, task_records};
use luthor::{state, supervisor};
use serde_json::json;

fn assert_retry_held(lane: &mut Lane) {
    let mut launcher = crate::FakeLauncher::default();
    let mut prs = crate::FakePr::default();
    let reads = lane.github.reads;
    let result = luthor::coordinator::retry_one(
        &mut lane.f.store,
        luthor::coordinator::RetryDependencies {
            task_id: "task-a",
            previous_attempt_id: "attempt-task-a",
            attempt_id: "attempt-next",
            config: &lane.f.config,
            config_revision: "corrected-revision",
            actor: "bot",
            reason: "explicit-request-does-not-supply-continuation-lineage",
            revalidate_terminal_exit: false,
            projects: &mut lane.github,
            prs: &mut prs,
            launcher: &mut launcher,
        },
    );
    assert!(matches!(
        result,
        Err(luthor::coordinator::DispatchError::State(
            state::StateError::LaunchBlocked
        ))
    ));
    assert!(launcher.plans.is_empty());
    assert_eq!(lane.github.reads, reads);
    assert_eq!(prs.lookups, 0);
}

fn terminalize(lane: &mut Lane, stopped: bool) {
    if stopped {
        journal::record_stop_intent(&mut lane.f.store, "task-a", "attempt-task-a").unwrap();
    }
    let receipt = json!({
        "attempt_id":"attempt-task-a", "child_pid":123, "boot_identity":"boot",
        "child_start_identity":"start", "exit_code":if stopped { None } else { Some(17) },
        "signal":if stopped { Some(15) } else { None },
        "stdout_path":"stdout", "stdout_bytes":0, "stderr_path":"stderr", "stderr_bytes":0,
        "stop_signals":if stopped { vec![15] } else { vec![] }
    });
    let db = database(lane);
    db.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-a','attempt-task-a','attempt_exit',?1)", [receipt.to_string()]).unwrap();
    let outcome = if stopped {
        "exit_code=None;signal=Some(15)"
    } else {
        "exit_code=Some(17);signal=None"
    };
    db.execute(
        "UPDATE attempts SET lifecycle='completed',outcome=?1",
        [outcome],
    )
    .unwrap();
    db.execute("UPDATE reservations SET status='released'", [])
        .unwrap();
    if stopped {
        exit_observation::record_pause_pr_lookup(
            &mut lane.f.store,
            "task-a",
            "attempt-task-a",
            &state::PausePrEvidence {
                observed_at_unix_secs: 2,
                repository: "org/code".into(),
                status: state::PausePrStatus::Absent,
            },
        )
        .unwrap();
    } else {
        journal::record_evidence(
            &mut lane.f.store,
            "task-a",
            Some("attempt-task-a"),
            "exit_pr_lookup",
            &serde_json::to_string(&super::super::pr()).unwrap(),
        )
        .unwrap();
        task_records::set_task_phase(&mut lane.f.store, "task-a", "attention").unwrap();
    }
}

#[test]
fn future_template_versions_cannot_supply_same_task_resume_or_retry_lineage() {
    for dual in [false, true] {
        for stopped in [false, true] {
            let mut lane = if dual { dual_fixture() } else { fixture() };
            amend(&mut lane).unwrap();
            terminalize(&mut lane, stopped);
            let launches: usize = database(&lane)
                .query_row(
                    "SELECT COUNT(*) FROM intents WHERE kind='launch'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            if stopped {
                assert!(
                    launches::resume_context(&lane.f.store, "task-a").is_err(),
                    "dual={dual}"
                );
                assert!(
                    supervisor::prepare_resume(&mut lane.f.store, "task-a", "attempt-next")
                        .is_err()
                );
            } else {
                assert!(
                    state::retry_context_for_task(&lane.f.store, "task-a", "attempt-task-a")
                        .is_err(),
                    "dual={dual}"
                );
                assert_retry_held(&mut lane);
            }
            assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
            assert_eq!(
                database(&lane)
                    .query_row::<usize, _, _>(
                        "SELECT COUNT(*) FROM intents WHERE kind='launch'",
                        [],
                        |r| r.get(0)
                    )
                    .unwrap(),
                launches
            );
        }
    }
}

#[test]
fn future_template_issue331_new_selection_uses_corrected_templates_without_worker_execution() {
    let mut original = dual_fixture();
    amend(&mut original).unwrap();
    let context = original
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    let corrected = &context.amendment().unwrap().current_config;
    let mut f = crate::Fixture::new(1);
    f.config.initial = corrected.initial.clone();
    f.config.resume = corrected.resume.clone();
    f.candidate.issue_number = 331;
    f.candidate.issue_url = "https://github.com/org/tracker/issues/331".into();
    f.pause();
    let selection = task_records::selection_evidence(&f.store, "task-a")
        .unwrap()
        .unwrap();
    assert_eq!(
        selection.effective_config,
        state::EffectiveConfigSnapshot::from(&f.config)
    );
    assert_eq!(selection.candidate.issue_number, 331);
    let saved: supervisor::LaunchPlan = serde_json::from_str(
        &launches::launch_intent(&f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        !saved
            .args
            .iter()
            .any(|arg| arg == "--branch" || arg.starts_with("--branch="))
    );
    let resumed = supervisor::prepare_resume(&mut f.store, "task-a", "attempt-next").unwrap();
    assert_eq!(resumed.executable, f.config.resume.executable);
    assert!(
        !resumed
            .args
            .iter()
            .any(|arg| arg == "--branch" || arg.starts_with("--branch="))
    );
    assert_eq!(resumed.session_id, saved.session_id);
    assert_eq!(resumed.worktree, saved.worktree);
    assert_eq!(resumed.config_revision, selection.config_revision);
    assert!(
        resumed
            .args
            .last()
            .unwrap()
            .contains("Fixes org/tracker#331")
    );
    assert_ne!(resumed.args.last(), saved.args.last());
}
