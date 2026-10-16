//! Execute cycles must survive a pending retry's configuration revision.
use super::{Fixture, await_exit, latest};
use luthor::{
    config::{CleanupPolicy, DependencyRule, IssueReference, RetryPolicy, TransientFailure},
    state::{StateStore, journal, scheduling, task_records},
};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn pending_turn_budget() -> (Fixture, String, String) {
    let f = Fixture::new();
    let worker = &f.config.initial.executable;
    fs::write(worker, r#"#!/bin/sh
set -eu
session=''
while test "$#" -gt 0; do
  if test "$1" = --session; then session="$2"; shift; fi
  shift
done
printf '{"status":"error","session_id":"%s","error":{"code":"turn-budget","message":"round budget exhausted"}}\n' "$session"
exit 6
"#).unwrap();
    let task = f.dispatch();
    let previous = latest(&f, &task);
    await_exit(&f, &task, &previous);
    f.cycle();
    // Model the first observation as an earlier cycle, never shorten backoff.
    let mut store = StateStore::open(&f.config.state_root, 1).unwrap();
    journal::record_evidence(
        &mut store,
        &task,
        Some(&previous),
        "worker_outcome",
        &json!({"outcome":{"kind":"transient","class":"turn_budget"},"observed_at_unix_secs":1})
            .to_string(),
    )
    .unwrap();
    drop(store);
    (f, task, previous)
}

fn revised_cycle(f: &Fixture, mismatch: bool) -> std::process::Output {
    let mut config = f.config.clone();
    config.retry_policy = RetryPolicy::Bounded {
        max_attempts: 2,
        backoff_secs: 60,
        classes: vec![TransientFailure::TurnBudget],
    };
    if mismatch {
        config.cleanup = CleanupPolicy::Worktree;
    } else {
        config.sources[0].dependencies.push(DependencyRule {
            issue: IssueReference {
                repository: "org/tracker".into(),
                number: 8,
            },
            blocked_by: vec![IssueReference {
                repository: "org/tracker".into(),
                number: 9,
            }],
            allow_closed_unmerged_prs: false,
        });
    }
    config.validate().unwrap();
    let path = f.root.path().join("config.json");
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    crate::run(
        path.to_str().unwrap(),
        &format!("{}:/usr/bin:/bin", f.root.path().display()),
        &["--issues", "7", "--execute"],
    )
}

#[test]
fn daemon_retries_pending_turn_budget_after_dependencies_only_revision_and_exits_zero() {
    let (f, task, previous) = pending_turn_budget();
    let output = revised_cycle(&f, false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["automatic_retries"][0]["task_id"], task);
    assert_eq!(report["automatic_retries"][0]["attempt_id"], previous);
    assert_eq!(report["automatic_retries"][0]["decision"], "launched");
    assert_eq!(report["launched"].as_array().unwrap().len(), 1);
    let attempt = report["launched"][0]["attempt_id"].as_str().unwrap();
    await_exit(&f, &task, attempt);
    let settled = f.cycle();
    assert_eq!(settled["automatic_retries"][0]["decision"], "exhausted");
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    let audit = journal::evidence_payloads(&store, &task, attempt, "retry_authorized").unwrap();
    let audit: Value = serde_json::from_str(&audit[0]).unwrap();
    assert_eq!(audit["previous_plan"]["attempt_id"], previous);
    assert_eq!(audit["actor"], "acoliver");
    assert_eq!(audit["plan"]["session_id"], task);
    assert!(Path::new(audit["plan"]["worktree"].as_str().unwrap()).is_dir());
}

#[test]
fn daemon_reports_task_config_mismatch_on_stderr_and_cycle_json_but_exits_zero() {
    let (f, task, previous) = pending_turn_budget();
    let output = revised_cycle(&f, true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let retry = &report["automatic_retries"][0];
    assert_eq!(retry["decision"], "config_mismatch");
    assert_eq!(retry["failure"]["differing_fields"], json!(["cleanup"]));
    assert_eq!(retry["failure"]["task_id"], task);
    assert_eq!(retry["failure"]["attempt_id"], previous);
    assert!(report["launched"].as_array().unwrap().is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(&task) && stderr.contains(&previous));
    assert!(stderr.contains("retry_admission") && stderr.contains("cleanup"));
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert_eq!(
        task_records::task_phase(&store, &task).unwrap().as_deref(),
        Some("attention")
    );
    assert_eq!(
        task_records::latest_attempt(&store, &task)
            .unwrap()
            .as_deref(),
        Some(previous.as_str())
    );
}
