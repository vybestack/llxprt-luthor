use super::finish_verified_stopped_attempt_with_pr;
use crate::github::project::{Issue, Page, ProjectItem, ProjectReadError};
use crate::github::pull_request::ErrorCategory;
use crate::state::{journal, scheduling, task_records};
use crate::{
    github::{
        project::ProjectReader,
        pull_request::{LookupError, PullRequestReader},
    },
    state::{PausePrEvidence, PausePrStatus, StateError, StateStore},
    supervisor,
};
use rusqlite::params;
use serde_json::{Value, json};

struct FakePr {
    scenario: &'static str,
    lookups: usize,
}

impl PullRequestReader for FakePr {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok("bot".into())
    }

    fn page(&mut self, repository: &str, page: u32) -> Result<Vec<Value>, LookupError> {
        assert_eq!(repository, "org/code");
        assert_eq!(page, 1);
        self.lookups += 1;
        let entry = |number| {
            json!({
                "number": number,
                "body": "Tracker-Issue: https://github.com/org/tracker/issues/1"
            })
        };
        match self.scenario {
            "absent" => Ok(vec![]),
            "open" => Ok(vec![entry(7)]),
            "ambiguous" => Ok(vec![entry(7), entry(8)]),
            "error" => Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            }),
            _ => unreachable!(),
        }
    }

    fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError> {
        match name {
            "org/code" => Ok(10),
            "org/head" => Ok(20),
            _ => Err(LookupError {
                category: ErrorCategory::Malformed,
                code: "unexpected-repository",
                status: None,
            }),
        }
    }

    fn detail(&mut self, repository: &str, number: u64) -> Result<Value, LookupError> {
        assert_eq!(repository, "org/code");
        Ok(json!({
            "id": number, "number": number, "state": "open",
            "html_url": format!("https://github.com/org/code/pull/{number}"),
            "body": "Tracker-Issue: https://github.com/org/tracker/issues/1",
            "created_at": "2026-01-01T00:00:00Z",
            "base": {"repo": {"id": 10, "full_name": "org/code"}, "ref": "main"},
            "head": {"repo": {"id": 10, "full_name": "org/code"}, "ref": "branch", "sha": "abc123"},
            "user": {"login": "bot"}, "draft": false
        }))
    }
}

fn stopped_store(dir: &tempfile::TempDir) -> StateStore {
    let store = StateStore::open(dir.path(), 1).unwrap();
    let connection = rusqlite::Connection::open(dir.path().join("state.sqlite3")).unwrap();
    let selection = json!({
        "candidate": {
            "project_id": "p", "item_id": "i", "repository": "org/tracker",
            "issue_node_id": "issue", "issue_number": 1,
            "issue_url": "https://github.com/org/tracker/issues/1",
            "tracker_repo_id": "repo", "milestone_id": null, "milestone_title": null,
            "observed_at_unix_secs": 1, "observed_state": "open",
            "observed_assignees": [], "observed_labels": [],
            "observed_project_fields": [], "marker": {"kind": "label", "name": "ready"},
            "mapping": {
                "tracker_repository": "org/tracker", "code_repository": "org/code",
                "checkout": "checkout", "base_branch": "main", "push_remote": "origin",
                "allowed_pr_head_repository": "org/code", "allowed_pr_author": "bot"
            },
            "source": {"project_id": "p", "repositories": ["org/tracker"],
                "ready_marker": {"kind": "label", "name": "ready"}, "milestone": null}
        },
        "config_revision": "r",
        "effective_config": {
            "state_root": "state", "worktree_root": "private", "capacity": 1,
            "assignment_login": "bot", "sources": [], "mappings": [],
            "initial": {"executable": "worker", "args": []},
            "resume": {"executable": "worker", "args": []}
        }
    });
    let receipt = json!({
        "attempt_id": "attempt", "child_pid": 123, "boot_identity": "boot",
        "child_start_identity": "start", "exit_code": null, "signal": 15,
        "stdout_path": "stdout", "stdout_bytes": 0,
        "stderr_path": "stderr", "stderr_bytes": 0, "stop_signals": [15]
    });
    connection.execute(
        "INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision)
         VALUES('task','repo','issue','org/tracker',1,'held','r')", [],
    ).unwrap();
    connection
        .execute(
            "INSERT INTO attempts(id,task_id,lifecycle,outcome)
         VALUES('attempt','task','completed','exit_code=None;signal=Some(15)')",
            [],
        )
        .unwrap();
    connection.execute(
        "INSERT INTO reservations(attempt_id,task_id,status) VALUES('attempt','task','released')", [],
    ).unwrap();
    connection
        .execute(
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail)
         VALUES('stop','task','attempt','stop','')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail)
         VALUES('launch','task','attempt','launch','plan')",
            [],
        )
        .unwrap();
    for kind in ["claim_verified", "worktree_created"] {
        connection
            .execute(
                "INSERT INTO evidence(task_id,kind,payload) VALUES('task',?1,'proof')",
                [kind],
            )
            .unwrap();
    }
    connection.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task',NULL,'selection',?1)",
        [selection.to_string()],
    ).unwrap();
    connection.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt','attempt_exit',?1)",
        [receipt.to_string()],
    ).unwrap();
    store
}

struct NoProject;

impl ProjectReader for NoProject {
    fn page(&mut self, _: &str, _: Option<&str>) -> Result<Page<ProjectItem>, ProjectReadError> {
        panic!("project page should not be read")
    }

    fn issue(&mut self, _: &ProjectItem) -> Result<Issue, ProjectReadError> {
        panic!("issue should not be read")
    }
}

#[test]
fn verified_stopped_attempt_pr_outcomes_gate_slot() {
    for (scenario, expected) in [
        ("absent", PausePrStatus::Absent),
        ("open", PausePrStatus::Open),
        ("ambiguous", PausePrStatus::Ambiguous),
        (
            "error",
            PausePrStatus::Error {
                category: ErrorCategory::Transport,
                code: "offline".into(),
                http_status: None,
            },
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = stopped_store(&dir);
        let mut prs = FakePr {
            scenario,
            lookups: 0,
        };
        let mut projects = NoProject;
        let result = finish_verified_stopped_attempt_with_pr(
            &mut store,
            "task",
            "attempt",
            &mut projects,
            &mut prs,
            supervisor::Reconciliation::Completed {
                exit_code: Some(17),
                signal: Some(15),
            },
        )
        .unwrap();
        assert_eq!(prs.lookups, 1, "{scenario}");
        let proof: PausePrEvidence = serde_json::from_str(
            &journal::evidence_payload(&store, "task", Some("attempt"), "pause_pr_lookup")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(proof.repository, "org/code", "{scenario}");
        assert!(proof.observed_at_unix_secs > 0);
        assert_eq!(proof.status, expected, "{scenario}");
        assert_scenario_outcome(&store, scenario, result);
        assert_eq!(
            scheduling::reservation_count(&store).unwrap(),
            0,
            "{scenario}"
        );
        let connection = rusqlite::Connection::open(dir.path().join("state.sqlite3")).unwrap();
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM evidence WHERE task_id='task' AND attempt_id='attempt' AND kind='pause_pr_lookup'",
            params![], |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1, "{scenario}");
    }
}

fn assert_scenario_outcome(store: &StateStore, scenario: &str, result: supervisor::Reconciliation) {
    if scenario == "absent" {
        assert!(matches!(
            result,
            supervisor::Reconciliation::Completed { .. }
        ));
        assert_eq!(
            task_records::task_phase(store, "task").unwrap().as_deref(),
            Some("paused")
        );
        assert!(scheduling::ensure_dispatch_capacity(store).is_ok());
    } else if scenario == "open" {
        assert_eq!(
            result,
            supervisor::Reconciliation::Held {
                reason: "exit PR evidence unavailable".into(),
            }
        );
        assert_eq!(
            task_records::task_phase(store, "task").unwrap().as_deref(),
            Some("held")
        );
        assert!(
            journal::evidence_payload(store, "task", Some("attempt"), "verified_open_pr")
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            scheduling::ensure_dispatch_capacity(store),
            Err(StateError::Capacity { .. })
        ));
    } else {
        assert!(matches!(result, supervisor::Reconciliation::Held { .. }));
        assert_eq!(
            task_records::task_phase(store, "task").unwrap().as_deref(),
            Some("held")
        );
        assert!(matches!(
            scheduling::ensure_dispatch_capacity(store),
            Err(StateError::Capacity { .. })
        ));
    }
}
