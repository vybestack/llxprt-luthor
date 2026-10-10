use super::harness::{Harness, stderr};
use luthor::state::StateStore;
use luthor::state::{scheduling, task_records};
use std::fs;

pub(crate) fn dispatch_execute_holds_unresolved_source_before_project_selection_with_spare_capacity()
 {
    let h = Harness::new();
    h.seed_held_task();
    let mut store = StateStore::open(&h.state, 2).unwrap();
    task_records::record_claim_intent(&mut store, "task", "agent", "org/tracker", 7).unwrap();
    assert_eq!(
        scheduling::unresolved_sources(&store).unwrap()[0].1,
        "claim_assignment"
    );
    drop(store);
    let (assignments, worker) = h.dispatch_gh();
    let output = h.run(&[
        "dispatch",
        "--execute",
        "--config",
        h.config.to_str().unwrap(),
        "--repository",
        "org/tracker",
        "--issue",
        "8",
        "--config-revision",
        "rev",
    ]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("startup reconciliation"),
        "{}",
        stderr(&output)
    );
    assert!(
        !h.log.exists(),
        "project selection ran before reconciliation"
    );
    assert_eq!(fs::read_to_string(assignments).unwrap(), "");
    assert!(!worker.exists());
    let store = StateStore::open(&h.state, 2).unwrap();
    assert_eq!(
        scheduling::unresolved_sources(&store).unwrap()[0].1,
        "claim_assignment"
    );
}

pub(crate) fn dispatch_execute_holds_uncertain_attempt_with_spare_capacity() {
    let h = Harness::new();
    h.seed_held_task();
    let db = rusqlite::Connection::open(h.state.join("state.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES('old-attempt','task','launch_intended')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO reservations(attempt_id,task_id,status) VALUES('old-attempt','task','reserved')",
        [],
    )
    .unwrap();
    drop(db);
    let store = StateStore::open(&h.state, 2).unwrap();
    assert!(
        scheduling::pending_attempts(&store)
            .unwrap()
            .contains(&("task".into(), "old-attempt".into()))
    );
    assert!(scheduling::unresolved_sources(&store).unwrap().is_empty());
    drop(store);
    let (assignments, worker) = h.dispatch_gh();
    let output = h.run(&[
        "dispatch",
        "--execute",
        "--config",
        h.config.to_str().unwrap(),
        "--repository",
        "org/tracker",
        "--issue",
        "8",
        "--config-revision",
        "rev",
    ]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("startup reconciliation"),
        "{}",
        stderr(&output)
    );
    assert!(!h.log.exists());
    assert_eq!(fs::read_to_string(assignments).unwrap(), "");
    assert!(!worker.exists());
}

pub(crate) fn dispatch_execute_admits_second_issue_beside_verified_live_worker() {
    let h = Harness::new();
    h.seed_running_task();
    let (assignments, worker) = h.dispatch_gh();
    let output = h.run(&[
        "dispatch",
        "--execute",
        "--config",
        h.config.to_str().unwrap(),
        "--repository",
        "org/tracker",
        "--issue",
        "8",
        "--config-revision",
        "rev",
    ]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("claim failed"),
        "{}",
        stderr(&output)
    );
    assert!(!stderr(&output).contains("startup reconciliation"));
    let calls = fs::read_to_string(&h.log).unwrap();
    assert!(calls.contains("api graphql"), "{calls}");
    assert!(calls.contains("api user --jq .login"), "{calls}");
    let assignment_calls = fs::read_to_string(assignments).unwrap();
    let assignment_calls: Vec<_> = assignment_calls.lines().collect();
    assert_eq!(assignment_calls.len(), 1, "{assignment_calls:?}");
    assert!(
        assignment_calls[0].contains("repos/org/tracker/issues/8/assignees")
            && assignment_calls[0].contains("assignees[]=agent"),
        "{assignment_calls:?}"
    );
    assert!(!assignment_calls[0].contains("issues/7"));
    assert!(!worker.exists());
    let store = StateStore::open(&h.state, 2).unwrap();
    assert_eq!(
        task_records::latest_attempt(&store, "task")
            .unwrap()
            .as_deref(),
        Some("running-attempt")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    drop(store);
    assert_eq!(
        fs::read_to_string(h._dir.path().join("a-starts.log")).unwrap(),
        "started\n"
    );
    h.stop_running_task();
}

pub(crate) fn dispatch_execute_holds_live_child_without_gate_send_proof() {
    let h = Harness::new();
    h.seed_running_task();
    let db = rusqlite::Connection::open(h.state.join("state.sqlite3")).unwrap();
    db.execute("DELETE FROM evidence WHERE task_id='task' AND attempt_id='running-attempt' AND kind='gate_sent'", []).unwrap();
    drop(db);
    let (assignments, worker) = h.dispatch_gh();
    let output = h.run(&[
        "dispatch",
        "--execute",
        "--config",
        h.config.to_str().unwrap(),
        "--repository",
        "org/tracker",
        "--issue",
        "8",
        "--config-revision",
        "rev",
    ]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("startup reconciliation"));
    assert!(!h.log.exists());
    assert_eq!(fs::read_to_string(assignments).unwrap(), "");
    assert!(!worker.exists());
    h.stop_running_task();
}

pub(crate) fn dispatch_execute_clean_store_reaches_eligible_project_candidate() {
    let h = Harness::new();
    let (assignments, worker) = h.dispatch_gh();
    let output = h.run(&[
        "dispatch",
        "--execute",
        "--config",
        h.config.to_str().unwrap(),
        "--repository",
        "org/tracker",
        "--issue",
        "8",
        "--config-revision",
        "rev",
    ]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("worktree failed"),
        "{}",
        stderr(&output)
    );
    assert!(!stderr(&output).contains("authorized PR author"));
    let calls = fs::read_to_string(&h.log).unwrap();
    assert!(calls.contains("api graphql"), "{calls}");
    assert!(calls.contains("api user --jq .login"), "{calls}");
    assert_eq!(fs::read_to_string(assignments).unwrap(), "");
    assert!(!worker.exists());
}

pub(crate) fn dispatch_without_execute_does_not_open_state_or_call_github() {
    let h = Harness::new();
    let (assignments, worker) = h.dispatch_gh();
    let output = h.run(&[
        "dispatch",
        "--config",
        h.config.to_str().unwrap(),
        "--repository",
        "org/tracker",
        "--issue",
        "8",
        "--config-revision",
        "rev",
    ]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("pass --execute"),
        "{}",
        stderr(&output)
    );
    assert!(!h.state.exists());
    assert!(!h.log.exists());
    assert_eq!(fs::read_to_string(assignments).unwrap(), "");
    assert!(!worker.exists());
}
