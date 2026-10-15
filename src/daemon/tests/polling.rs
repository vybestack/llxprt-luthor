//! Behavioral coverage for historical settlement and quota-aware daemon polling.
use super::*;
use super::{
    merges::{merge_enabled, seed_aligned_issue12, serve_merge},
    repairs::{phase_of_12, pr_detail, serve_pr_42_detail},
};

fn saved_terminal(f: &Fixture, merged: bool) {
    let (saved, _) = seed_aligned_issue12(f);
    let mut detail = pr_detail(f, false);
    detail["head"]["sha"] = json!(saved.head_commit_sha);
    detail["head"]["ref"] = json!(saved.head_branch);
    if !merged {
        detail["merged"] = json!(false);
        detail["merged_at"] = json!(null);
        detail["merge_commit_sha"] = json!(null);
        detail["closed_at"] = json!("2026-10-15T12:00:00Z");
    }
    serve_pr_42_detail(f, false, &detail);
}

fn pr_reads(f: &Fixture) -> usize {
    fs::read_to_string(&f.calls)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("api repos/org/code/pulls/42"))
        .count()
}

fn run(f: &Fixture, config: &Config) -> ScheduleReport {
    f.cycle(config, &mut RecordingLauncher::default())
        .unwrap()
        .unwrap()
}

#[test]
fn upgrade_settles_historical_same_head_deliveries_even_with_disabled_merge_cleanup_and_mapping_drift()
 {
    let f = Fixture::new();
    saved_terminal(&f, true);
    let mut config = f.config.clone();
    config.mappings[0].required_checks =
        crate::config::RequiredChecks::Names(vec!["upgraded-ci".into()]);
    let store = StateStore::open(&config.state_root, 2).unwrap();
    let before = history(&store, Some("task-12"));
    // Model an on-disk database from before issue 88, not a new-task-only path.
    store
        .connection
        .execute_batch("DROP TABLE pr_polls; PRAGMA user_version=5;")
        .unwrap();
    drop(store);
    let report = run(&f, &config);
    assert_eq!(report.pr_merge.delivered.len(), 1);
    assert_eq!(phase_of_12(&config), "pr_merged");
    let store = StateStore::open(&config.state_root, 2).unwrap();
    let proof = crate::state::retirement::latest(&store.connection, "task-12")
        .unwrap()
        .unwrap();
    assert_eq!(proof["evidence"]["pr"]["state"], "merged");
    assert_eq!(
        proof["evidence"]["pr"]["head_commit_sha"],
        crate::state::pr_repair::saved_pr(&store.connection, "task-12")
            .unwrap()
            .head_commit_sha
    );
    assert_eq!(proof["evidence"]["luthor_merge_evidence"], "none");
    assert!(
        crate::state::pr_merge::authorization(&store, "task-12")
            .unwrap()
            .is_none()
    );
    let after = history(&store, Some("task-12"));
    assert_eq!(&after[1..4], &before[1..4]);
    assert!(after[4].starts_with(&before[4]));
    drop(store);
    let reads = pr_reads(&f);
    run(&f, &config);
    assert_eq!(pr_reads(&f), reads);
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(!calls.contains("/pulls/42/merge") && !calls.contains("--method PATCH"));
    assert!(!calls.contains("check-runs") && !calls.contains("headRefOid"));
}

#[test]
fn closed_unmerged_pr_settles_once_without_closing_the_tracker_or_fetching_checks() {
    let f = Fixture::new();
    saved_terminal(&f, false);
    let config = merge_enabled(&f);
    assert_eq!(run(&f, &config).pr_merge.delivered.len(), 1);
    assert_eq!(phase_of_12(&config), "pr_closed");
    let reads = pr_reads(&f);
    run(&f, &config);
    assert_eq!(pr_reads(&f), reads);
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(!calls.contains("--method PATCH") && !calls.contains("/pulls/42/merge"));
    assert!(!calls.contains("check-runs"));
}

#[test]
fn mapping_mismatch_records_local_facts_once_and_does_not_poll_merge_or_repair() {
    let f = Fixture::new();
    let (saved, _) = seed_aligned_issue12(&f);
    let mut detail = pr_detail(&f, true);
    detail["head"]["sha"] = json!(saved.head_commit_sha);
    detail["head"]["ref"] = json!(saved.head_branch);
    serve_pr_42_detail(&f, true, &detail);
    let mut config = merge_enabled(&f);
    config.mappings[0].required_checks =
        crate::config::RequiredChecks::Names(vec!["changed-ci".into()]);
    assert_eq!(run(&f, &config).pr_merge.waiting.len(), 1);
    let reads = pr_reads(&f);
    assert_eq!(reads, 1); // Lightweight upgrade/terminal probe only.
    assert!(run(&f, &config).pr_merge.waiting.is_empty());
    assert_eq!(pr_reads(&f), reads);
    let store = StateStore::open(&config.state_root, 2).unwrap();
    assert_eq!(store.connection.query_row("SELECT COUNT(*) FROM evidence WHERE task_id='task-12' AND kind='merge_mapping_mismatch'", [], |r| r.get::<_, u64>(0)).unwrap(), 1);
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(!calls.contains("check-runs") && !calls.contains("headRefOid"));
}

#[test]
fn pending_checks_are_not_refetched_each_cycle_and_resume_after_the_persisted_deadline() {
    let f = Fixture::new();
    serve_merge(&f);
    let source = fs::read_to_string(&f.gh)
        .unwrap()
        .replace("\"state\":\"SUCCESS\"", "\"state\":\"PENDING\"");
    fs::write(&f.gh, source).unwrap();
    let config = merge_enabled(&f);
    let first = run(&f, &config);
    assert!(!first.pr_merge.waiting.is_empty());
    let reads = pr_reads(&f);
    let health_reads = fs::read_to_string(&f.calls)
        .unwrap()
        .matches("headRefOid")
        .count();
    assert_eq!(health_reads, 1);
    run(&f, &config);
    assert_eq!(pr_reads(&f), reads);
    assert_eq!(
        fs::read_to_string(&f.calls)
            .unwrap()
            .matches("headRefOid")
            .count(),
        health_reads
    );
    // Advance the persisted observation clock, not a production timeout.
    let store = StateStore::open(&config.state_root, 2).unwrap();
    store
        .connection
        .execute(
            "UPDATE pr_polls SET observed_at=0 WHERE task_id='task-12'",
            [],
        )
        .unwrap();
    drop(store);
    run(&f, &config);
    assert!(pr_reads(&f) > reads);
    assert!(
        fs::read_to_string(&f.calls)
            .unwrap()
            .matches("headRefOid")
            .count()
            > health_reads
    );
    assert!(
        !fs::read_to_string(&f.calls)
            .unwrap()
            .contains("--method PUT")
    );
}

#[test]
fn quota_reserve_suppresses_optional_sweeps_but_not_dispatch_and_reports_actual_api_attempts() {
    let f = Fixture::new();
    serve_merge(&f);
    let source = fs::read_to_string(&f.gh)
        .unwrap()
        .replace("\"remaining\":5000", "\"remaining\":499");
    fs::write(&f.gh, source).unwrap();
    fs::write(&f.calls, "").unwrap();
    let config = merge_enabled(&f);
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&config, &mut launcher).unwrap().unwrap();
    assert_eq!(report.launched.len(), 1);
    assert_eq!(pr_reads(&f), 0);
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(!calls.contains("check-runs") && !calls.contains("headRefOid"));
    assert_eq!(report.github.core_remaining, Some(499));
    assert_eq!(report.github.core_reset, Some(9999999999));
    assert_eq!(
        report.github.rest,
        calls
            .lines()
            .filter(|c| c.starts_with("api ") && !c.starts_with("api graphql"))
            .count() as u64
    );
    assert_eq!(
        report.github.graphql,
        calls
            .lines()
            .filter(|c| c.starts_with("api graphql"))
            .count() as u64
    );
    assert!(report.github.rest > 0 && report.github.graphql > 0);
}

#[test]
fn terminal_tracker_followup_is_mandatory_below_reserve_and_never_repeats_an_uncertain_write() {
    let f = Fixture::new();
    saved_terminal(&f, true);
    super::external_merges::serve_tracker(&f);
    let source = fs::read_to_string(&f.gh).unwrap().replace(
        &format!(
            "touch '{}';",
            f.root
                .path()
                .join("externally-delivered-tracker-closed")
                .display()
        ),
        "exit 1;",
    );
    fs::write(&f.gh, source).unwrap();
    let config = merge_enabled(&f);
    assert_eq!(run(&f, &config).pr_merge.waiting.len(), 1);
    assert_eq!(phase_of_12(&config), "pr_complete");
    let patches = || {
        fs::read_to_string(&f.calls)
            .unwrap()
            .matches("--method PATCH")
            .count()
    };
    assert_eq!(patches(), 1);
    let source = fs::read_to_string(&f.gh)
        .unwrap()
        .replace("\"remaining\":5000", "\"remaining\":499");
    fs::write(&f.gh, source).unwrap();
    assert_eq!(run(&f, &config).pr_merge.waiting.len(), 1);
    assert_eq!(patches(), 1);
    assert_eq!(phase_of_12(&config), "pr_complete");
    // An operator/GitHub confirms closure; the interrupted write is never replayed.
    fs::write(
        f.root.path().join("externally-delivered-tracker-closed"),
        "closed",
    )
    .unwrap();
    assert_eq!(run(&f, &config).pr_merge.delivered.len(), 1);
    assert_eq!(patches(), 1);
    assert_eq!(phase_of_12(&config), "pr_merged");
}

#[test]
fn held_repair_delivery_closes_its_tracker_before_durable_retirement() {
    let f = Fixture::new();
    saved_terminal(&f, true);
    super::external_merges::serve_tracker(&f);
    let mut store = StateStore::open(&f.config.state_root, 2).unwrap();
    task_records::hold_task(&mut store, "task-12", "held repair awaiting delivery").unwrap();
    drop(store);
    let config = merge_enabled(&f);
    assert_eq!(run(&f, &config).pr_repair.retired.len(), 1);
    assert_eq!(phase_of_12(&config), "pr_merged");
    assert_eq!(
        fs::read_to_string(&f.calls)
            .unwrap()
            .matches("--method PATCH")
            .count(),
        1
    );
    let reads = pr_reads(&f);
    run(&f, &config);
    assert_eq!(pr_reads(&f), reads);
}

#[test]
fn luthor_merge_and_tracker_closure_become_terminal_and_do_not_poll_again() {
    let f = Fixture::new();
    serve_merge(&f);
    let config = merge_enabled(&f);
    assert_eq!(run(&f, &config).pr_merge.merged, ["task-12"]);
    assert_eq!(phase_of_12(&config), "pr_merged");
    let reads = pr_reads(&f);
    run(&f, &config);
    assert_eq!(pr_reads(&f), reads);
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert_eq!(calls.matches("--method PUT").count(), 1);
    assert_eq!(calls.matches("--method PATCH").count(), 1);
    assert_eq!(
        calls
            .matches("--method POST repos/org/tracker/issues/12/comments")
            .count(),
        1
    );
}

#[test]
fn quota_headers_stop_further_optional_reads_in_the_same_cycle() {
    let f = Fixture::new();
    serve_merge(&f);
    let source = fs::read_to_string(&f.gh).unwrap().replace(
        "  repos/org/code/pulls/42) if",
        "  repos/org/code/pulls/42) printf 'HTTP/2 200\\r\\nX-RateLimit-Remaining: 499\\r\\nX-RateLimit-Reset: 12345\\r\\n\\r\\n'; if",
    );
    fs::write(&f.gh, source).unwrap();
    let report = run(&f, &merge_enabled(&f));
    assert_eq!(pr_reads(&f), 1);
    assert_eq!(report.github.core_remaining, Some(499));
    assert_eq!(report.github.core_reset, Some(12345));
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(!calls.contains("headRefOid") && !calls.contains("check-runs"));
    assert!(!calls.contains("--method PUT"));
}

#[test]
fn read_only_reconciliation_defers_terminal_delivery_to_configured_tracker_followup() {
    let f = Fixture::new();
    saved_terminal(&f, true);
    super::external_merges::serve_tracker(&f);
    let config = merge_enabled(&f);
    let mut store = StateStore::open(&config.state_root, 2).unwrap();
    task_records::hold_task(&mut store, "task-12", "post-repair PR verification failed").unwrap();
    let mut projects = GhProjectReader::new(f.gh.clone());
    let mut prs = GhPullRequestReader::new(f.gh.clone());
    {
        let _cycle = crate::github::budget::CycleGuard::start(&config.github_polling);
        let result = crate::coordinator::retirement::retire_if_finished(
            &mut store,
            "task-12",
            &mut projects,
            &mut prs,
        )
        .unwrap();
        assert!(matches!(
            result,
            crate::coordinator::retirement::Settlement::Blocked(_)
        ));
        assert_eq!(
            task_records::task_phase(&store, "task-12")
                .unwrap()
                .as_deref(),
            Some("held")
        );
    }
    drop(store);
    assert_eq!(run(&f, &config).pr_repair.retired.len(), 1);
    assert_eq!(phase_of_12(&config), "pr_merged");
    assert_eq!(
        fs::read_to_string(&f.calls)
            .unwrap()
            .matches("--method PATCH")
            .count(),
        1
    );
}

#[test]
fn interrupted_own_merge_confirms_tracker_after_worker_mapping_drift_without_replaying_writes() {
    let f = Fixture::new();
    serve_merge(&f);
    let script = fs::read_to_string(&f.gh).unwrap().replace(
        &format!(
            "touch '{}';",
            f.root.path().join("tracker-closed").display()
        ),
        "exit 1;",
    );
    fs::write(&f.gh, script).unwrap();
    let mut config = merge_enabled(&f);
    assert_eq!(run(&f, &config).pr_merge.waiting.len(), 1);
    assert_eq!(phase_of_12(&config), "pr_complete");
    fs::write(f.root.path().join("tracker-closed"), "confirmed closed").unwrap();
    config.mappings[0].required_checks =
        crate::config::RequiredChecks::Names(vec!["changed-ci".into()]);
    let script = fs::read_to_string(&f.gh)
        .unwrap()
        .replace("\"remaining\":5000", "\"remaining\":499");
    fs::write(&f.gh, script).unwrap();
    assert_eq!(run(&f, &config).pr_merge.merged, ["task-12"]);
    assert_eq!(phase_of_12(&config), "pr_merged");
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert_eq!(calls.matches("--method PUT").count(), 1);
    assert_eq!(calls.matches("--method PATCH").count(), 1);
}
