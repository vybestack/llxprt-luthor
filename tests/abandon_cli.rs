//! `luthor abandon` through the real binary: audited, local-only, and never for
//! a task that may still be running.
use luthor::{cli::execute, config::Config, state::StateStore};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn write_config(root: &Path) -> std::path::PathBuf {
    let checkout = root.join("checkout");
    fs::create_dir(&checkout).unwrap();
    let path = root.join("config.json");
    let config = json!({
        "state_root": root.join("state"), "worktree_root": root.join("worktrees"), "capacity": 1,
        "assignment_login": "fixture-agent",
        "sources": [{"project_id":"PROJECT","repositories":["org/tracker"],"ready_marker":{"kind":"label","name":"ready"},"milestone":null}],
        "mappings": [{"tracker_repository":"org/tracker","code_repository":"org/code","checkout":checkout,"base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/code","allowed_pr_author":"acoliver"}],
        "initial": {"executable": root.join("worker"), "args": ["--session", "{task.id}"]},
        "resume": {"executable": root.join("worker"), "args": ["--session", "{task.id}"]}
    });
    fs::write(&path, config.to_string()).unwrap();
    Config::from_json(&config.to_string()).unwrap();
    path
}

/// `stuck` ended with a verified exit and sits in attention; `busy` has an
/// attempt that never recorded one, as a running task does.
fn seed(root: &Path) {
    let store = StateStore::open(root.join("state"), 1).unwrap();
    let db = Connection::open(root.join("state/state.sqlite3")).unwrap();
    for (task, number, state) in [("stuck", 7, "attention"), ("busy", 8, "held")] {
        db.execute(
            "INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision) VALUES(?1,'repo',?1,'org/tracker',?2,?3,'rev')",
            rusqlite::params![task, number, state],
        )
        .unwrap();
    }
    db.execute_batch(
        "INSERT INTO attempts(id,task_id,lifecycle,outcome) VALUES('a1','stuck','completed','exited');
         INSERT INTO reservations(attempt_id,task_id,status) VALUES('a1','stuck','released');
         INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('stuck','a1','attempt_exit','{\"stop_signals\":[]}');
         INSERT INTO attempts(id,task_id,lifecycle) VALUES('b1','busy','launch_intended');
         INSERT INTO reservations(attempt_id,task_id,status) VALUES('b1','busy','reserved');",
    )
    .unwrap();
    drop(store);
}

/// `luthor abandon TASK --config PATH --actor LOGIN --reason TEXT [--execute]`
fn abandon(config: &Path, task: &str, actor: &str, reason: &str, execute: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_luthor"));
    command
        .args(["abandon", task, "--config"])
        .arg(config)
        .args(["--actor", actor, "--reason", reason]);
    if execute {
        command.arg("--execute");
    }
    command.output().unwrap()
}

fn status(root: &Path) -> Value {
    serde_json::from_str(&execute(&root.join("state"), &["status".into()]).unwrap()).unwrap()
}

fn task<'a>(status: &'a Value, id: &str) -> &'a Value {
    status["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["task_id"] == id)
        .unwrap()
}

#[test]
fn abandon_records_actor_and_reason_and_releases_the_slot() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let config = write_config(dir.path());
    let output = abandon(&config, "stuck", "alice", "duplicate of #12", true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed["status"], "abandoned");
    assert_eq!(printed["actor"], "alice");
    assert_eq!(printed["github_changes"], "none");
    let status = status(dir.path());
    let stuck = task(&status, "stuck");
    assert_eq!(stuck["phase"], "abandoned");
    assert_eq!(stuck["reason"], "duplicate of #12");
    assert_eq!(stuck["terminal"]["actor"], "alice");
    assert_eq!(stuck["reserved_slot"], false);
    assert_eq!(task(&status, "busy")["phase"], "held");
}

#[test]
fn abandon_refuses_a_task_that_may_still_be_running() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let config = write_config(dir.path());
    let output = abandon(&config, "busy", "alice", "wrong scope", true);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("a running task cannot be retired"),
        "{stderr}"
    );
    let status = status(dir.path());
    let busy = task(&status, "busy");
    assert_eq!(busy["phase"], "held");
    assert_eq!(busy["reserved_slot"], true);
    assert_eq!(status["reserved_slot_count"], 1);
}

#[test]
fn abandon_requires_every_argument_and_a_known_task() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let config = write_config(dir.path());
    for (output, error) in [
        (
            abandon(&config, "stuck", "alice", "x", false),
            "expected TASK",
        ),
        (
            abandon(&config, "stuck", "alice", " ", true),
            "expected TASK",
        ),
        (abandon(&config, "stuck", "", "x", true), "expected TASK"),
        (
            abandon(&config, "unknown", "alice", "x", true),
            "task not found",
        ),
    ] {
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(task(&status(dir.path()), "stuck")["phase"], "attention");
}
