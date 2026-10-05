use super::support::Fixture;
use luthor::{cli::CliError, state::StateStore};
use serde_json::{Value, json};
use std::fs;

pub(crate) fn show_reports_events_issue_mapping_session_and_receipt_without_secrets() {
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "attempt", "persisted-session");
    let candidate = json!({"issue_url":"https://github.com/org/tracker/issues/7",
        "source":{"project_id":"project"},"mapping":{"code_repository":"org/code"}});
    f.evidence(
        "task",
        None,
        "selection",
        &json!({"candidate":candidate,"effective_config":{"secret":"do-not-show"}}).to_string(),
    );
    f.evidence("task", None, "held_reason", "needs inspection");
    let (stdout, stderr) = f.logs("attempt");
    f.receipt("task", "attempt", stdout, stderr);
    let shown = f.run(&["show", "task"]).unwrap();
    assert_eq!(shown["session"], "persisted-session");
    assert_eq!(shown["task"]["source"]["project_id"], "project");
    assert_eq!(shown["task"]["mapping"]["code_repository"], "org/code");
    assert_eq!(
        shown["task"]["issue_url"],
        "https://github.com/org/tracker/issues/7"
    );
    assert_eq!(shown["reason"], "needs inspection");
    assert_eq!(shown["reserved_slot"], true);
    assert_eq!(shown["attempts"][0]["id"], "attempt");
    assert_eq!(shown["evidence"][2]["detail"]["stdout_bytes"], 5);
    assert!(
        shown["evidence"][0]["created_at"]
            .as_str()
            .unwrap()
            .contains(' ')
    );
    assert!(
        shown["evidence"][0]["created_at_unix_secs"]
            .as_i64()
            .unwrap()
            > 0
    );
    assert!(shown["last_output_age_seconds"].is_number());
    let rendered = shown.to_string();
    assert!(!rendered.contains("do-not-show"));
    assert!(!rendered.contains("secret-prompt"));
    assert_eq!(
        f.run(&["logs", "task", "--attempt", "attempt"]).unwrap()["receipt"]["exit_code"],
        0
    );
    assert_eq!(f.run(&["logs", "task"]).unwrap()["logs"]["stdout"], "hello");
}

pub(crate) fn show_reports_verified_pr_as_cached_and_rejects_malformed_or_duplicate_proof() {
    let f = Fixture::new();
    f.task("task");
    f.db()
        .execute("UPDATE tasks SET state='pr_complete' WHERE id='task'", [])
        .unwrap();
    let proof = json!({"id":123,"url":"https://github.com/org/code/pull/9",
        "repository":"org/code","head_repository":"org/fork","draft":true,
        "checks":["failure","pending"],"observed_at":1700000000,"attempt_id":"attempt"});
    f.evidence(
        "task",
        Some("attempt"),
        "verified_open_pr",
        &proof.to_string(),
    );
    let shown = f.run(&["show", "task"]).unwrap();
    assert_eq!(shown["verified_pr"]["id"], 123);
    assert_eq!(shown["verified_pr"]["url"], proof["url"]);
    assert_eq!(shown["verified_pr"]["checks"], proof["checks"]);
    assert_eq!(shown["verified_pr"]["repository"], "org/code");
    assert_eq!(shown["verified_pr"]["head_repository"], "org/fork");
    assert_eq!(shown["verified_pr"]["observed_at"], 1700000000);
    assert_eq!(shown["last_pr_verification_at_unix_secs"], 1700000000);
    assert_eq!(shown["pr_state"], "open_at_last_verification");
    assert!(shown["pr_unavailable_reason"].is_null());
    assert_eq!(
        f.db()
            .query_row(
                "SELECT COUNT(*) FROM evidence WHERE kind='verified_open_pr'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );

    f.evidence(
        "task",
        Some("attempt"),
        "verified_open_pr",
        &proof.to_string(),
    );
    assert_eq!(f.run(&["show", "task"]), Err(CliError::Database));
    f.db()
        .execute("DELETE FROM evidence WHERE sequence=(SELECT MAX(sequence) FROM evidence WHERE kind='verified_open_pr')", [])
        .unwrap();
    f.db()
        .execute(
            "UPDATE evidence SET payload='not-json' WHERE kind='verified_open_pr'",
            [],
        )
        .unwrap();
    assert_eq!(f.run(&["show", "task"]), Err(CliError::Database));
}

pub(crate) fn status_reports_cached_pr_only_for_completed_task_without_gating_on_checks() {
    let f = Fixture::new();
    f.task("completed");
    f.task("held");
    f.db()
        .execute(
            "UPDATE tasks SET state='pr_complete' WHERE id='completed'",
            [],
        )
        .unwrap();
    let proof = json!({"id":123,"url":"https://github.com/org/code/pull/9",
        "repository":"org/code","head_repository":"org/fork","draft":false,
        "checks":["failure","pending"],"observed_at":1700000000,"attempt_id":"attempt"});
    f.evidence(
        "completed",
        Some("attempt"),
        "verified_open_pr",
        &proof.to_string(),
    );

    let status = f.run(&["status"]).unwrap();
    let completed = status["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["task_id"] == "completed")
        .unwrap();
    let held = status["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["task_id"] == "held")
        .unwrap();
    assert_eq!(completed["phase"], "pr_complete");
    assert_eq!(completed["pr_state"], "open_at_last_verification");
    assert_eq!(completed["verified_pr"]["id"], 123);
    assert_eq!(completed["verified_pr"]["url"], proof["url"]);
    assert_eq!(completed["verified_pr"]["draft"], false);
    assert_eq!(completed["verified_pr"]["checks"], proof["checks"]);
    assert_eq!(completed["verified_pr"]["observed_at"], 1700000000);
    assert_eq!(completed["verified_pr"]["attempt_id"], "attempt");
    assert_eq!(held["pr_state"], "unavailable");
    assert!(held["verified_pr"].is_null());
    assert!(held["pr_unavailable_reason"].is_string());
}

pub(crate) fn status_and_show_work_while_coordinator_owns_lock() {
    let f = Fixture::new();
    f.task("task");
    let status = f.run(&["status"]).unwrap();
    assert_eq!(status["capacity"]["limit"], 2);
    assert_eq!(status["tasks"][0]["phase"], "held");
    assert_eq!(status["tasks"][0]["reserved_slot"], false);
    assert_eq!(status["tasks"][0]["latest_attempt_id"], Value::Null);
    assert_eq!(status["tasks"][0]["latest_attempt_outcome"], Value::Null);
    assert_eq!(status["tasks"][0]["pr_state"], "unavailable");
    assert!(
        status["tasks"][0]["pr_unavailable_reason"]
            .as_str()
            .is_some()
    );
    assert_eq!(status["reserved_slot_count"], 0);
    assert_eq!(status["latest_telemetry"], Value::Null);
    assert_eq!(status["tasks"][0]["last_output_age_seconds"], Value::Null);
    assert_eq!(status["tasks"][0]["output_silence_warning"], false);
    assert_eq!(
        status["tasks"][0]["output_age_unavailable_reason"],
        "no reserved attempt"
    );
    let shown = f.run(&["show", "task"]).unwrap();
    assert_eq!(shown["pr_state"], "unavailable");
    assert_eq!(shown["last_observed_pr"], Value::Null);
    assert_eq!(
        shown["last_observed_pr_unavailable_reason"],
        "no stored PR observation"
    );
    assert_eq!(shown["output_log_status"], "unavailable");
    assert_eq!(shown["last_output_age_seconds"], Value::Null);
    assert_eq!(
        shown["output_age_unavailable_reason"],
        "no reserved attempt"
    );
    assert_eq!(f.run(&["show", "missing"]), Err(CliError::TaskNotFound));
    assert_eq!(f.run(&["logs", "missing"]), Err(CliError::TaskNotFound));
    assert_eq!(f.run(&["logs", "task"]), Err(CliError::AttemptNotFound));
}

#[test]
fn read_only_views_preserve_state_and_launch_artifacts_under_active_lock() {
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "attempt", "session");
    f.logs("attempt");
    let snapshot = || {
        let mut files: Vec<_> = fs::read_dir(f.root())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_file() && path.file_name().unwrap() != "state.sqlite3-shm")
            .collect();
        files.extend(
            fs::read_dir(f.root().join("attempts"))
                .unwrap()
                .map(|entry| entry.unwrap().path()),
        );
        files.sort();
        files
            .into_iter()
            .map(|path| {
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect::<Vec<_>>()
    };
    let before = snapshot();
    for args in [vec!["status"], vec!["show", "task"], vec!["logs", "task"]] {
        f.run(&args).unwrap();
    }
    assert_eq!(snapshot(), before);
    assert!(StateStore::open(f.root(), 2).is_err());
}
