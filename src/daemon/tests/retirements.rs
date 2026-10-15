//! Issue 54 through a whole execute cycle: tasks whose issue closed or whose PR
//! finished are retired before scheduling, and never block the queue.
use super::repairs::{
    assert_only_331_dispatched, newer_config, phase_of_12, seed_finished_issue12, serve_pr_42_with,
};
use super::*;

const CLAIM_CHANGED: &str = "PR repair held: issue claim changed or could not be re-read";
const POST_REPAIR: &str = "post-repair PR or claim verification failed; requires a person";

/// Issue 12 as GitHub reports it once closed by other work.
fn close_issue_12(f: &Fixture) {
    let closed = |assigned| {
        issue_json(12, assigned).replace(
            "\"state\":\"open\"",
            "\"state\":\"closed\",\"closed_at\":\"2026-02-01T00:00:03Z\"",
        )
    };
    let mut script = fs::read_to_string(&f.gh).unwrap();
    for assigned in [true, false] {
        let open = issue_json(12, assigned);
        assert!(script.contains(&open));
        script = script.replace(&open, &closed(assigned));
    }
    fs::write(&f.gh, script).unwrap();
}

/// Task 12 as a repair left it: its attempt ended with a verified exit and the
/// task was then held. The hold keeps it counted as unresolved.
fn held_after_exit(f: &Fixture, reason: &str) {
    seed_finished_issue12(f);
    let mut store = StateStore::open(&f.config.state_root, 2).unwrap();
    store.connection.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-12','attempt-12','exit_pr_lookup',?1)",
        [r#"{"status":{"status":"open"}}"#],
    ).unwrap();
    task_records::hold_task(&mut store, "task-12", reason).unwrap();
}

fn retired_in(report: &ScheduleReport) -> serde_json::Value {
    serde_json::to_value(&report.pr_repair).unwrap()["retired"].clone()
}

#[test]
fn closed_issue_retires_the_held_task_before_scheduling_even_while_search_lists_its_pr() {
    let f = Fixture::new();
    held_after_exit(&f, CLAIM_CHANGED);
    close_issue_12(&f);
    // The merge and close race: the search and the PR read both still say open.
    serve_pr_42_with(&f, true, true);
    let config = newer_config(&f);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&config, &mut launcher).unwrap().unwrap();
    assert_only_331_dispatched(&launcher, &report);
    let retired = retired_in(&report);
    assert_eq!(retired[0]["task_id"], "task-12");
    assert_eq!(retired[0]["phase"], "issue_closed");
    assert!(report.pr_repair.held.is_empty());
    assert_eq!(phase_of_12(&config), "issue_closed");
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}

#[test]
fn merged_pr_retires_the_task_while_the_search_still_lists_it_open() {
    for reason in [CLAIM_CHANGED, POST_REPAIR] {
        let f = Fixture::new();
        held_after_exit(&f, reason);
        serve_pr_42_with(&f, true, false);
        let config = newer_config(&f);
        let mut launcher = RecordingLauncher::default();
        let report = f.cycle(&config, &mut launcher).unwrap().unwrap();
        assert_only_331_dispatched(&launcher, &report);
        assert_eq!(retired_in(&report)[0]["phase"], "pr_merged", "{reason}");
        assert_eq!(phase_of_12(&config), "pr_merged");
        let store = StateStore::open(&f.config.state_root, 2).unwrap();
        let shown = crate::cli::execute(&f.config.state_root, &["status".into()]).unwrap();
        assert!(shown.contains("\"phase\":\"pr_merged\""), "{shown}");
        assert!(shown.contains("saved verified PR 42 merged"), "{shown}");
        assert!(scheduling::ensure_dispatch_capacity(&store).is_ok());
    }
}

#[test]
fn unfinished_work_stays_held_and_is_not_reported_retired() {
    let f = Fixture::new();
    held_after_exit(&f, CLAIM_CHANGED);
    serve_pr_42_with(&f, true, true);
    let config = newer_config(&f);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&config, &mut launcher).unwrap().unwrap();
    assert!(retired_in(&report).as_array().unwrap().is_empty());
    assert!(report.pr_repair.retire_retained.is_empty());
    assert_eq!(phase_of_12(&config), "held");
}
