use super::harness::{Harness, stderr};
use luthor::state::StateStore;
use luthor::state::{scheduling, task_records};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    thread,
    time::{Duration, Instant},
};

pub(crate) fn local_controls_verify_live_child_and_expose_stop_intent_without_secrets() {
    use luthor::supervisor::{Reconciliation, reconcile_attempt};
    let h = Harness::new();
    h.seed_running_task();
    let mut store = StateStore::open(&h.state, 2).unwrap();
    let status = h.run(&["status", "--config", h.config.to_str().unwrap()]);
    assert!(status.status.success(), "{}", stderr(&status));
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let task = &status["tasks"][0];
    assert_eq!(task["phase"], "running");
    assert_eq!(task["latest_attempt_lifecycle"], "running");
    assert_eq!(task["reserved_slot"], true);
    assert_eq!(status["capacity"]["reserved"], 1);
    for who in ["child", "supervisor"] {
        assert!(task["process"][who]["pid"].as_u64().unwrap() > 0);
        assert!(
            task["process"][who]["boot_identity"]
                .as_str()
                .unwrap()
                .len()
                <= 256
        );
        assert!(
            !task["process"][who]["start_identity"]
                .as_str()
                .unwrap()
                .is_empty()
        );
        assert!(task["process"][who]["group_id"].as_u64().unwrap() > 0);
    }
    let shown = h.run(&["show", "task", "--config", h.config.to_str().unwrap()]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["phase"], "running");
    assert_eq!(shown["process"], task["process"]);
    assert_eq!(shown["attempts"][0]["lifecycle"], "running");
    assert!(!status.to_string().contains("Work on "));
    assert!(!shown.to_string().contains("Work on "));
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "running-attempt").unwrap(),
        Reconciliation::Running
    ));
    drop(store);
    let after = h.run(&["status", "--config", h.config.to_str().unwrap()]);
    let after: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(after["tasks"][0]["phase"], "running");
    h.stop_running_task();
    for args in [vec!["status"], vec!["show", "task"]] {
        let mut command = args;
        command.extend(["--config", h.config.to_str().unwrap()]);
        let output = h.run(&command);
        assert!(output.status.success(), "{}", stderr(&output));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let task = if command[0] == "status" {
            &result["tasks"][0]
        } else {
            &result
        };
        assert_eq!(task["phase"], "stop_requested");
        assert_eq!(task["process"], serde_json::Value::Null);
        assert_eq!(task["reserved_slot"], true);
    }
}

pub(crate) fn live_worker_reports_stream_bytes_before_exit() {
    let h = Harness::new();
    h.seed_running_task();
    let attempts = h.state.join("attempts");
    let stdout_log = attempts.join("running-attempt.stdout.log");
    let stderr_log = attempts.join("running-attempt.stderr.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let stdout_bytes = fs::metadata(&stdout_log)
            .map(|meta| meta.len())
            .unwrap_or(0);
        let stderr_bytes = fs::metadata(&stderr_log)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if stdout_bytes > 0 && stderr_bytes > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        fs::metadata(&stdout_log).unwrap().len() > 0,
        "worker stdout was not captured"
    );
    assert!(
        fs::metadata(&stderr_log).unwrap().len() > 0,
        "worker stderr was not captured"
    );

    let status = h.run(&["status", "--config", h.config.to_str().unwrap()]);
    assert!(status.status.success(), "{}", stderr(&status));
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let status_task = &status["tasks"][0];
    assert_eq!(status_task["phase"], "running");
    assert!(status_task["observed_stdout_bytes"].as_u64().unwrap() > 0);
    assert!(status_task["observed_stderr_bytes"].as_u64().unwrap() > 0);
    assert_eq!(status_task["byte_counts_are_observational"], true);
    assert!(status_task["last_output_age_seconds"].as_u64().unwrap() < 5);

    let shown = h.run(&["show", "task", "--config", h.config.to_str().unwrap()]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    let shown_task = &shown;
    assert_eq!(shown_task["phase"], "running");
    assert!(shown_task["observed_stdout_bytes"].as_u64().unwrap() > 0);
    assert!(shown_task["observed_stderr_bytes"].as_u64().unwrap() > 0);
    assert_eq!(shown_task["byte_counts_are_observational"], true);
    assert!(shown_task["last_output_age_seconds"].as_u64().unwrap() < 5);
    for output in [&status.to_string(), &shown.to_string()] {
        assert!(!output.contains("stdout-check"));
        assert!(!output.contains("stderr-check"));
    }

    h.stop_running_task();
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(attempts.join("running-attempt.receipt.json")).unwrap())
            .unwrap();
    assert_eq!(
        receipt["stdout_bytes"],
        fs::metadata(stdout_log).unwrap().len()
    );
    assert_eq!(
        receipt["stderr_bytes"],
        fs::metadata(stderr_log).unwrap().len()
    );
}

pub(crate) fn local_controls_hold_live_child_when_gate_proof_is_missing() {
    let h = Harness::new();
    h.seed_running_task();
    let db = rusqlite::Connection::open(h.state.join("state.sqlite3")).unwrap();
    db.execute("DELETE FROM evidence WHERE task_id='task' AND attempt_id='running-attempt' AND kind='gate_sent'", []).unwrap();
    drop(db);
    for args in [vec!["status"], vec!["show", "task"]] {
        let mut command = args;
        command.extend(["--config", h.config.to_str().unwrap()]);
        let output = h.run(&command);
        assert!(output.status.success(), "{}", stderr(&output));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let task = if command[0] == "status" {
            &result["tasks"][0]
        } else {
            &result
        };
        assert_eq!(task["phase"], "held");
        assert_eq!(task["process"], serde_json::Value::Null);
        assert_eq!(task["reserved_slot"], true);
    }
    h.stop_running_task();
}

pub(crate) fn local_controls_reject_forged_child_identity_with_gate_evidence() {
    let h = Harness::new();
    h.seed_running_task();
    let path = h.state.join("attempts/running-attempt.child.json");
    let mut child: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    child["pid"] = serde_json::json!(1);
    child["group_id"] = serde_json::json!(1);
    fs::write(&path, serde_json::to_vec(&child).unwrap()).unwrap();
    let db = rusqlite::Connection::open(h.state.join("state.sqlite3")).unwrap();
    db.execute("UPDATE evidence SET payload=?1 WHERE task_id='task' AND attempt_id='running-attempt' AND kind='child_registered'", [child.to_string()]).unwrap();
    drop(db);
    for args in [vec!["status"], vec!["show", "task"]] {
        let mut command = args;
        command.extend(["--config", h.config.to_str().unwrap()]);
        let output = h.run(&command);
        assert!(output.status.success(), "{}", stderr(&output));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let task = if command[0] == "status" {
            &result["tasks"][0]
        } else {
            &result
        };
        assert_eq!(task["phase"], "held");
        assert_eq!(task["process"], serde_json::Value::Null);
        assert_eq!(task["reserved_slot"], true);
    }
    h.stop_running_task();
}

pub(crate) fn reconcile_prelaunch_claim_uses_only_read_only_gh_and_pause_still_needs_attempt() {
    let h = Harness::new();
    h.seed_held_task();
    let mut store = StateStore::open(&h.state, 2).unwrap();
    task_records::record_claim_intent(&mut store, "task", "agent", "org/tracker", 7).unwrap();
    drop(store);
    let gh = h._dir.path().join("gh");
    fs::write(&gh, format!(r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  *graphql*) printf '%s\n' '{{"data":{{"node":{{"items":{{"nodes":[{{"id":"ITEM","content":{{"__typename":"Issue","id":"ISSUE","number":7,"repository":{{"id":"REPO","nameWithOwner":"org/tracker"}}}},"fieldValues":{{"nodes":[],"pageInfo":{{"hasNextPage":false}}}}}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}' ;;
  *repos/org/tracker/issues/7*) printf '%s\n' '{{"node_id":"ISSUE","number":7,"repository_url":"https://api.github.com/repos/org/tracker","html_url":"https://github.com/org/tracker/issues/7","state":"open","assignees":[{{"login":"agent"}}],"labels":[{{"name":"ready"}}],"milestone":null}}' ;;
  *repos/org/tracker*) printf '%s\n' '{{"node_id":"REPO"}}' ;;
  *) exit 91 ;;
esac
"#, h.log.display())).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let config = h.config.to_str().unwrap();
    let output = h.run(&["reconcile", "task", "--config", config]);
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "held");
    assert_eq!(report["project_membership"], true);
    assert_eq!(report["marker_present"], true);
    assert_eq!(report["assignees"], serde_json::json!(["agent"]));
    assert!(
        report["reasons"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("claim_intent_unverified"))
    );
    let calls = fs::read_to_string(&h.log).unwrap();
    assert_eq!(calls.lines().count(), 3);
    assert!(
        calls
            .lines()
            .all(|line| line.starts_with("api ") && !line.contains("-X") && !line.contains("POST"))
    );
    let pause = h.run(&["pause", "task", "--config", config]);
    assert!(!pause.status.success());
    assert!(stderr(&pause).contains("attempt not found"));
    let store = StateStore::open(&h.state, 2).unwrap();
    assert_eq!(task_records::latest_attempt(&store, "task").unwrap(), None);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(
        scheduling::unresolved_sources(&store).unwrap()[0].1,
        "claim_assignment"
    );
}
