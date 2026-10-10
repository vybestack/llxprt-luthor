use super::*;
use crate::model::VerifiedOpenPr;
use crate::state::journal;
use rusqlite::params;
use serde_json::{Value, json};

#[test]
fn amended_recovery_transactions_refuse_even_intervening_or_partial_amendment_proofs() {
    for matching in [false, true] {
        for sql in [
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt','initial_branch_removed','stale audit')",
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','foreign','initial_branch_removed','intervening audit')",
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('seal','task','attempt','initial_branch_removal_seal','stale seal')",
            "UPDATE intents SET detail='{\"amendment_sequence\":1,\"effective_plan\":{}}' WHERE kind='supervisor_dispatch'",
        ] {
            let (_dir, mut store) = fixture();
            let stale_audit = audit().to_string();
            store.connection.execute_batch(sql).unwrap();
            let before = snapshot(&store);
            assert!(
                matches!(
                    commit(&mut store, matching, &stale_audit),
                    Err(StateError::LaunchBlocked)
                ),
                "{matching}: {sql}"
            );
            assert_eq!(snapshot(&store), before);
        }
    }
}

fn audit() -> Value {
    json!({"actor": "operator", "reason": "receipt lost", "observed_at_unix_secs": 10,
        "os_ids": [{"pid": 123}]})
}

fn proof() -> VerifiedOpenPr {
    serde_json::from_value(json!({
        "id": 42, "number": 7, "url": "https://github.com/org/code/pull/7",
        "repository_id": 2, "repository": "org/code", "head_repository_id": 2,
        "head_repository": "org/code", "base_branch": "main", "head_branch": "luthor/task",
        "author": "operator", "active_login": "operator",
        "tracker_issue_url": "https://github.com/org/tracker/issues/7", "draft": true,
        "checks": ["red"], "created_at": "2026-10-01", "head_commit_sha": "abc",
        "observed_at": 10, "attempt_id": "attempt"
    }))
    .unwrap()
}

fn lookup(matching: bool) -> ExitPrEvidence {
    ExitPrEvidence {
        observed_at_unix_secs: 10,
        repository: "org/code".into(),
        status: if matching {
            PausePrStatus::Open
        } else {
            PausePrStatus::Absent
        },
    }
}

fn commit(store: &mut StateStore, matching: bool, audit: &str) -> Result<(), StateError> {
    if matching {
        crate::state::exits::commit_telemetry_lost_pr_completion(
            &mut store.connection,
            "task",
            "attempt",
            audit,
            &lookup(true),
            &proof(),
        )
    } else {
        crate::state::exits::commit_telemetry_lost_recovery(
            &mut store.connection,
            "task",
            "attempt",
            audit,
            &lookup(false),
        )
    }
}

fn fixture() -> (tempfile::TempDir, StateStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::open(dir.path(), 1).unwrap();
    store.connection.execute_batch(
        "INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision)
         VALUES('task','repo','issue','org/tracker',7,'held','rev');
         INSERT INTO attempts(id,task_id,lifecycle) VALUES('attempt','task','launch_intended');
         INSERT INTO reservations(task_id,attempt_id,status) VALUES('task','attempt','reserved');
         INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES
           ('launch','task','attempt','launch','{}'),
           ('dispatch','task','attempt','supervisor_dispatch','{}'),
           ('release','task','attempt','gate_release','{}');
         INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES
           ('task','attempt','supervisor_ready','{}'),
           ('task','attempt','gate_sent','{}'),
           ('task','attempt','child_registered','{}'),
           ('task',NULL,'claim_verified','operator');"
    ).unwrap();
    let mapping = json!({"tracker_repository": "org/tracker", "code_repository": "org/code",
        "checkout": "/fixture/checkout", "base_branch": "main", "push_remote": "origin",
        "allowed_pr_head_repository": "org/code", "allowed_pr_author": "operator"});
    let source = json!({"project_id": "project", "repositories": ["org/tracker"],
        "ready_marker": {"kind": "label", "name": "ready"}, "milestone": null});
    let command = json!({"executable": "/bin/worker", "args": []});
    let selection = json!({"config_revision": "rev", "candidate": {
        "project_id": "project", "item_id": "item", "repository": "org/tracker",
        "issue_node_id": "issue", "issue_number": 7,
        "issue_url": "https://github.com/org/tracker/issues/7", "tracker_repo_id": "repo",
        "milestone_id": null, "milestone_title": null, "observed_at_unix_secs": 1,
        "observed_state": "open", "observed_assignees": [], "observed_labels": ["ready"],
        "observed_project_fields": [], "marker": source["ready_marker"],
        "mapping": mapping, "source": source}, "effective_config": {
        "state_root": dir.path(), "worktree_root": "/fixture/worktrees", "capacity": 1,
        "assignment_login": "operator", "sources": [source], "mappings": [mapping],
        "initial": command, "resume": command}});
    let identity = json!({"path": "/fixture/worktree", "device": 1, "inode": 2,
        "branch": "luthor/task", "base": "main", "head": "abc", "repository": "org/code",
        "git_directory": "/fixture/git", "remote": "git@github.com:org/code.git"});
    let intent = json!({"path": "/fixture/worktree", "branch": "luthor/task",
        "base": "main", "repository": "org/code"});
    store.connection.execute(
        "INSERT INTO intents(id,task_id,kind,detail) VALUES('worktree','task','worktree_create',?1)",
        [intent.to_string()],
    ).unwrap();
    for (kind, payload) in [("selection", selection), ("worktree_created", identity)] {
        store
            .connection
            .execute(
                "INSERT INTO evidence(task_id,kind,payload) VALUES('task',?1,?2)",
                params![kind, payload.to_string()],
            )
            .unwrap();
    }
    (dir, store)
}

fn snapshot(store: &StateStore) -> Value {
    let (lifecycle, outcome, status, phase): (String, Option<String>, String, String) = store
        .connection
        .query_row(
            "SELECT a.lifecycle,a.outcome,r.status,t.state FROM attempts a
            JOIN reservations r ON r.attempt_id=a.id JOIN tasks t ON t.id=a.task_id
            WHERE a.id='attempt'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    let evidence: Vec<(String, String)> = store
        .connection
        .prepare("SELECT kind,payload FROM evidence ORDER BY sequence")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    json!([lifecycle, outcome, status, phase, evidence])
}

#[test]
fn malformed_recovery_audits_preserve_every_persisted_transition() {
    for matching in [false, true] {
        let (_dir, mut store) = fixture();
        let before = snapshot(&store);
        for (key, invalid) in [
            ("actor", json!("  ")),
            ("actor", json!(123)),
            ("reason", json!("\t")),
            ("reason", json!([])),
            ("observed_at_unix_secs", json!(0)),
            ("observed_at_unix_secs", json!("10")),
            ("os_ids", json!([])),
            ("os_ids", json!({})),
            ("exit_code", Value::Null),
            ("signal", Value::Null),
        ] {
            let mut invalid_audit = audit();
            invalid_audit[key] = invalid;
            assert!(
                matches!(
                    commit(&mut store, matching, &invalid_audit.to_string()),
                    Err(StateError::LaunchBlocked)
                ),
                "{matching}: {key}"
            );
            assert_eq!(snapshot(&store), before);
        }
        for key in ["actor", "reason", "observed_at_unix_secs", "os_ids"] {
            let mut invalid_audit = audit();
            invalid_audit.as_object_mut().unwrap().remove(key);
            assert!(
                matches!(
                    commit(&mut store, matching, &invalid_audit.to_string()),
                    Err(StateError::LaunchBlocked)
                ),
                "{matching}: missing {key}"
            );
            assert_eq!(snapshot(&store), before);
        }
        assert!(matches!(
            commit(&mut store, matching, "{partial"),
            Err(StateError::Serialization(_))
        ));
        assert_eq!(snapshot(&store), before);
        commit(&mut store, matching, &audit().to_string()).unwrap();
        let completed = snapshot(&store);
        assert_eq!(completed[0], "telemetry_lost");
        assert_eq!(completed[1], Value::Null);
        assert_eq!(completed[2], "released");
        assert_eq!(completed[3], if matching { "pr_complete" } else { "held" });
        assert!(
            !journal::evidence_kinds(&store, "task")
                .unwrap()
                .contains(&"attempt_exit".into())
        );
        assert!(commit(&mut store, matching, &audit().to_string()).is_err());
        assert_eq!(snapshot(&store), completed);
    }
}

#[test]
fn recovery_transition_failures_roll_back_audit_proof_and_capacity() {
    for matching in [false, true] {
        for target in ["attempts", "reservations", "tasks"] {
            let (_dir, mut store) = fixture();
            let before = snapshot(&store);
            store
                .connection
                .execute_batch(&format!(
                    "CREATE TRIGGER reject_transition BEFORE UPDATE ON {target}
                 BEGIN SELECT RAISE(IGNORE); END;"
                ))
                .unwrap();
            assert!(
                matches!(
                    commit(&mut store, matching, &audit().to_string()),
                    Err(StateError::LaunchBlocked)
                ),
                "{matching}: {target}"
            );
            assert_eq!(snapshot(&store), before);
            store
                .connection
                .execute_batch("DROP TRIGGER reject_transition")
                .unwrap();
            commit(&mut store, matching, &audit().to_string()).unwrap();
        }
    }
}

#[test]
fn completed_exit_replays_require_identical_outcome_reservation_and_receipt() {
    let (_dir, mut store) = fixture();
    crate::state::exits::reconcile_verified_exit(
        &mut store.connection,
        "task",
        "attempt",
        "receipt",
        "outcome",
    )
    .unwrap();
    let completed = snapshot(&store);
    crate::state::exits::reconcile_verified_exit(
        &mut store.connection,
        "task",
        "attempt",
        "receipt",
        "outcome",
    )
    .unwrap();
    assert_eq!(snapshot(&store), completed);
    for (receipt, outcome) in [("different", "outcome"), ("receipt", "different")] {
        assert!(matches!(
            crate::state::exits::reconcile_verified_exit(
                &mut store.connection,
                "task",
                "attempt",
                receipt,
                outcome
            ),
            Err(StateError::LaunchBlocked)
        ));
        assert_eq!(snapshot(&store), completed);
    }
    for sql in [
        "UPDATE reservations SET status='reserved' WHERE attempt_id='attempt'",
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt','attempt_exit','receipt')",
        "UPDATE evidence SET task_id='other' WHERE kind='attempt_exit'",
    ] {
        let tx = store.connection.transaction().unwrap();
        tx.execute_batch("INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision)
            VALUES('other','other-repo','other-issue','org/tracker',8,'held','rev')").unwrap();
        tx.execute_batch(sql).unwrap();
        tx.commit().unwrap();
        let conflicting = snapshot(&store);
        assert!(matches!(
            crate::state::exits::reconcile_verified_exit(
                &mut store.connection,
                "task",
                "attempt",
                "receipt",
                "outcome"
            ),
            Err(StateError::LaunchBlocked)
        ));
        assert_eq!(snapshot(&store), conflicting);
        store.connection.execute_batch(
            "UPDATE reservations SET status='released' WHERE attempt_id='attempt';
             DELETE FROM evidence WHERE kind='attempt_exit';
             INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt','attempt_exit','receipt');
             DELETE FROM tasks WHERE id='other';"
        ).unwrap();
    }
}

#[test]
fn verified_exit_transition_failures_do_not_leave_receipt_or_free_capacity() {
    for target in ["attempts", "reservations"] {
        let (_dir, mut store) = fixture();
        let before = snapshot(&store);
        store
            .connection
            .execute_batch(&format!(
                "CREATE TRIGGER reject_transition BEFORE UPDATE ON {target}
             BEGIN SELECT RAISE(IGNORE); END;"
            ))
            .unwrap();
        assert!(matches!(
            crate::state::exits::reconcile_verified_exit(
                &mut store.connection,
                "task",
                "attempt",
                "receipt",
                "outcome"
            ),
            Err(StateError::LaunchBlocked)
        ));
        assert_eq!(snapshot(&store), before);
        store
            .connection
            .execute_batch("DROP TRIGGER reject_transition")
            .unwrap();
        crate::state::exits::reconcile_verified_exit(
            &mut store.connection,
            "task",
            "attempt",
            "receipt",
            "outcome",
        )
        .unwrap();
    }
}
