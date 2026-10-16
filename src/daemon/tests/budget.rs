//! Quota exhaustion is visible without turning it into an ordinary idle queue.
use super::*;
use super::{
    merges::{merge_enabled, serve_merge},
    repairs::phase_of_12,
};

#[test]
fn exhausted_graphql_quota_is_reported_in_cycle_and_read_only_status_without_discovery() {
    let f = Fixture::new();
    serve_merge(&f);
    let config = merge_enabled(&f);
    let script = fs::read_to_string(&f.gh).unwrap().replace(
        "\"graphql\":{\"remaining\":5000",
        "\"graphql\":{\"remaining\":0",
    );
    assert_ne!(script, fs::read_to_string(&f.gh).unwrap());
    fs::write(&f.gh, script).unwrap();
    fs::write(&f.calls, "").unwrap();
    let report = f
        .cycle(&config, &mut RecordingLauncher::default())
        .unwrap()
        .unwrap();
    assert_eq!(report.github.graphql_remaining, Some(0));
    assert_eq!(report.github.graphql_reset, Some(9999999999));
    assert_eq!(report.github.exhausted(1)[0].resource, "graphql");
    assert!(report.launched.is_empty());
    assert!(!f.assigned.exists());
    assert_eq!(phase_of_12(&config), "pr_complete");
    assert!(report.pr_merge.waiting.iter().any(|note| {
        note.task_id == "task-12" && note.reason.contains("github_budget_exhausted")
    }));
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(!calls.contains("api graphql"));
    let status: serde_json::Value =
        serde_json::from_str(&crate::cli::execute(&config.state_root, &["status".into()]).unwrap())
            .unwrap();
    assert_eq!(status["github"]["graphql_remaining"], 0);
    assert_eq!(status["github_budget_exhausted"][0]["resource"], "graphql");
    assert_eq!(
        status["github_budget_exhausted"][0]["reset_at_unix_secs"],
        9999999999_u64
    );
    let task = status["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["task_id"] == "task-12")
        .unwrap();
    assert_eq!(
        task["github_maintenance_wait"]["reason"],
        "github_budget_exhausted"
    );
    assert_eq!(
        calls,
        fs::read_to_string(&f.calls).unwrap(),
        "status must not read GitHub"
    );
}

#[test]
fn point_charges_in_project_response_reach_cycle_and_persisted_status() {
    let f = Fixture::new();
    let script = fs::read_to_string(&f.gh).unwrap().replace(
        "{\"data\":{\"node\":",
        "{\"data\":{\"rateLimit\":{\"cost\":7,\"remaining\":4900,\"resetAt\":\"2026-10-17T04:35:00Z\"},\"node\":",
    );
    assert_ne!(script, fs::read_to_string(&f.gh).unwrap());
    fs::write(&f.gh, script).unwrap();
    let report = f
        .cycle(&f.config, &mut RecordingLauncher::default())
        .unwrap()
        .unwrap();
    assert!(report.github.graphql > 0);
    assert_eq!(report.github.graphql_points, report.github.graphql * 7);
    let status: serde_json::Value = serde_json::from_str(
        &crate::cli::execute(&f.config.state_root, &["status".into()]).unwrap(),
    )
    .unwrap();
    assert_eq!(
        status["github"]["graphql_points"],
        report.github.graphql_points
    );
    assert_eq!(status["github"]["graphql_remaining"], 4900);
    assert!(
        status["github_budget_exhausted"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn failed_cycle_preserves_budget_snapshot_without_swallowing_the_scheduling_error() {
    let f = Fixture::new();
    let mut config = f.config.clone();
    config.mappings[0].checkout = f.root.path().join("missing-checkout");
    let error = f
        .cycle(&config, &mut RecordingLauncher::default())
        .unwrap_err();
    assert!(error.to_string().contains("worktree I/O failed: NotFound"));
    let status: serde_json::Value =
        serde_json::from_str(&crate::cli::execute(&config.state_root, &["status".into()]).unwrap())
            .unwrap();
    assert!(status["github"]["graphql"].as_u64().unwrap() > 0);
    assert_eq!(status["github"]["graphql_remaining"], 5000);
    assert!(status["github"]["observed_at"].as_u64().unwrap() > 0);
    assert!(!f.assigned.exists());
}
