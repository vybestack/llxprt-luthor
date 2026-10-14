//! Issue 65: a failed discovery read must not stall known work or claim an unreadable issue.
use super::repairs::{newer_config, seed_finished_issue12, serve_pr_42_with};
use super::*;

fn fail_route(f: &Fixture, route: &str, once: bool) {
    let script = fs::read_to_string(&f.gh).unwrap();
    let marker = format!("{route})");
    assert_eq!(script.matches(&marker).count(), 1);
    let fail = if once {
        format!(
            "if ! test -f '{}'; then touch '{}'; printf 'HTTP 500' >&2; exit 1; fi;",
            f.root.path().join("read-failed").display(),
            f.root.path().join("read-failed").display()
        )
    } else {
        "printf 'HTTP 500' >&2; exit 1;".into()
    };
    fs::write(&f.gh, script.replace(&marker, &format!("{marker} {fail}"))).unwrap();
    fs::write(&f.calls, "").unwrap();
}

#[test]
fn candidate_500_skips_unverified_issue_but_dispatches_other_ready_work() {
    let f = Fixture::new();
    fail_route(&f, "repos/org/tracker/issues/12?per_page=100", false);
    let mut options = f.options();
    options.targets = None;
    let mut launcher = RecordingLauncher::default();
    let report = cycle_with_dependencies(
        &f.config,
        &options,
        f.gh.clone(),
        &mut launcher,
        &mut FixedIds,
    )
    .unwrap()
    .unwrap();
    assert_eq!(report.launched.len(), 1);
    assert_eq!(report.candidate_read_failures.len(), 1);
    assert_eq!(report.candidate_read_failures[0].issue, "ISSUE12");
    assert_eq!(report.candidate_read_failures[0].http_status, Some(500));
    assert!(f.assigned.exists());
    assert!(!f.assigned12.exists());
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.contains("issues/12?"))
            .count(),
        3
    );
    assert!(!calls.contains("POST repos/org/tracker/issues/12"));
}

#[test]
fn project_500_keeps_reconciliation_retirement_cleanup_and_repair_running() {
    let f = Fixture::new();
    seed_finished_issue12(&f);
    serve_pr_42_with(&f, true, true);
    fail_route(&f, "graphql", false);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&newer_config(&f), &mut launcher).unwrap().unwrap();
    assert!(report.launched.is_empty());
    assert_eq!(
        report.pr_repair.skipped.len(),
        1,
        "repair pass still handles task-12"
    );
    assert_eq!(report.pr_repair.skipped[0].task_id, "task-12");
    assert_eq!(report.source_read_failures.len(), 1);
    assert_eq!(report.source_read_failures[0].http_status, Some(500));
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(
        calls.contains("repos/org/code/pulls/42"),
        "retirement re-reads saved PR"
    );
    assert!(!calls.contains("-X POST"));
}

#[test]
fn project_500_then_200_retries_and_dispatches_with_no_failure_report() {
    let f = Fixture::new();
    fail_route(&f, "graphql", true);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&f.config, &mut launcher).unwrap().unwrap();
    assert_eq!(report.launched.len(), 1);
    assert!(report.source_read_failures.is_empty());
    assert!(report.candidate_read_failures.is_empty());
    assert!(f.assigned.exists());
}

#[test]
fn targeted_candidate_500_reports_success_without_claim_or_task() {
    let f = Fixture::new();
    fail_route(&f, "repos/org/tracker/issues/331?per_page=100", false);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&f.config, &mut launcher).unwrap().unwrap();
    assert!(report.launched.is_empty());
    assert_eq!(report.candidate_read_failures.len(), 1);
    assert!(!f.assigned.exists());
    assert!(
        scheduling::pending_attempts(&StateStore::open(&f.config.state_root, 2).unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn project_500_does_not_skip_reconciliation_of_existing_attempts() {
    let f = Fixture::new();
    let original = seed_held_issue12(&f);
    fail_route(&f, "graphql", false);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&f.config, &mut launcher).unwrap().unwrap();
    assert_eq!(report.startup.attempts.len(), 1);
    assert_eq!(report.startup.attempts[0].attempt_id, "attempt-12");
    assert!(matches!(
        report.startup.attempts[0].review,
        AttemptReview::Held(_)
    ));
    assert_eq!(report.source_read_failures.len(), 1);
    assert!(report.launched.is_empty());
    assert_saved_attempt(
        &StateStore::open(&f.config.state_root, 2).unwrap(),
        &original,
    );
}

#[test]
fn project_500_does_not_prevent_retiring_a_known_closed_issue() {
    let f = Fixture::new();
    seed_finished_issue12(&f);
    serve_pr_42_with(&f, true, true);
    {
        let mut store = StateStore::open(&f.config.state_root, 2).unwrap();
        for (kind, payload) in [
            ("attempt_exit", r#"{"stop_signals":[]}"#),
            ("exit_pr_lookup", r#"{"status":{"status":"open"}}"#),
        ] {
            store.connection.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-12','attempt-12',?1,?2)", [kind, payload]).unwrap();
        }
        task_records::hold_task(
            &mut store,
            "task-12",
            "PR repair held: issue claim changed or could not be re-read",
        )
        .unwrap();
    }
    let script = fs::read_to_string(&f.gh).unwrap();
    let script = [true, false].into_iter().fold(script, |script, assigned| {
        script.replace(
            &issue_json(12, assigned),
            &issue_json(12, assigned).replace(
                "\"state\":\"open\"",
                "\"state\":\"closed\",\"closed_at\":\"2026-02-01T00:00:03Z\"",
            ),
        )
    });
    fs::write(&f.gh, script).unwrap();
    fail_route(&f, "graphql", false);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&f.config, &mut launcher).unwrap().unwrap();
    assert_eq!(report.pr_repair.retired.len(), 1);
    assert_eq!(report.pr_repair.retired[0].task_id, "task-12");
    assert_eq!(
        report.pr_repair.retired[0].phase,
        crate::state::retirement::RetiredPhase::IssueClosed
    );
    assert_eq!(report.source_read_failures.len(), 1);
    assert!(report.launched.is_empty());
    assert!(!f.assigned.exists());
}
