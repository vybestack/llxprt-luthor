//! Issue 51 through a whole execute cycle: finished tasks selected under an
//! older configuration must not hold, fail or block the rest of the cycle.
use super::*;
use crate::{model::VerifiedOpenPr, pr_repair::UNSPECIFIC_REPAIR_HOLD_REASON};

const TRACKER_ISSUE: &str = "https://github.com/org/tracker/issues/12";
const LIST_ROUTE: &str =
    "\"repos/org/code/pulls?state=open&per_page=100&page=1\") printf '%s\\n' '[]' ;;";

pub(super) fn pr_detail(f: &Fixture, open: bool) -> serde_json::Value {
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    let branch = crate::state::worktree_records::worktree_record(&store, "task-12")
        .unwrap()
        .unwrap()
        .identity
        .unwrap()
        .branch;
    let mut detail = json!({
        "id":4242,"number":42,"state":"open","draft":false,"merged":false,
        "html_url":"https://github.com/org/code/pull/42",
        "body":format!("Tracker-Issue: {TRACKER_ISSUE}"),
        "created_at":"2026-01-01T00:00:00Z",
        "base":{"repo":{"id":10,"full_name":"org/code"},"ref":"main"},
        "head":{"repo":{"id":10,"full_name":"org/code"},"ref":branch,"sha":"a".repeat(40)},
        "user":{"login":"acoliver"}
    });
    if !open {
        detail["state"] = json!("closed");
        detail["merged"] = json!(true);
        detail["closed_at"] = json!("2026-02-01T00:00:00Z");
        detail["merged_at"] = json!("2026-02-01T00:00:00Z");
        detail["merge_commit_sha"] = json!("c".repeat(40));
    }
    detail
}

/// The hermetic `gh` serves PR 42 as open, or as merged with no open PR listed.
fn serve_pr_42(f: &Fixture, open: bool) {
    serve_pr_42_with(f, open, open);
}

/// The open-PR search and the read by id can disagree while GitHub catches up.
pub(super) fn serve_pr_42_with(f: &Fixture, listed_open: bool, detail_open: bool) {
    serve_pr_42_detail(f, listed_open, &pr_detail(f, detail_open));
}

/// Serves exactly this detail for PR 42, with the open-PR search listing it or not.
pub(super) fn serve_pr_42_detail(f: &Fixture, listed_open: bool, detail: &serde_json::Value) {
    let list = if listed_open {
        json!([{"number":42,"body":format!("Tracker-Issue: {TRACKER_ISSUE}")}])
    } else {
        json!([])
    };
    let routes = format!(
        "\"repos/org/code/pulls?state=open&per_page=100&page=1\") printf '%s\\n' '{list}' ;;\n  repos/org/code/pulls/42) printf '%s\\n' '{detail}' ;;"
    );
    let script = fs::read_to_string(&f.gh).unwrap();
    assert!(script.contains(LIST_ROUTE));
    fs::write(&f.gh, script.replace(LIST_ROUTE, &routes)).unwrap();
}

/// Issue 12 as an earlier release left it: worker finished, PR verified.
pub(super) fn seed_finished_issue12(f: &Fixture) {
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
    crate::state::journal::record_evidence(
        &mut store,
        "task-12",
        Some("attempt-12"),
        "attempt_exit",
        r#"{"stop_signals":[]}"#,
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
        head_branch: crate::state::worktree_records::worktree_record(&store, "task-12")
            .unwrap()
            .unwrap()
            .identity
            .unwrap()
            .branch,
        author: "acoliver".into(),
        active_login: "acoliver".into(),
        tracker_issue_url: TRACKER_ISSUE.into(),
        draft: false,
        checks: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        head_commit_sha: "a".repeat(40),
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
pub(super) fn newer_config(f: &Fixture) -> Config {
    let mut config = f.config.clone();
    config.min_free_space_bytes = 1;
    config.initial.args[5] = "Continue issue, newer prompt".into();
    config
}

pub(super) fn phase_of_12(config: &Config) -> String {
    let store = StateStore::open(&config.state_root, 2).unwrap();
    task_records::task_phase(&store, "task-12")
        .unwrap()
        .unwrap()
}

pub(super) fn assert_only_331_dispatched(launcher: &RecordingLauncher, report: &ScheduleReport) {
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
    assert_eq!(phase_of_12(&config), "pr_merged");
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    let after = history(&store, Some("task-12"));
    assert_eq!(&after[1..4], &before[1..4]);
    assert!(after[4].starts_with(&before[4]));
    assert_eq!(report.pr_merge.delivered.len(), 1);
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
    let mut store = StateStore::open(&f.config.state_root, 2).unwrap();
    let after = history(&store, Some("task-12"));
    assert_eq!(&after[..4], &before[..4]);
    assert!(after[4].starts_with(&before[4]));
    assert_eq!(after[4].len(), before[4].len() + 1);
    assert_eq!(
        after[4].last().unwrap()[3],
        rusqlite::types::Value::Text("repair_mapping_mismatch".into())
    );
    let calls = fs::read_to_string(&f.calls).unwrap();
    let again = crate::coordinator::repair_verified_prs(
        &mut store,
        &config,
        &mut GhProjectReader::new(f.gh.clone()),
        &mut GhPullRequestReader::new(f.gh.clone()),
        &mut launcher,
        &mut FixedIds,
    )
    .unwrap();
    assert!(again.skipped.is_empty() && again.held.is_empty() && again.launched.is_empty());
    assert_eq!(history(&store, Some("task-12")), after);
    assert_eq!(fs::read_to_string(&f.calls).unwrap(), calls);
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
    assert_eq!(phase_of_12(&config), "pr_merged");
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    let kinds = crate::state::journal::evidence_kinds(&store, "task-12").unwrap();
    assert!(kinds.contains(&"pr_hold_restored".into()));
    assert!(
        kinds.contains(&"held_reason".into()),
        "hold evidence is kept"
    );
}

#[test]
fn cycle_reports_and_persists_the_specific_repair_identity_hold() {
    let f = Fixture::new();
    seed_finished_issue12(&f);
    let mut detail = pr_detail(&f, true);
    detail["id"] = json!(9999);
    serve_pr_42_detail(&f, true, &detail);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&f.config, &mut launcher).unwrap().unwrap();
    assert_eq!(report.pr_repair.held.len(), 1);
    let reported = serde_json::to_value(&report.pr_repair).unwrap();
    let held = &reported["held"][0];
    assert_eq!(held["task_id"], "task-12");
    assert_eq!(held["reason"], "PR repair held: pr-identity-unverified");
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    assert_eq!(
        task_records::held_reason(&store, "task-12")
            .unwrap()
            .unwrap(),
        held["reason"].as_str().unwrap()
    );
    assert_eq!(
        task_records::task_phase(&store, "task-12")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert!(launcher.plans.iter().all(|plan| plan.task_id != "task-12"));
}
