//! Issue 93 regression: real CLI exit codes and stdout/stderr diagnostics.
use super::{fixture, run, tempdir};
use luthor::{config::Config, state::StateStore};
use serde_json::Value;
use std::fs;

#[test]
fn fatal_scheduling_error_reports_underlying_cause_and_context_on_both_streams() {
    let root = tempdir();
    let (config, path) = fixture(root.path(), "acoliver");
    // The configured checkout does not exist: fail closed on the actual I/O
    // error, without an assignment or a worker launch.
    let output = run(&config, &path, &["--issues", "7", "--execute"]);
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "cycle_error");
    let failure = &report["scheduling_error"];
    assert_eq!(failure["check"], "candidate_dispatch");
    assert!(
        failure["reason"]
            .as_str()
            .unwrap()
            .contains("worktree I/O failed: NotFound")
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    for field in ["reason", "task_id", "attempt_id", "check"] {
        assert!(
            stderr.contains(failure[field].as_str().unwrap()),
            "{stderr}"
        );
    }
    assert!(!stderr.contains("inspect local task state"));
    assert!(!root.path().join("assigned").exists());
    let config = Config::from_json(&fs::read_to_string(config).unwrap()).unwrap();
    let store = StateStore::open(&config.state_root, 1).unwrap();
    assert_eq!(
        luthor::state::scheduling::reservation_count(&store).unwrap(),
        0
    );
    assert_eq!(
        luthor::state::task_records::task_phase(&store, failure["task_id"].as_str().unwrap())
            .unwrap()
            .as_deref(),
        Some("held")
    );
}
