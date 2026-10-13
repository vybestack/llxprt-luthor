//! Issue 51 through a whole execute cycle: finished tasks selected under an
//! older configuration must not hold, fail or block the rest of the cycle.
use super::*;
use crate::{model::VerifiedOpenPr, pr_repair::UNSPECIFIC_REPAIR_HOLD_REASON};

const TRACKER_ISSUE: &str = "https://github.com/org/tracker/issues/12";
const LIST_ROUTE: &str =
    "\"repos/org/code/pulls?state=open&per_page=100&page=1\") printf '%s\\n' '[]' ;;";

fn pr_detail(open: bool) -> serde_json::Value {
    let mut detail = json!({
        "id":4242,"number":42,"state":"open","draft":false,"merged":false,
        "html_url":"https://github.com/org/code/pull/42",
        "body":format!("Tracker-Issue: {TRACKER_ISSUE}"),
        "created_at":"2026-01-01T00:00:00Z",
        "base":{"repo":{"id":10,"full_name":"org/code"},"ref":"main"},
        "head":{"repo":{"id":10,"full_name":"org/code"},"ref":"issue-12","sha":"abc123"},
        "user":{"login":"acoliver"}
    });
    if !open {
        detail["state"] = json!("closed");
        detail["merged"] = json!(true);
        detail["closed_at"] = json!("2026-02-01T00:00:00Z");
        detail["merged_at"] = json!("2026-02-01T00:00:00Z");
    }
    detail
}

/// The hermetic `gh` serves PR 42 as open, or as merged with no open PR listed.
fn serve_pr_42(f: &Fixture, open: bool) {
    let list = if open {
        json!([{"number":42,"body":format!("Tracker-Issue: {TRACKER_ISSUE}")}])
    } else {
        json!([])
    };
    let routes = format!(
        "\"repos/org/code/pulls?state=open&per_page=100&page=1\") printf '%s\\n' '{list}' ;;\n  repos/org/code/pulls/42) printf '%s\\n' '{}' ;;",
        pr_detail(open)
    );
    let script = fs::read_to_string(&f.gh).unwrap();
    assert!(script.contains(LIST_ROUTE));
    fs::write(&f.gh, script.replace(LIST_ROUTE, &routes)).unwrap();
}

/// Issue 12 as an earlier release left it: worker finished, PR verified.
fn seed_finished_issue12(f: &Fixture) {
    let mut store = StateStore::open(&f.config.state_root, 2).unwrap();
    let mut projects = GhProjectReader::new(f.gh.clone());
    let mut prs = GhPullRequestReader::new(f.gh.clone());
    let mut assignments = GhAssignmentWriter {
        executable: f.gh.clone(),
    };
    let selected = eligibility::select_target(
        &mut projects,
        &f.config.sources,
        &f.config.mappings,
        "org/tracker",
        12,
    )
    .unwrap();
    dispatch_one(
        &mut store,
        &selected[0],
        DispatchDependencies {
            config: &f.config,
            config_revision: "test-revision",
            task_id: "task-12",
            attempt_id: "attempt-12",
            projects: &mut projects,
            prs: &mut prs,
            assignments: &mut assignments,
            launcher: &mut RecordingLauncher::default(),
        },
    )
    .unwrap();
    let saved = VerifiedOpenPr {
        id: 4242,
        number: 42,
        url: "https://github.com/org/code/pull/42".into(),
        repository_id: 10,
        repository: "org/code".into(),
        head_repository_id: 10,
        head_repository: "org/code".into(),
        base_branch: "main".into(),
        head_branch: "issue-12".into(),
        author: "acoliver".into(),
        active_login: "acoliver".into(),
        tracker_issue_url: TRACKER_ISSUE.into(),
        draft: false,
        checks: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        head_commit_sha: "abc123".into(),
        observed_at: 1,
        attempt_id: "attempt-12".into(),
    };
    store
        .connection
        .execute_batch(
            "UPDATE attempts SET lifecycle='completed', outcome='exited' WHERE id='attempt-12';
             UPDATE reservations SET status='released' WHERE attempt_id='attempt-12';
             UPDATE tasks SET state='pr_complete' WHERE id='task-12';",
        )
        .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-12','attempt-12','verified_open_pr',?1)",
            [serde_json::to_string(&saved).unwrap()],
        )
        .unwrap();
}

/// The cycle's configuration differs from the one task-12 was selected under.
fn newer_config(f: &Fixture) -> Config {
    let mut config = f.config.clone();
    config.min_free_space_bytes = 1;
    config.initial.args[5] = "Continue issue, newer prompt".into();
    config
}

fn phase_of_12(config: &Config) -> String {
    let store = StateStore::open(&config.state_root, 2).unwrap();
    task_records::task_phase(&store, "task-12")
        .unwrap()
        .unwrap()
}

fn assert_only_331_dispatched(launcher: &RecordingLauncher, report: &ScheduleReport) {
    assert_eq!(launcher.plans.len(), 1);
    assert_eq!(launcher.plans[0].task_id, "task-fixture-id");
    assert_eq!(report.launched, launcher.plans);
    assert!(report.startup.source_holds.is_empty());
}

#[test]
fn merged_pr_with_older_config_neither_fails_the_cycle_nor_holds_the_task() {
    let f = Fixture::new();
    seed_finished_issue12(&f);
    serve_pr_42(&f, false);
    let before = history(
        &StateStore::open(&f.config.state_root, 2).unwrap(),
        Some("task-12"),
    );
    let config = newer_config(&f);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&config, &mut launcher).unwrap().unwrap();
    assert_only_331_dispatched(&launcher, &report);
    assert!(report.pr_repair.skipped.is_empty() && report.pr_repair.held.is_empty());
    assert_eq!(phase_of_12(&config), "pr_complete");
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    assert_eq!(history(&store, Some("task-12")), before);
}

#[test]
fn open_pr_with_older_config_is_reported_skipped_while_other_work_dispatches() {
    let f = Fixture::new();
    seed_finished_issue12(&f);
    serve_pr_42(&f, true);
    let before = history(
        &StateStore::open(&f.config.state_root, 2).unwrap(),
        Some("task-12"),
    );
    let config = newer_config(&f);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&config, &mut launcher).unwrap().unwrap();
    assert_only_331_dispatched(&launcher, &report);
    assert!(report.pr_repair.held.is_empty());
    assert_eq!(report.pr_repair.skipped.len(), 1);
    assert_eq!(report.pr_repair.skipped[0].task_id, "task-12");
    assert_eq!(
        report.pr_repair.skipped[0].reason,
        "selection config differs: min_free_space_bytes, initial.args"
    );
    let reported = serde_json::to_value(&report.pr_repair).unwrap();
    assert_eq!(reported["skipped"][0]["task_id"], "task-12");
    assert_eq!(phase_of_12(&config), "pr_complete");
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    assert_eq!(history(&store, Some("task-12")), before);
}

#[test]
fn cycle_restores_a_wrongly_held_merged_task_before_scheduling() {
    let f = Fixture::new();
    seed_finished_issue12(&f);
    {
        let mut store = StateStore::open(&f.config.state_root, 2).unwrap();
        task_records::hold_task(&mut store, "task-12", UNSPECIFIC_REPAIR_HOLD_REASON).unwrap();
    }
    serve_pr_42(&f, false);
    let config = newer_config(&f);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&config, &mut launcher).unwrap().unwrap();
    assert_only_331_dispatched(&launcher, &report);
    assert_eq!(report.pr_repair.restored, ["task-12"]);
    assert!(report.pr_repair.restore_retained.is_empty());
    assert_eq!(phase_of_12(&config), "pr_complete");
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    let kinds = crate::state::journal::evidence_kinds(&store, "task-12").unwrap();
    assert!(kinds.contains(&"pr_hold_restored".into()));
    assert!(
        kinds.contains(&"held_reason".into()),
        "hold evidence is kept"
    );
}
