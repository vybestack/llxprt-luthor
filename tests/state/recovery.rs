use super::*;
use luthor::state::{scheduling, task_records};

pub(crate) fn audited_telemetry_lost_pr_completion_is_terminal_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = StateStore::open(dir.path(), 1).unwrap();
    task_records::create_task(
        &mut store,
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
         INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('recovered-stop','recovered','recovered-attempt','stop','{}');
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
        scheduling::pending_attempts(&reopened).unwrap(),
        Vec::<(String, String)>::new()
    );
    assert!(
        scheduling::unresolved_sources(&reopened)
            .unwrap()
            .is_empty()
    );
    scheduling::ensure_dispatch_capacity(&reopened).unwrap();

    let connection = Connection::open(dir.path().join("state.sqlite3")).unwrap();
    connection.execute_batch(
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('malformed-stop','recovered','recovered-attempt','stop','{}');
         DELETE FROM evidence WHERE task_id='recovered' AND attempt_id='recovered-attempt' AND kind='telemetry_lost';",
    ).unwrap();
    assert_eq!(
        scheduling::unresolved_sources(&reopened).unwrap(),
        vec![
            ("recovered".to_owned(), "stop".to_owned()),
            ("recovered".to_owned(), "stop".to_owned()),
        ]
    );
}

pub(crate) fn stopped_prior_attempt_without_absent_pause_lookup_is_not_terminal() {
    for prior_lookup in [None, Some("exit_pr_lookup")] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = StateStore::open(dir.path(), 1).unwrap();
        task_records::create_task(
            &mut store,
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
            scheduling::pending_attempts(&reopened)
                .unwrap()
                .contains(&("recovered".to_owned(), "recovered-attempt".to_owned()))
        );
        assert!(scheduling::ensure_dispatch_capacity(&reopened).is_err());
    }
}

pub(crate) fn audited_pr_completion_missing_any_recovery_evidence_is_not_terminal() {
    for missing_kind in ["telemetry_lost", "exit_pr_lookup", "verified_open_pr"] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = StateStore::open(dir.path(), 1).unwrap();
        task_records::create_task(
            &mut store,
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
            task_records::task_phase(&reopened, "recovered")
                .unwrap()
                .as_deref(),
            Some("pr_complete"),
            "missing {missing_kind}"
        );
        assert_eq!(
            scheduling::pending_attempts(&reopened).unwrap(),
            vec![("recovered".to_owned(), "recovered-attempt".to_owned())],
            "missing {missing_kind} must remain visible for recovery"
        );
        assert!(
            scheduling::ensure_dispatch_capacity(&reopened).is_err(),
            "missing {missing_kind} must retain capacity"
        );
    }
}

pub(crate) fn exit_lookup_capacity_uses_latest_evidence_only() {
    for (statuses, releases_capacity) in [
        (&["error", "absent"][..], true),
        (&["absent", "error"][..], false),
        (&["error"][..], false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = StateStore::open(dir.path(), 1).unwrap();
        task_records::create_task(&mut store, "task", &candidate("issue", 1), "rev", &config())
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
            scheduling::ensure_dispatch_capacity(&reopened).is_ok(),
            releases_capacity,
            "lookup sequence: {statuses:?}"
        );
    }
}
