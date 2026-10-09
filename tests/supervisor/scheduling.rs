use super::*;
use luthor::state::{journal, scheduling, task_records};

#[cfg(unix)]
pub(crate) fn natural_exit_seven_attends_and_scheduler_dispatches_only_other_issue() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let mut prs = ExitPr::default();
    let (_, mut other) = configured(
        &std::env::current_dir()
            .unwrap()
            .join(config.state_root.parent().unwrap()),
    );
    other.item_id = "other-item".into();
    other.issue_node_id = "other-issue".into();
    other.issue_number = 8;
    other.issue_url = "https://github.com/org/tracker/issues/8".into();
    let mut project = OtherProject(other.clone(), 0);
    let mut writer = OtherWriter;
    let mut launcher = OtherLauncher::default();
    let mut ids = OtherIds::default();
    let report = schedule_candidates(
        &mut store,
        vec![other.clone()],
        ScheduleDependencies {
            config: &config,
            config_revision: "rev",
            projects: &mut project,
            prs: &mut prs,
            assignments: &mut writer,
            launcher: &mut launcher,
            ids: &mut ids,
        },
    )
    .unwrap();
    assert_eq!(prs.reads, 4); // one fresh exit read, then claim and prelaunch reads
    assert!(matches!(
        report.startup.attempts[0].review,
        luthor::coordinator::AttemptReview::Completed(Reconciliation::Completed {
            exit_code: Some(7),
            signal: None
        })
    ));
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("attention")
    );
    let proof = exit_proof(&config);
    assert_eq!(proof.status, PausePrStatus::Absent);
    assert!(proof.observed_at_unix_secs > 0);
    assert!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"attention_reason".into())
    );
    assert_eq!(
        task_records::latest_attempt(&store, "task")
            .unwrap()
            .as_deref(),
        Some("attempt-real")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1); // only the other issue
    assert_eq!(report.launched.len(), 1);
    assert_eq!(launcher.0, 1);
    assert_eq!(scheduling::pending_attempts(&store).unwrap().len(), 1); // other task awaits worker
    assert!(
        task_records::existing_issue(&store, &other.tracker_repo_id, &other.issue_node_id).unwrap()
    );
    assert_second_schedule(
        &mut store,
        other,
        ScheduleDependencies {
            config: &config,
            config_revision: "rev",
            projects: &mut project,
            prs: &mut prs,
            assignments: &mut writer,
            launcher: &mut launcher,
            ids: &mut ids,
        },
    );
    assert_eq!(launcher.0, 1);
}

#[cfg(unix)]
fn assert_second_schedule(
    store: &mut StateStore,
    other: Candidate,
    dependencies: ScheduleDependencies<
        '_,
        OtherProject,
        ExitPr,
        OtherWriter,
        OtherLauncher,
        OtherIds,
    >,
) {
    let again = schedule_candidates(store, vec![other], dependencies).unwrap();
    assert!(again.launched.is_empty());
    assert_eq!(
        task_records::latest_attempt(store, "task")
            .unwrap()
            .as_deref(),
        Some("attempt-real")
    );
}
