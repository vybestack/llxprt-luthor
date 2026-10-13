//! Cached check-policy evidence must distinguish unknown, absent and no gates.
use super::support::Fixture;
use luthor::cli::CliError;
use serde_json::json;

#[test]
fn status_and_show_check_source_is_null_before_observation_unknown_for_legacy() {
    let f = Fixture::new();
    f.task("task");
    assert!(f.run(&["show", "task"]).unwrap()["required_checks_source"].is_null());
    assert!(f.run(&["status"]).unwrap()["tasks"][0]["required_checks_source"].is_null());
    let legacy = json!({"id":42,"head":"a".repeat(40),"conflicting":false,
        "failed_required_checks":[],"required_checks_pending":false,
        "failed_jobs":[],"required_jobs":[]});
    f.evidence("task", None, "pr_health", &legacy.to_string());
    assert_eq!(
        f.run(&["show", "task"]).unwrap()["required_checks_source"],
        "unknown"
    );
    assert_eq!(
        f.run(&["status"]).unwrap()["tasks"][0]["required_checks_source"],
        "unknown"
    );
    let mut current = legacy;
    current["required_checks_source"] = json!("none");
    f.evidence("task", None, "pr_health", &current.to_string());
    assert_eq!(
        f.run(&["show", "task"]).unwrap()["required_checks_source"],
        "none"
    );
    assert_eq!(
        f.run(&["status"]).unwrap()["tasks"][0]["required_checks_source"],
        "none"
    );
}

#[test]
fn malformed_latest_check_health_fails_closed_instead_of_hiding_observation() {
    let f = Fixture::new();
    f.task("task");
    f.evidence(
        "task",
        None,
        "pr_health",
        r#"{"required_checks_source":"none"}"#,
    );
    assert_eq!(f.run(&["show", "task"]), Err(CliError::Database));
    assert_eq!(f.run(&["status"]), Err(CliError::Database));
}
