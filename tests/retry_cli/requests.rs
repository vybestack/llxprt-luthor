use crate::{
    CliFixture, ExistingAttempt, cli_fixture, correct_config, dispatch_and_capture, invoke_retry,
    run, state_rows, worker_exit,
};
use luthor::{
    config::Config,
    state::{StateStore, launches::launch_intent, task_records::task_phase},
};
use luthor::{
    coordinator::{SupervisorLauncher, requests::drain},
    github::{project::GhProjectReader, pull_request::GhPullRequestReader},
    supervisor::{LaunchPlan, SupervisorError},
};
use serde_json::{Value, json};
use std::{
    fs,
    process::Command,
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct RecordingLauncher(Vec<LaunchPlan>);
impl SupervisorLauncher for RecordingLauncher {
    fn launch(
        &mut self,
        _: &mut StateStore,
        plan: &LaunchPlan,
        _: &luthor::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        self.0.push(plan.clone());
        Ok(())
    }
}

fn retry_fixture() -> (CliFixture, ExistingAttempt) {
    let f = cli_fixture(false);
    let previous = dispatch_and_capture(&f, false);
    correct_config(&f);
    (f, previous)
}

fn summary(f: &CliFixture) -> Value {
    let out = run(&f.config_path, &f.path, "status", &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn queued(out: std::process::Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["status"], "queued");
    value
}

fn drain_fixture(f: &CliFixture, launcher: &mut RecordingLauncher) {
    let config = Config::from_json(&fs::read_to_string(&f.config_path).unwrap()).unwrap();
    let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
    let gh = f._dir.path().join("gh");
    drain(
        &mut store,
        &config,
        &gh,
        &mut GhProjectReader::new(gh.clone()),
        &mut GhPullRequestReader::new(gh.clone()),
        launcher,
    )
    .unwrap();
}

fn blocker(f: &CliFixture, reserved: bool) {
    let db = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    if reserved {
        db.execute("INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision) VALUES('blocker','blocker-repo','blocker-issue','org/tracker',8,'held','old')", []).unwrap();
        db.execute("INSERT INTO attempts(id,task_id,lifecycle) VALUES('blocker-attempt','blocker','launch_intended')", []).unwrap();
        db.execute("INSERT INTO reservations(attempt_id,task_id,status) VALUES('blocker-attempt','blocker','reserved')", []).unwrap();
    } else {
        db.execute(
            "UPDATE reservations SET status='released' WHERE task_id='blocker'",
            [],
        )
        .unwrap();
        db.execute("UPDATE tasks SET state='completed' WHERE id='blocker'", [])
            .unwrap();
        db.execute("UPDATE attempts SET lifecycle='completed',outcome='fixture done' WHERE task_id='blocker'", []).unwrap();
    }
}

#[test]
fn retry_is_durable_during_coordinator_lock_and_daemon_applies_it_once() {
    let (f, previous) = retry_fixture();
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    let before = state_rows(&f.config.state_root);
    let first = queued(invoke_retry(&f, &previous, &["--execute"]));
    assert_eq!(first["reason"], "coordinator busy; request queued");
    let duplicate = queued(invoke_retry(&f, &previous, &["--execute"]));
    assert_eq!(first["request_id"], duplicate["request_id"]);
    assert_eq!(state_rows(&f.config.state_root), before);
    assert_eq!(summary(&f)["operator_requests"][0]["status"], "pending");
    drop(store);
    let out = run(
        &f.config_path,
        &f.path,
        "daemon",
        &[
            "--config-revision",
            "daemon-revision",
            "--once",
            "--execute",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cycle: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        cycle["launched"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["attempt_id"] == first["attempt_id"])
    );
    let status = summary(&f);
    assert_eq!(status["operator_requests"][0]["status"], "applied");
    let id = first["attempt_id"].as_str().unwrap();
    worker_exit::await_dispatched_worker_exit(&f.config, &previous.task, id);
    let out = run(
        &f.config_path,
        &f.path,
        "daemon",
        &[
            "--config-revision",
            "daemon-revision",
            "--once",
            "--execute",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        luthor::state::task_records::latest_attempt(&store, &previous.task)
            .unwrap()
            .as_deref(),
        Some(id)
    );
    let plan: LaunchPlan =
        serde_json::from_str(&launch_intent(&store, id).unwrap().unwrap()).unwrap();
    assert_eq!(plan.config_revision, "corrected-512");
    assert!(
        plan.args
            .windows(2)
            .any(|args| args == ["--max-tool-calls", "512"])
    );
}

#[test]
fn capacity_wait_survives_restart_and_never_redispatches_a_recorded_attempt() {
    let (f, previous) = retry_fixture();
    blocker(&f, true);
    let request = queued(invoke_retry(&f, &previous, &["--execute"]));
    assert!(
        request["reason"]
            .as_str()
            .unwrap()
            .contains("capacity full: limit 1, reserved or unresolved 1")
    );
    let mut launcher = RecordingLauncher::default();
    drain_fixture(&f, &mut launcher);
    assert!(launcher.0.is_empty());
    assert_eq!(summary(&f)["operator_requests"][0]["status"], "pending");
    blocker(&f, false);
    drain_fixture(&f, &mut launcher);
    assert_eq!(launcher.0.len(), 1);
    assert_eq!(
        launcher.0[0].attempt_id,
        request["attempt_id"].as_str().unwrap()
    );
    assert_eq!(summary(&f)["operator_requests"][0]["status"], "applied");
    // Simulate death after durable launch intent but before inbox acknowledgement.
    let db =
        rusqlite::Connection::open(f.config.state_root.join("operator-requests/inbox.sqlite3"))
            .unwrap();
    db.execute("UPDATE requests SET status='pending'", [])
        .unwrap();
    drain_fixture(&f, &mut launcher);
    assert_eq!(launcher.0.len(), 1);
    assert_eq!(summary(&f)["operator_requests"][0]["status"], "recorded");
}

fn retry_with_revision(
    f: &CliFixture,
    previous: &ExistingAttempt,
    revision: &str,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args([
            "retry",
            &previous.task,
            "--attempt",
            &previous.previous,
            "--config",
            f.config_path.to_str().unwrap(),
            "--config-revision",
            revision,
            "--actor",
            "acoliver",
            "--reason",
            "correct budget",
            "--execute",
        ])
        .env("PATH", &f.path)
        .output()
        .unwrap()
}

#[test]
fn unchanged_revision_and_claim_drift_are_refused_immediately_even_when_busy() {
    let (f, previous) = retry_fixture();
    // Revision failure is specific both with and without lock contention.
    let out = retry_with_revision(&f, &previous, "old");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("config revision unchanged"));
    let _store = StateStore::open(&f.config.state_root, 1).unwrap();
    let out = retry_with_revision(&f, &previous, "old");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("config revision unchanged"));
    fs::remove_file(f._dir.path().join("assigned")).unwrap();
    let out = invoke_retry(&f, &previous, &["--execute"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("claim drift"));
    assert!(
        summary(&f)["operator_requests"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn queued_retry_revalidates_claim_instead_of_launching_after_drift() {
    let (f, previous) = retry_fixture();
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    queued(invoke_retry(&f, &previous, &["--execute"]));
    drop(store);
    fs::remove_file(f._dir.path().join("assigned")).unwrap();
    let mut launcher = RecordingLauncher::default();
    drain_fixture(&f, &mut launcher);
    assert!(launcher.0.is_empty());
    let request = &summary(&f)["operator_requests"][0];
    assert_eq!(request["status"], "refused");
    assert!(request["reason"].as_str().unwrap().contains("claim drift"));
}

#[test]
fn queued_retry_is_bound_to_its_exact_previous_attempt() {
    let (f, previous) = retry_fixture();
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    queued(invoke_retry(&f, &previous, &["--execute"]));
    drop(store);
    let db = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    db.execute("INSERT INTO attempts(id,task_id,lifecycle,outcome) VALUES('newer',?1,'completed','fixture')", [&previous.task]).unwrap();
    let mut launcher = RecordingLauncher::default();
    drain_fixture(&f, &mut launcher);
    assert!(launcher.0.is_empty());
    let request = &summary(&f)["operator_requests"][0];
    assert_eq!(request["status"], "refused");
    assert_eq!(
        request["reason"],
        "latest attempt changed; request is stale"
    );
}

fn present_pr(f: &CliFixture) {
    let gh = f._dir.path().join("gh");
    let script = fs::read_to_string(&gh).unwrap();
    let body = "Tracker-Issue: https://github.com/org/tracker/issues/7";
    let pr = json!({"id":11,"number":1,"state":"open","html_url":"https://github.com/org/code/pull/1",
        "body":body,"draft":false,"created_at":"2026-10-01T00:00:00Z","user":{"login":"acoliver"},
        "base":{"repo":{"id":1,"full_name":"org/code"},"ref":"main"},
        "head":{"repo":{"id":1,"full_name":"org/code"},"ref":"task","sha":"abc"}});
    let needle = "repos/org/code/pulls?*) printf '%s\\n' '[]' ;;";
    assert!(script.contains(needle));
    let replacement = format!(
        "repos/org/code/pulls/1) printf '%s\\n' '{pr}' ;;\n repos/org/code/pulls?*) printf '%s\\n' '{}' ;;",
        json!([{"number":1,"body":body}])
    );
    fs::write(gh, script.replace(needle, &replacement)).unwrap();
}

#[test]
fn pr_present_is_refused_immediately_during_contention_and_on_queued_revalidation() {
    let (f, previous) = retry_fixture();
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    queued(invoke_retry(&f, &previous, &["--execute"]));
    present_pr(&f);
    let out = invoke_retry(&f, &previous, &["--execute"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("PR present"));
    drop(store);
    let mut launcher = RecordingLauncher::default();
    drain_fixture(&f, &mut launcher);
    assert!(launcher.0.is_empty());
    let request = &summary(&f)["operator_requests"][0];
    assert_eq!(request["status"], "refused");
    assert!(request["reason"].as_str().unwrap().contains("PR present"));
}

fn paused_fixture() -> (CliFixture, String) {
    let f = cli_fixture(false);
    let marker = f._dir.path().join("started");
    fs::write(&f.config.initial.executable, format!("#!/bin/sh\nif test -f '{}'; then exit 2; fi\ntouch '{}'\ntrap 'exit 130' TERM\nwhile :; do sleep 0.05; done\n", marker.display(), marker.display())).unwrap();
    let out = run(
        &f.config_path,
        &f.path,
        "dispatch",
        &[
            "--repository",
            "org/tracker",
            "--issue",
            "7",
            "--config-revision",
            "original",
            "--execute",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let launch: Value = serde_json::from_slice(&out.stdout).unwrap();
    let task = launch["task_id"].as_str().unwrap();
    let attempt = launch["attempt_id"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "fixture worker never started");
        thread::sleep(Duration::from_millis(20));
    }
    let out = run(&f.config_path, &f.path, "pause", &[task]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    worker_exit::await_dispatched_worker_exit(&f.config, task, attempt);
    let out = run(&f.config_path, &f.path, "reconcile", &[task]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(task_phase(&store, task).unwrap().as_deref(), Some("paused"));
    drop(store);
    (f, task.into())
}

fn resume(f: &CliFixture, task: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args([
            "resume",
            task,
            "--config",
            f.config_path.to_str().unwrap(),
            "--execute",
        ])
        .env("PATH", &f.path)
        .output()
        .unwrap()
}

#[test]
fn resume_queues_while_busy_waits_for_capacity_and_preserves_paused_state() {
    let (f, task) = paused_fixture();
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    let request = queued(resume(&f, &task));
    assert_eq!(request["reason"], "coordinator busy; request queued");
    assert_eq!(
        queued(resume(&f, &task))["request_id"],
        request["request_id"]
    );
    drop(store);
    blocker(&f, true);
    let mut launcher = RecordingLauncher::default();
    drain_fixture(&f, &mut launcher);
    assert!(launcher.0.is_empty());
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        task_phase(&store, &task).unwrap().as_deref(),
        Some("paused")
    );
    drop(store);
    let request_status = &summary(&f)["operator_requests"][0];
    assert_eq!(request_status["status"], "pending");
    assert!(
        request_status["reason"]
            .as_str()
            .unwrap()
            .contains("capacity full")
    );
    blocker(&f, false);
    drain_fixture(&f, &mut launcher);
    assert_eq!(launcher.0.len(), 1);
    assert_eq!(
        launcher.0[0].attempt_id,
        request["attempt_id"].as_str().unwrap()
    );
    assert_eq!(launcher.0[0].session_id, task);
    assert_eq!(summary(&f)["operator_requests"][0]["status"], "applied");
}

#[test]
fn resume_at_full_capacity_is_accepted_without_holding_the_paused_task() {
    let (f, task) = paused_fixture();
    blocker(&f, true);
    let request = queued(resume(&f, &task));
    assert!(
        request["reason"]
            .as_str()
            .unwrap()
            .contains("capacity full")
    );
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        task_phase(&store, &task).unwrap().as_deref(),
        Some("paused")
    );
}

#[test]
fn concurrent_operator_submissions_coalesce_to_one_durable_request() {
    let (f, previous) = retry_fixture();
    let _store = StateStore::open(&f.config.state_root, 1).unwrap();
    let (first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| invoke_retry(&f, &previous, &["--execute"]));
        let second = scope.spawn(|| invoke_retry(&f, &previous, &["--execute"]));
        (
            queued(first.join().unwrap()),
            queued(second.join().unwrap()),
        )
    });
    assert_eq!(first["request_id"], second["request_id"]);
    assert_eq!(
        summary(&f)["operator_requests"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn queued_resume_retains_decision_and_paused_state_across_transient_github_errors() {
    let (f, task) = paused_fixture();
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    queued(resume(&f, &task));
    drop(store);
    let gh = f._dir.path().join("gh");
    let original = fs::read_to_string(&gh).unwrap();
    let mut launcher = RecordingLauncher::default();
    for endpoint in ["user)", "graphql)"] {
        assert!(original.contains(endpoint));
        let failing = original.replacen(
            endpoint,
            &format!("{endpoint} printf '%s\\n' 'HTTP 503' >&2; exit 1 ;;\n unused)"),
            1,
        );
        fs::write(&gh, failing).unwrap();
        drain_fixture(&f, &mut launcher);
        assert!(launcher.0.is_empty());
        let state = summary(&f);
        assert_eq!(
            state["operator_requests"][0]["status"], "pending",
            "{endpoint}: {}",
            state["operator_requests"]
        );
        assert!(
            state["operator_requests"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("temporarily unavailable")
        );
        let store = StateStore::open(&f.config.state_root, 1).unwrap();
        assert_eq!(
            task_phase(&store, &task).unwrap().as_deref(),
            Some("paused")
        );
    }
    fs::write(&gh, original).unwrap();
    drain_fixture(&f, &mut launcher);
    assert_eq!(launcher.0.len(), 1);
    assert_eq!(summary(&f)["operator_requests"][0]["status"], "applied");
}
