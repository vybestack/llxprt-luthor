use super::harness::{Harness, stderr};
use luthor::state::StateStore;
use luthor::state::task_records;

pub(crate) fn resume_without_execute_is_held_without_github_or_worker_invocations() {
    let h = Harness::new();
    let output = h.run(&["resume", "task", "--config", h.config.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("pass --execute"));
    assert!(!h.log.exists());
}

pub(crate) fn resume_rejects_malformed_and_duplicate_execute_arguments_without_invocation() {
    let h = Harness::new();
    let config = h.config.to_str().unwrap();
    for args in [
        vec![
            "resume",
            "task",
            "--config",
            config,
            "--execute",
            "--execute",
        ],
        vec!["resume", "task", "--execute", "--config", config],
        vec!["resume", "task", "--config", config, "--unknown"],
    ] {
        let output = h.run(&args);
        assert!(!output.status.success(), "{args:?}");
    }
    assert!(!h.log.exists());
}

pub(crate) fn resume_unknown_task_fails_before_github_invocation() {
    let h = Harness::new();
    let output = h.run(&[
        "resume",
        "unknown",
        "--config",
        h.config.to_str().unwrap(),
        "--execute",
    ]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("task not found"));
    assert!(!h.log.exists());
}

pub(crate) fn resume_rejects_non_resumable_held_task_without_github_or_worker_invocation() {
    let h = Harness::new();
    h.seed_held_task();
    let output = h.run(&[
        "resume",
        "task",
        "--config",
        h.config.to_str().unwrap(),
        "--execute",
    ]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("resume held"));
    let store = StateStore::open(&h.state, 2).unwrap();
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(
        task_records::held_reason(&store, "task")
            .unwrap()
            .as_deref(),
        Some("fixture paused")
    );
    assert!(!h.log.exists());
}
