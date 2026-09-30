#[test]
fn evidence_payloads_are_ordered_task_scoped_and_durable() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task("task", &candidate("issue", 1), "rev", &config())
        .unwrap();
    store
        .create_task("other-task", &candidate("other-issue", 2), "rev", &config())
        .unwrap();
    store
        .record_evidence(
            "task",
            Some("shared-attempt"),
            "tracked_descendant",
            "first",
        )
        .unwrap();
    store
        .record_evidence(
            "task",
            Some("shared-attempt"),
            "tracked_descendant",
            "second",
        )
        .unwrap();
    store
        .record_evidence(
            "other-task",
            Some("shared-attempt"),
            "tracked_descendant",
            "other task",
        )
        .unwrap();
    store
        .record_evidence(
            "task",
            Some("shared-attempt"),
            "different_kind",
            "other kind",
        )
        .unwrap();

    assert_eq!(
        store
            .evidence_payloads("task", "shared-attempt", "tracked_descendant")
            .unwrap(),
        vec!["first", "second"]
    );
    assert!(
        store
            .evidence_payloads("missing", "shared-attempt", "tracked_descendant")
            .unwrap()
            .is_empty()
    );
    drop(store);

    let reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(
        reopened
            .evidence_payloads("task", "shared-attempt", "tracked_descendant")
            .unwrap(),
        vec!["first", "second"]
    );
}

#[test]
fn migrates_v2_released_reservation_and_preserves_attempt_history() {
    use rusqlite::Connection;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.sqlite3");
    let connection = Connection::open(&db).unwrap();
    connection.execute_batch("CREATE TABLE tasks (id TEXT PRIMARY KEY, tracker_repo_id TEXT NOT NULL, issue_node_id TEXT NOT NULL, repository TEXT NOT NULL, issue_number INTEGER NOT NULL, state TEXT NOT NULL, config_revision TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(tracker_repo_id, issue_node_id)); CREATE TABLE attempts (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), lifecycle TEXT NOT NULL, outcome TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE intents (sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE reservations (task_id TEXT PRIMARY KEY REFERENCES tasks(id), attempt_id TEXT NOT NULL UNIQUE, status TEXT NOT NULL CHECK(status IN ('reserved','released')), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE evidence (sequence INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE state_meta(key TEXT PRIMARY KEY, value INTEGER NOT NULL); INSERT INTO state_meta VALUES('capacity',1); INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision) VALUES('task','repo-id','issue-id','org/repo',7,'preparing','rev'); INSERT INTO attempts(id,task_id,lifecycle) VALUES('old-attempt','task','completed'); INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','old-attempt','result','old evidence'); INSERT INTO reservations(task_id,attempt_id,status) VALUES('task','old-attempt','released'); PRAGMA user_version=2;").unwrap();
    drop(connection);

    let mut store = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(store.task_count().unwrap(), 1);
    assert_eq!(store.reservation_count().unwrap(), 0);
    store.reserve("task", "new-attempt").unwrap();
    assert_eq!(store.reservation_count().unwrap(), 1);
    drop(store);

    let connection = Connection::open(&db).unwrap();
    let version: i32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let rows: Vec<(String, String)> = connection
        .prepare("SELECT attempt_id,status FROM reservations ORDER BY attempt_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("new-attempt".into(), "reserved".into()),
            ("old-attempt".into(), "released".into()),
        ]
    );
    let attempt_ids: Vec<String> = connection
        .prepare("SELECT id FROM attempts ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(attempt_ids, vec!["new-attempt", "old-attempt"]);
    let evidence_attempt_ids: Vec<Option<String>> = connection
        .prepare("SELECT attempt_id FROM evidence ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(evidence_attempt_ids, vec![Some("old-attempt".into())]);
    drop(connection);

    let reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(reopened.reservation_count().unwrap(), 1);
}

#[test]
fn migrates_v1_database_atomically_and_preserves_records_and_capacity() {
    use rusqlite::Connection;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.sqlite3");
    let connection = Connection::open(&db).unwrap();
    connection.execute_batch("CREATE TABLE tasks (id TEXT PRIMARY KEY, tracker_repo_id TEXT NOT NULL, issue_node_id TEXT NOT NULL, repository TEXT NOT NULL, issue_number INTEGER NOT NULL, state TEXT NOT NULL, config_revision TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(tracker_repo_id, issue_node_id)); CREATE TABLE attempts (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), lifecycle TEXT NOT NULL, outcome TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE intents (sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE reservations (task_id TEXT PRIMARY KEY REFERENCES tasks(id), attempt_id TEXT NOT NULL UNIQUE, status TEXT NOT NULL CHECK(status IN ('reserved','released')), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE evidence (sequence INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision) VALUES('task','repo-id','issue-id','org/repo',7,'preparing','rev'); INSERT INTO attempts(id,task_id,lifecycle) VALUES('attempt','task','launch_intended'); INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('intent','task','attempt','launch','detail'); INSERT INTO reservations(task_id,attempt_id,status) VALUES('task','attempt','reserved'); INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt','project','item'); PRAGMA user_version=1;").unwrap();
    drop(connection);
    drop(StateStore::open(dir.path(), 2).unwrap());
    let connection = Connection::open(&db).unwrap();
    let version: i32 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    for (table, expected) in [
        ("tasks", 1),
        ("attempts", 1),
        ("intents", 1),
        ("reservations", 1),
        ("evidence", 1),
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, expected, "{table}");
    }
    assert_eq!(
        connection
            .query_row::<i64, _, _>(
                "SELECT value FROM state_meta WHERE key='capacity'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        2
    );
    drop(connection);
    drop(StateStore::open(dir.path(), 2).unwrap());
    assert!(matches!(
        StateStore::open(dir.path(), 3),
        Err(StateError::CapacityMismatch { .. })
    ));
}

#[test]
fn failed_v1_migration_rolls_back_schema_and_version() {
    use rusqlite::Connection;
    let dir = tempfile::tempdir().unwrap();
    let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    connection.execute_batch("CREATE TABLE tasks (id TEXT PRIMARY KEY, tracker_repo_id TEXT NOT NULL, issue_node_id TEXT NOT NULL, repository TEXT NOT NULL, issue_number INTEGER NOT NULL, state TEXT NOT NULL, config_revision TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(tracker_repo_id, issue_node_id)); CREATE TABLE attempts (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id), lifecycle TEXT NOT NULL, outcome TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE intents (sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, detail TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE reservations (task_id TEXT PRIMARY KEY REFERENCES tasks(id), attempt_id TEXT NOT NULL UNIQUE, status TEXT NOT NULL CHECK(status IN ('reserved','released')), created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE evidence (sequence INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL REFERENCES tasks(id), attempt_id TEXT, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP); CREATE TABLE state_meta(key TEXT PRIMARY KEY, value INTEGER NOT NULL); INSERT INTO state_meta VALUES('capacity',1); PRAGMA user_version=1;").unwrap();
    drop(connection);
    assert!(StateStore::open(dir.path(), 2).is_err());
    let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    let version: i32 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    assert_eq!(
        connection
            .query_row::<i64, _, _>("SELECT COUNT(*) FROM state_meta", [], |r| r.get(0))
            .unwrap(),
        1
    );
}

use luthor::{
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    eligibility::Candidate,
};
use rusqlite::Connection;

fn candidate(issue_id: &str, number: u64) -> Candidate {
    Candidate {
        project_id: "project-1".into(),
        item_id: format!("item-{issue_id}"),
        repository: "org/tracker".into(),
        issue_node_id: issue_id.into(),
        issue_number: number,
        issue_url: format!("https://github.com/org/tracker/issues/{number}"),
        tracker_repo_id: "repo-node-id".into(),
        milestone_id: None,
        milestone_title: None,
        observed_at_unix_secs: 123,
        observed_state: "open".into(),
        observed_assignees: vec![],
        observed_labels: vec!["ready".into()],
        observed_project_fields: vec![],
        marker: Marker::Label {
            name: "ready".into(),
        },
        mapping: Mapping {
            tracker_repository: "org/tracker".into(),
            code_repository: "org/code".into(),
            checkout: "/checkout".into(),
            base_branch: "main".into(),
            push_remote: "origin".into(),
            allowed_pr_head_repository: "bot/fork".into(),
            allowed_pr_author: "bot".into(),
        },
        source: Source {
            project_id: "project-1".into(),
            repositories: vec!["org/tracker".into()],
            ready_marker: Marker::Label {
                name: "ready".into(),
            },
            milestone: None,
        },
    }
}

fn config() -> Config {
    Config {
        state_root: "/state".into(),
        worktree_root: "/worktrees".into(),
        capacity: 1,
        assignment_login: "bot".into(),
        sources: vec![Source {
            project_id: "project-1".into(),
            repositories: vec!["org/tracker".into()],
            ready_marker: Marker::Label {
                name: "ready".into(),
            },
            milestone: None,
        }],
        mappings: vec![Mapping {
            tracker_repository: "org/tracker".into(),
            code_repository: "org/code".into(),
            checkout: "/checkout".into(),
            base_branch: "main".into(),
            push_remote: "origin".into(),
            allowed_pr_head_repository: "bot/fork".into(),
            allowed_pr_author: "bot".into(),
        }],
        initial: CommandTemplate {
            executable: "/bin/worker".into(),
            args: vec!["start".into(), "{task.id}".into()],
        },
        resume: CommandTemplate {
            executable: "/bin/worker".into(),
            args: vec!["resume".into(), "{task.id}".into()],
        },
    }
}

#[test]
fn persisted_capacity_is_authoritative_and_schema_version_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        StateStore::open(dir.path(), 0),
        Err(StateError::InvalidCapacity)
    ));
    drop(StateStore::open(dir.path(), 1).unwrap());
    let mismatch = match StateStore::open(dir.path(), 2) {
        Ok(_) => panic!("capacity mismatch was accepted"),
        Err(error) => error,
    };
    assert!(
        matches!(mismatch, StateError::CapacityMismatch { .. }),
        "unexpected reopen error: {mismatch:?}"
    );
    Connection::open(dir.path().join("state.sqlite3"))
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    assert!(matches!(
        StateStore::open(dir.path(), 1),
        Err(StateError::UnsupportedDatabaseVersion(99))
    ));
}

#[test]
fn invalid_config_does_not_create_task_or_selection_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    let mut invalid = config();
    invalid.initial.args = vec!["--prompt".into(), "PRIVATE-TOKEN: DEMO_VALUE".into()];
    let error = store
        .create_task("t1", &candidate("i1", 1), "rev", &invalid)
        .unwrap_err()
        .to_string();
    assert_eq!(error, "invalid configuration");
    assert!(!error.contains("DEMO_VALUE"));
    assert_eq!(store.task_count().unwrap(), 0);
    assert_eq!(store.selection_evidence("t1").unwrap(), None);
}

#[test]
fn failed_attempt_insert_rolls_back_its_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 2).unwrap();
    store
        .create_task("t1", &candidate("i1", 1), "rev", &config())
        .unwrap();
    store
        .create_task("t2", &candidate("i2", 2), "rev", &config())
        .unwrap();
    store.reserve("t1", "a1").unwrap();
    assert!(store.reserve("t2", "a1").is_err());
    assert_eq!(store.reservation_count().unwrap(), 1);
}

use luthor::state::{StateError, StateStore};

#[test]
fn persists_identity_evidence_and_reservations_transactionally() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task("t1", &candidate("ISSUE1", 1), "rev", &config())
        .unwrap();
    assert!(matches!(
        store.create_task("t2", &candidate("ISSUE1", 1), "rev", &config()),
        Err(StateError::DuplicateTask(_, _))
    ));
    store
        .record_evidence("t1", None, "project", "item-1")
        .unwrap();
    store
        .record_evidence("t1", None, "direct_issue", "issue-1")
        .unwrap();
    assert_eq!(
        store.evidence_kinds("t1").unwrap(),
        vec!["selection", "project", "direct_issue"]
    );
    let selection = store.selection_evidence("t1").unwrap().unwrap();
    assert_eq!(selection.candidate.project_id, "project-1");
    assert_eq!(selection.candidate.item_id, "item-ISSUE1");
    assert_eq!(
        selection.candidate.marker,
        Marker::Label {
            name: "ready".into()
        }
    );
    assert_eq!(selection.candidate.milestone_title, None);
    assert_eq!(selection.candidate.mapping.code_repository, "org/code");
    assert_eq!(selection.candidate.observed_at_unix_secs, 123);
    assert_eq!(selection.candidate.observed_state, "open");
    assert_eq!(selection.candidate.observed_labels, vec!["ready"]);
    assert!(selection.candidate.observed_assignees.is_empty());
    assert_eq!(selection.config_revision, "rev");
    assert_eq!(selection.effective_config.capacity, 1);
    assert_eq!(
        selection.effective_config.worktree_root,
        std::path::PathBuf::from("/worktrees")
    );
    assert_eq!(
        selection.effective_config.initial.args,
        vec!["start", "{task.id}"]
    );
    assert_eq!(
        selection.effective_config.mappings[0].code_repository,
        "org/code"
    );
    assert!(matches!(
        store.create_task("t2", &candidate("ISSUE1", 1), "rev", &config()),
        Err(StateError::DuplicateTask(_, _))
    ));
    assert_eq!(store.task_count().unwrap(), 1);
    assert_eq!(store.selection_evidence("t2").unwrap(), None);
    store.reserve("t1", "a1").unwrap();
    assert!(matches!(
        store.reserve("t1", "a2"),
        Err(StateError::Capacity { .. })
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert_eq!(store.release_reservation("a1").unwrap(), 1);
    assert_eq!(store.reservation_count().unwrap(), 0);
}

#[test]
fn coordinator_lock_is_exclusive_across_processes_and_scoped_to_root() {
    use std::process::Command;
    let held = tempfile::tempdir().unwrap();
    let independent = tempfile::tempdir().unwrap();
    let store = StateStore::open(held.path(), 1).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("coordinator_child_cannot_open_held_root")
        .arg("--nocapture")
        .env("LUTHOR_LOCK_CHILD_ROOT", held.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(StateStore::open(independent.path(), 1).is_ok());
    drop(store);
}

#[test]
fn coordinator_child_cannot_open_held_root() {
    if let Ok(root) = std::env::var("LUTHOR_LOCK_CHILD_ROOT") {
        assert!(StateStore::open(root, 1).is_err());
    }
}

#[test]
fn serializes_coordinator_for_state_directory() {
    let dir = tempfile::tempdir().unwrap();
    let first = StateStore::open(dir.path(), 2).unwrap();
    assert!(StateStore::open(dir.path(), 2).is_err());
    drop(first);
    assert!(StateStore::open(dir.path(), 2).is_ok());
}

#[test]
fn duplicate_identity_does_not_leave_partial_task() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task("t1", &candidate("ISSUE1", 1), "rev", &config())
        .unwrap();
    let _ = store.create_task("t2", &candidate("ISSUE1", 1), "rev", &config());
    assert_eq!(store.task_count().unwrap(), 1);
}

#[test]
fn reservation_history_allows_a_new_attempt_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task("task", &candidate("issue", 1), "rev", &config())
        .unwrap();
    store
        .record_evidence("task", Some("first"), "result", "prior evidence")
        .unwrap();
    store.reserve("task", "first").unwrap();
    assert!(matches!(
        store.reserve("task", "blocked"),
        Err(StateError::Capacity { .. })
    ));
    assert_eq!(store.release_reservation("first").unwrap(), 1);
    drop(store);

    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store.reserve("task", "second").unwrap();
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert_eq!(
        store.evidence_kinds("task").unwrap(),
        vec!["selection", "result"]
    );
    let selection = store.selection_evidence("task").unwrap().unwrap();
    assert_eq!(selection.candidate.project_id, "project-1");
    assert_eq!(selection.candidate.item_id, "item-issue");
    assert_eq!(
        selection.candidate.marker,
        Marker::Label {
            name: "ready".into()
        }
    );
    assert_eq!(selection.candidate.milestone_title, None);
    assert_eq!(selection.candidate.mapping.code_repository, "org/code");
    assert_eq!(selection.candidate.observed_at_unix_secs, 123);
    assert_eq!(selection.candidate.observed_state, "open");
    assert_eq!(selection.candidate.observed_labels, vec!["ready"]);
    assert!(selection.candidate.observed_assignees.is_empty());
    assert_eq!(selection.config_revision, "rev");
    assert_eq!(selection.effective_config.capacity, 1);
    assert_eq!(
        selection.effective_config.worktree_root,
        std::path::PathBuf::from("/worktrees")
    );
    assert_eq!(
        selection.effective_config.initial.args,
        vec!["start", "{task.id}"]
    );
    assert_eq!(
        selection.effective_config.mappings[0].code_repository,
        "org/code"
    );
    let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    let rows: Vec<(String, String)> = connection
        .prepare("SELECT attempt_id,status FROM reservations ORDER BY attempt_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("first".into(), "released".into()),
            ("second".into(), "reserved".into())
        ]
    );
}

#[test]
fn invalid_selections_leave_no_task_or_evidence_after_reopen() {
    for name in [
        "unrelated mapping",
        "unrelated source",
        "mismatched marker",
        "unexpected milestone",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut candidate = candidate(name, 1);
        let mut config = config();
        match name {
            "unrelated mapping" => candidate.mapping.code_repository = "evil/repo".into(),
            "unrelated source" => candidate.source.project_id = "other-project".into(),
            "mismatched marker" => {
                candidate.marker = Marker::Label {
                    name: "other".into(),
                }
            }
            "unexpected milestone" => {
                config.sources[0].milestone = Some("v2".into());
                candidate.source = config.sources[0].clone();
                candidate.milestone_title = Some("unexpected".into());
            }
            _ => unreachable!(),
        }
        let mut store = StateStore::open(dir.path(), 1).unwrap();
        assert!(
            matches!(
                store.create_task("task", &candidate, "rev", &config),
                Err(StateError::InvalidSelection)
            ),
            "{name}"
        );
        drop(store);
        let reopened = StateStore::open(dir.path(), 1).unwrap();
        assert_eq!(reopened.task_count().unwrap(), 0, "{name}");
        assert_eq!(reopened.selection_evidence("task").unwrap(), None, "{name}");
    }
}

#[test]
fn optional_source_persists_actual_issue_milestone_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut candidate = candidate("milestone-issue", 8);
    candidate.milestone_title = Some("0.12.0".into());
    candidate.milestone_id = Some("MILESTONE1".into());
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task("task", &candidate, "rev", &config())
        .unwrap();
    drop(store);
    let reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(reopened.task_count().unwrap(), 1);
    let selection = reopened.selection_evidence("task").unwrap().unwrap();
    assert_eq!(selection.candidate, candidate);
    assert_eq!(selection.candidate.source.milestone, None);
    assert_eq!(
        selection.candidate.milestone_title.as_deref(),
        Some("0.12.0")
    );
    assert_eq!(
        selection.candidate.milestone_id.as_deref(),
        Some("MILESTONE1")
    );
    assert_eq!(reopened.evidence_kinds("task").unwrap(), vec!["selection"]);
}

#[test]
fn configured_milestone_selection_is_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let mut candidate = candidate("milestone-issue", 8);
    let mut config = config();
    config.sources[0].milestone = Some("v2".into());
    candidate.source = config.sources[0].clone();
    candidate.milestone_title = Some("v2".into());
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task("task", &candidate, "rev", &config)
        .unwrap();
    drop(store);
    let reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(reopened.task_count().unwrap(), 1);
    assert_eq!(
        reopened
            .selection_evidence("task")
            .unwrap()
            .unwrap()
            .candidate,
        candidate
    );
}

#[test]
fn verified_open_pr_requires_a_terminal_attempt() {
    use luthor::{
        github::pull_request::PullRequestEvidence,
        pr_evidence::{ExpectedPr, VerifiedOpenPr},
    };

    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task("task", &candidate("issue", 4), "rev", &config())
        .unwrap();

    let expected = ExpectedPr {
        issue_url: "https://github.com/org/tracker/issues/4".into(),
        repository_id: 10,
        repository: "org/code".into(),
        base_branch: "main".into(),
        head_repository_id: 11,
        head_repository: "bot/fork".into(),
        task_branch: "luthor/task-4".into(),
        allowed_author: "bot".into(),
        current_identity: "bot".into(),
    };
    let proof = VerifiedOpenPr::from_matching(
        PullRequestEvidence {
            id: 9,
            number: 4,
            url: "https://github.com/org/code/pull/4".into(),
            repository_id: 10,
            repository: "org/code".into(),
            base_repository_id: 10,
            base_repository: "org/code".into(),
            base_branch: "main".into(),
            head_repository_id: 11,
            head_repository: "bot/fork".into(),
            head_branch: "luthor/task-4".into(),
            author: "bot".into(),
            draft: false,
            tracker_issue_url: "https://github.com/org/tracker/issues/4".into(),
            body: "Tracker-Issue: https://github.com/org/tracker/issues/4".into(),
            checks: Some(vec![]),
            created_at: "2026-09-29T00:00:00Z".into(),
            commit_sha: "abc123".into(),
        },
        &expected,
        "bot",
        "attempt-1",
        42,
    )
    .unwrap();

    assert!(matches!(
        store.record_verified_open_pr("task", "attempt-1", &proof),
        Err(StateError::LaunchBlocked)
    ));
    assert_ne!(
        store.task_phase("task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .contains(&"verified_open_pr".to_owned())
    );
}

#[test]
fn audited_telemetry_lost_pr_completion_is_terminal_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    store
        .create_task(
            "recovered",
            &candidate("recovered-issue", 1),
            "rev",
            &config(),
        )
        .unwrap();

    let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    connection.execute_batch(
        "INSERT INTO attempts(id,task_id,lifecycle,outcome) VALUES('prior-attempt','recovered','completed','success');
         INSERT INTO attempts(id,task_id,lifecycle,outcome) VALUES('recovered-attempt','recovered','telemetry_lost',NULL);
         INSERT INTO reservations(attempt_id,task_id,status) VALUES('prior-attempt','recovered','released');
         INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('prior-stop','recovered','prior-attempt','stop','{}');
         INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('recovered','prior-attempt','attempt_exit','{\"attempt_id\":\"prior-attempt\",\"child_pid\":123,\"boot_identity\":\"boot\",\"child_start_identity\":\"start\",\"exit_code\":0,\"signal\":null,\"stdout_path\":\"stdout\",\"stdout_bytes\":0,\"stderr_path\":\"stderr\",\"stderr_bytes\":0,\"stop_signals\":[15]}'),('recovered','prior-attempt','pause_pr_lookup','{\"status\":{\"status\":\"absent\"}}');
         INSERT INTO reservations(attempt_id,task_id,status) VALUES('recovered-attempt','recovered','released');
         INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES
           ('recovered','recovered-attempt','telemetry_lost','{\"actor\":\"operator\",\"reason\":\"lost receipt\",\"observed_at_unix_secs\":10,\"os_ids\":[123],\"evidence\":{\"worker_absent\":true}}'),
           ('recovered','recovered-attempt','exit_pr_lookup','{\"observed_at_unix_secs\":11,\"repository\":\"org/code\",\"status\":{\"status\":\"open\"}}'),
           ('recovered','recovered-attempt','verified_open_pr','{\"id\":99}'),
           ('recovered',NULL,'claim_verified','{}'),
           ('recovered',NULL,'worktree_created','{}');
         UPDATE tasks SET state='pr_complete' WHERE id='recovered';",
    )
    .unwrap();
    drop(connection);
    drop(store);

    let reopened = StateStore::open(dir.path(), 1).unwrap();
    assert_eq!(
        reopened.pending_attempts().unwrap(),
        Vec::<(String, String)>::new()
    );
    reopened.ensure_dispatch_capacity().unwrap();
}

#[test]
fn stopped_prior_attempt_without_absent_pause_lookup_is_not_terminal() {
    for prior_lookup in [None, Some("exit_pr_lookup")] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = StateStore::open(dir.path(), 1).unwrap();
        store
            .create_task(
                "recovered",
                &candidate("recovered-issue", 1),
                "rev",
                &config(),
            )
            .unwrap();

        let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
        connection.execute_batch(
            "INSERT INTO attempts(id,task_id,lifecycle,outcome) VALUES('prior-attempt','recovered','completed','success');
             INSERT INTO attempts(id,task_id,lifecycle,outcome) VALUES('recovered-attempt','recovered','telemetry_lost',NULL);
             INSERT INTO reservations(attempt_id,task_id,status) VALUES('prior-attempt','recovered','released'),('recovered-attempt','recovered','released');
             INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('prior-stop','recovered','prior-attempt','stop','{}');
             INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES
               ('recovered','prior-attempt','attempt_exit','{}'),
               ('recovered','recovered-attempt','telemetry_lost','{\"actor\":\"operator\",\"reason\":\"lost receipt\",\"observed_at_unix_secs\":10,\"os_ids\":[123],\"evidence\":{\"worker_absent\":true}}'),
               ('recovered','recovered-attempt','exit_pr_lookup','{\"observed_at_unix_secs\":11,\"repository\":\"org/code\",\"status\":{\"status\":\"open\"}}'),
               ('recovered','recovered-attempt','verified_open_pr','{\"id\":99}'),
               ('recovered',NULL,'claim_verified','{}'),('recovered',NULL,'worktree_created','{}');
             UPDATE tasks SET state='pr_complete' WHERE id='recovered';",
        )
        .unwrap();
        if let Some(kind) = prior_lookup {
            connection.execute(
                "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('recovered','prior-attempt',?1,'{\"status\":{\"status\":\"absent\"}}')",
                [kind],
            ).unwrap();
        }
        drop(connection);
        drop(store);

        let reopened = StateStore::open(dir.path(), 1).unwrap();
        assert!(
            reopened
                .pending_attempts()
                .unwrap()
                .contains(&("recovered".to_owned(), "recovered-attempt".to_owned()))
        );
        assert!(reopened.ensure_dispatch_capacity().is_err());
    }
}

#[test]
fn audited_pr_completion_missing_any_recovery_evidence_is_not_terminal() {
    for missing_kind in ["telemetry_lost", "exit_pr_lookup", "verified_open_pr"] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = StateStore::open(dir.path(), 1).unwrap();
        store
            .create_task(
                "recovered",
                &candidate("recovered-issue", 1),
                "rev",
                &config(),
            )
            .unwrap();
        let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
        connection.execute_batch(
            "INSERT INTO attempts(id,task_id,lifecycle,outcome) VALUES('prior-attempt','recovered','completed','success');
             INSERT INTO attempts(id,task_id,lifecycle,outcome) VALUES('recovered-attempt','recovered','telemetry_lost',NULL);
             INSERT INTO reservations(attempt_id,task_id,status) VALUES('prior-attempt','recovered','released'),('recovered-attempt','recovered','released');
             INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES
               ('recovered','prior-attempt','attempt_exit','{}'),('recovered','prior-attempt','exit_pr_lookup','{\"status\":{\"status\":\"absent\"}}'),
               ('recovered','recovered-attempt','telemetry_lost','{\"actor\":\"operator\",\"reason\":\"lost receipt\",\"observed_at_unix_secs\":10,\"os_ids\":[123],\"evidence\":{\"worker_absent\":true}}'),
               ('recovered','recovered-attempt','exit_pr_lookup','{\"observed_at_unix_secs\":11,\"repository\":\"org/code\",\"status\":{\"status\":\"open\"}}'),
               ('recovered','recovered-attempt','verified_open_pr','{\"id\":99}'),
               ('recovered',NULL,'claim_verified','{}'),('recovered',NULL,'worktree_created','{}');
             UPDATE tasks SET state='pr_complete' WHERE id='recovered';"
        ).unwrap();
        connection.execute("DELETE FROM evidence WHERE task_id='recovered' AND attempt_id='recovered-attempt' AND kind=?1", [missing_kind]).unwrap();
        drop(connection);
        drop(store);
        let reopened = StateStore::open(dir.path(), 1).unwrap();
        assert_eq!(
            reopened.task_phase("recovered").unwrap().as_deref(),
            Some("pr_complete"),
            "missing {missing_kind}"
        );
        assert_eq!(
            reopened.pending_attempts().unwrap(),
            vec![("recovered".to_owned(), "recovered-attempt".to_owned())],
            "missing {missing_kind} must remain visible for recovery"
        );
        assert!(
            reopened.ensure_dispatch_capacity().is_err(),
            "missing {missing_kind} must retain capacity"
        );
    }
}

#[test]
fn exit_lookup_capacity_uses_latest_evidence_only() {
    for (statuses, releases_capacity) in [
        (&["error", "absent"][..], true),
        (&["absent", "error"][..], false),
        (&["error"][..], false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = StateStore::open(dir.path(), 1).unwrap();
        store
            .create_task("task", &candidate("issue", 1), "rev", &config())
            .unwrap();
        let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
        connection.execute_batch(
            "INSERT INTO attempts(id,task_id,lifecycle,outcome) VALUES('attempt','task','completed','success');
             INSERT INTO reservations(attempt_id,task_id,status) VALUES('attempt','task','released');
             INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('launch','task','attempt','launch','{}');
             INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES
               ('task','attempt','attempt_exit','{\"stop_signals\":[]}'),
               ('task',NULL,'claim_verified','{}'),('task',NULL,'worktree_created','{}');
             UPDATE tasks SET state='attention' WHERE id='task';",
        ).unwrap();
        for status in statuses {
            connection.execute(
                "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt','exit_pr_lookup',?1)",
                [format!("{{\"status\":{{\"status\":\"{status}\"}}}}")],
            ).unwrap();
        }
        drop(connection);
        drop(store);
        let reopened = StateStore::open(dir.path(), 1).unwrap();
        assert_eq!(
            reopened.ensure_dispatch_capacity().is_ok(),
            releases_capacity,
            "lookup sequence: {statuses:?}"
        );
    }
}
