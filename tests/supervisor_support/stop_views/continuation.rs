use luthor::state::{journal, launches, scheduling, task_records};
#[test]
fn never_dispatched_preserves_legacy_full_config_and_saved_argv_without_rerendering() {
    let (_dir, config, mut store, mut plan) = fixture();
    let db = database(&config);
    let mut selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    selection
        .effective_config
        .initial
        .args
        .extend(["--max-tool-calls".into(), "1024".into()]);
    plan.args.extend(["--max-tool-calls".into(), "1024".into()]);
    let saved = serde_json::to_string_pretty(&plan).unwrap();
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE kind='selection'",
        params![serde_json::to_string(&selection).unwrap()],
    )
    .unwrap();
    db.execute(
        "UPDATE intents SET detail=?1 WHERE kind='launch'",
        params![saved],
    )
    .unwrap();
    let context = store.never_dispatched_context("task", "attempt-1").unwrap();
    assert_eq!(context.selection(), &selection);
    assert_eq!(context.plan(), &plan);
    assert_eq!(context.saved_launch_plan(), saved);
    context
        .authorize(
            &mut store,
            "operator",
            NeverDispatchedReason::LegacyPreflightRecovery,
        )
        .unwrap();
    assert_eq!(
        launches::launch_intent(&store, "attempt-1")
            .unwrap()
            .unwrap(),
        saved
    );
    assert_eq!(
        task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap(),
        selection
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_private_plan_absent(&config);
}

#[test]
fn never_dispatched_authorization_rechecks_changed_saved_plan_and_config() {
    for sql in [
        "UPDATE intents SET detail=json_set(detail,'$.session_environment.home','/different-home') WHERE kind='launch';",
        "UPDATE evidence SET payload=json_set(payload,'$.effective_config.resume.args',json('[\"different prompt\"]')) WHERE kind='selection';",
        "UPDATE attempts SET created_at='changed';",
        "UPDATE reservations SET created_at='changed';",
        "UPDATE tasks SET created_at='changed';",
    ] {
        let (_dir, config, mut store, _plan) = fixture();
        let context = store.never_dispatched_context("task", "attempt-1").unwrap();
        let db = database(&config);
        db.execute_batch(sql).unwrap();
        let fresh = store.never_dispatched_context("task", "attempt-1").unwrap();
        assert_ne!(fresh, context);
        let before = rows(&db, "evidence");
        assert!(
            context
                .authorize(
                    &mut store,
                    "operator",
                    NeverDispatchedReason::LegacyPreflightRecovery
                )
                .is_err()
        );
        assert_eq!(rows(&db, "evidence"), before);
    }
}

#[test]
fn never_dispatched_authorization_audit_failure_rolls_back_without_consuming_context() {
    let (_dir, config, mut store, _plan) = fixture();
    let context = store.never_dispatched_context("task", "attempt-1").unwrap();
    let db = database(&config);
    db.execute_batch("CREATE TRIGGER fail_authorization BEFORE INSERT ON evidence WHEN NEW.kind='never_dispatched_authorized' BEGIN SELECT RAISE(ABORT,'injected audit failure'); END;").unwrap();
    let before: Vec<_> = ["tasks", "attempts", "reservations", "intents", "evidence"]
        .map(|table| rows(&db, table))
        .into();
    assert!(
        context
            .authorize(
                &mut store,
                "operator",
                NeverDispatchedReason::LegacyPreflightRecovery
            )
            .is_err()
    );
    for (index, table) in ["tasks", "attempts", "reservations", "intents", "evidence"]
        .iter()
        .enumerate()
    {
        assert_eq!(rows(&db, table), before[index]);
    }
    assert_eq!(
        store.never_dispatched_context("task", "attempt-1").unwrap(),
        context
    );
    db.execute_batch("DROP TRIGGER fail_authorization;")
        .unwrap();
    context
        .authorize(
            &mut store,
            "operator",
            NeverDispatchedReason::LegacyPreflightRecovery,
        )
        .unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}

use super::super::*;
use luthor::state::NeverDispatchedReason;
use rusqlite::{Connection, params};

fn fixture() -> (
    tempfile::TempDir,
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
) {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    let plan = prepare_initial(&mut store, "task", "attempt-1").unwrap();
    (dir, config, store, plan)
}

fn database(config: &Config) -> Connection {
    Connection::open(config.state_root.join("state.sqlite3")).unwrap()
}

fn rows(db: &Connection, table: &str) -> Vec<String> {
    let sql = match table {
        "tasks" => {
            "SELECT json_array(rowid,id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision,created_at) FROM tasks ORDER BY rowid"
        }
        "attempts" => {
            "SELECT json_array(rowid,id,task_id,lifecycle,outcome,created_at) FROM attempts ORDER BY rowid"
        }
        "reservations" => {
            "SELECT json_array(rowid,attempt_id,task_id,status,created_at) FROM reservations ORDER BY rowid"
        }
        "intents" => {
            "SELECT json_array(sequence,id,task_id,attempt_id,kind,detail,created_at) FROM intents ORDER BY sequence"
        }
        "evidence" => {
            "SELECT json_array(sequence,task_id,attempt_id,kind,payload,created_at) FROM evidence ORDER BY sequence"
        }
        _ => panic!("unsupported fixture table"),
    };
    db.prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
fn assert_private_plan_absent(config: &Config) {
    assert!(
        !config
            .state_root
            .join("attempts/attempt-1.plan.json")
            .exists()
    );
}

fn assert_audit(store: &StateStore, context: &luthor::state::NeverDispatchedContext) {
    let audit =
        journal::evidence_payloads(store, "task", "attempt-1", "never_dispatched_authorized")
            .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&audit[0]).unwrap();
    assert_eq!(audit["actor"], "operator");
    assert_eq!(audit["reason_code"], "legacy_preflight_recovery");
    assert_eq!(audit["task_id"], "task");
    assert_eq!(audit["attempt_id"], "attempt-1");
    assert_eq!(audit["saved_launch_plan"], context.saved_launch_plan());
    assert!(audit["authorized_at_unix_secs"].as_u64().unwrap() > 0);
}

#[test]
fn saved_never_dispatched_context_authorizes_exact_attempt_without_rewriting_history() {
    let (_dir, config, mut store, plan) = fixture();
    let db = database(&config);
    let before: Vec<_> = ["tasks", "attempts", "reservations", "intents", "evidence"]
        .map(|table| rows(&db, table))
        .into();
    let context = store.never_dispatched_context("task", "attempt-1").unwrap();
    assert_eq!(context.plan(), &plan);
    assert_eq!(
        context.saved_launch_plan(),
        launches::launch_intent(&store, "attempt-1")
            .unwrap()
            .unwrap()
    );
    assert_eq!(context.selection().effective_config, (&config).into());
    assert_eq!(context.claim().principal, config.assignment_login);
    assert_eq!(context.claim_verified(), config.assignment_login);
    assert_eq!(context.worktree_identity(), &plan.expected_worktree);
    assert_eq!(context.worktree_intent().path, plan.worktree);
    for (index, table) in ["tasks", "attempts", "reservations", "intents", "evidence"]
        .iter()
        .enumerate()
    {
        assert_eq!(rows(&db, table), before[index]);
    }
    assert_private_plan_absent(&config);
    let authorization = context
        .authorize(
            &mut store,
            "operator",
            NeverDispatchedReason::LegacyPreflightRecovery,
        )
        .unwrap();
    assert_eq!(authorization.context(), &context);
    assert!(authorization.audit_sequence() > 0);
    for (index, table) in ["tasks", "attempts", "reservations", "intents"]
        .iter()
        .enumerate()
    {
        assert_eq!(rows(&db, table), before[index]);
    }
    let after = rows(&db, "evidence");
    assert_eq!(&after[..before[4].len()], before[4]);
    assert_eq!(after.len(), before[4].len() + 1);
    assert_audit(&store, &context);
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_private_plan_absent(&config);
    assert!(store.never_dispatched_context("task", "attempt-1").is_err());
    assert!(
        context
            .authorize(
                &mut store,
                "operator",
                NeverDispatchedReason::LegacyPreflightRecovery
            )
            .is_err()
    );
    assert_eq!(rows(&db, "evidence"), after);
    drop(store);
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    assert!(
        context
            .authorize(
                &mut store,
                "operator",
                NeverDispatchedReason::LegacyPreflightRecovery
            )
            .is_err()
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(rows(&db, "evidence"), after);
}

fn rejected_mutation(sql: &str) {
    let (_dir, config, mut store, _plan) = fixture();
    let context = store.never_dispatched_context("task", "attempt-1").unwrap();
    let db = database(&config);
    db.execute_batch(sql).unwrap();
    let before: Vec<_> = ["tasks", "attempts", "reservations", "intents", "evidence"]
        .map(|table| rows(&db, table))
        .into();
    assert!(
        store.never_dispatched_context("task", "attempt-1").is_err(),
        "{sql}"
    );
    assert!(
        context
            .authorize(
                &mut store,
                "operator",
                NeverDispatchedReason::LegacyPreflightRecovery
            )
            .is_err(),
        "{sql}"
    );
    for (index, table) in ["tasks", "attempts", "reservations", "intents", "evidence"]
        .iter()
        .enumerate()
    {
        assert_eq!(rows(&db, table), before[index], "{sql}");
    }
}

#[test]
fn never_dispatched_rejects_dispatch_process_gate_exit_pr_and_unknown_rows() {
    for kind in [
        "supervisor_ready",
        "child_registered",
        "tracked_descendant",
        "gate_sent",
        "attempt_exit",
        "log_failure",
        "telemetry_lost",
        "verified_open_pr",
        "pause_pr_lookup",
        "exit_pr_lookup",
        "retry_authorized",
        "never_dispatched_authorized",
        "unknown",
    ] {
        for attempt in ["'attempt-1'", "NULL", "'foreign-attempt'"] {
            rejected_mutation(&format!(
                "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task',{attempt},'{kind}','{{}}');"
            ));
        }
    }
    for kind in ["supervisor_dispatch", "gate_release", "stop", "unknown"] {
        for attempt in ["'attempt-1'", "NULL", "'foreign-attempt'"] {
            rejected_mutation(&format!(
                "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('contrary','task',{attempt},'{kind}','{{}}');"
            ));
        }
    }
}

#[test]
fn never_dispatched_rejects_duplicate_missing_and_attempt_scoped_prelaunch_proofs() {
    for kind in ["selection", "claim_verified", "worktree_created"] {
        rejected_mutation(&format!(
            "INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='{kind}';"
        ));
        rejected_mutation(&format!("DELETE FROM evidence WHERE kind='{kind}';"));
        rejected_mutation(&format!(
            "UPDATE evidence SET attempt_id='attempt-1' WHERE kind='{kind}';"
        ));
    }
    for kind in ["claim_assignment", "worktree_create", "launch"] {
        rejected_mutation(&format!(
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail) SELECT 'duplicate',task_id,attempt_id,kind,detail FROM intents WHERE kind='{kind}';"
        ));
        rejected_mutation(&format!("DELETE FROM intents WHERE kind='{kind}';"));
    }
    rejected_mutation("UPDATE intents SET attempt_id=NULL WHERE kind='launch';");
    rejected_mutation("UPDATE intents SET attempt_id='attempt-1' WHERE kind='claim_assignment';");
    rejected_mutation("UPDATE intents SET attempt_id='attempt-1' WHERE kind='worktree_create';");
}

#[test]
fn never_dispatched_rejects_foreign_task_attempt_wide_rows_and_reservations() {
    let foreign = "INSERT INTO tasks SELECT 'foreign', 'other-repo', 'other-issue', repository, issue_number, state, config_revision, created_at FROM tasks;";
    for kind in ["supervisor_dispatch", "launch", "gate_release", "unknown"] {
        rejected_mutation(&format!(
            "{foreign} INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('foreign-intent','foreign','attempt-1','{kind}','{{}}');"
        ));
    }
    for kind in [
        "child_registered",
        "selection",
        "attempt_exit",
        "never_dispatched_authorized",
    ] {
        rejected_mutation(&format!(
            "{foreign} INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('foreign','attempt-1','{kind}','{{}}');"
        ));
    }
    rejected_mutation(&format!(
        "{foreign} UPDATE reservations SET task_id='foreign';"
    ));
    rejected_mutation(
        "INSERT INTO reservations(attempt_id,task_id,status) VALUES('ghost','task','released');",
    );
}

#[test]
fn never_dispatched_requires_held_first_and_latest_unfinished_reserved_exact_attempt() {
    for sql in [
        "UPDATE tasks SET state='claimed';",
        "UPDATE tasks SET state='paused';",
        "UPDATE attempts SET lifecycle='completed';",
        "UPDATE attempts SET outcome='uncertain';",
        "UPDATE reservations SET status='released';",
        "DELETE FROM reservations;",
        "INSERT INTO attempts(id,task_id,lifecycle) VALUES('later','task','launch_intended');",
        "INSERT INTO attempts(rowid,id,task_id,lifecycle,outcome) VALUES(-1,'older','task','completed','historical');",
        "UPDATE attempts SET id='older'; INSERT INTO attempts(id,task_id,lifecycle) VALUES('attempt-1','task','launch_intended');",
    ] {
        rejected_mutation(sql);
    }
    let (_dir, _config, store, _plan) = fixture();
    for (task, attempt) in [
        ("task", "wrong"),
        ("wrong", "attempt-1"),
        ("", ""),
        ("task", "../attempt-1"),
    ] {
        assert!(store.never_dispatched_context(task, attempt).is_err());
    }
}

#[test]
fn never_dispatched_rejects_malformed_duplicate_json_and_conflicting_identity() {
    for sql in [
        "UPDATE intents SET detail='not-json' WHERE kind='launch';",
        "UPDATE intents SET detail=json_set(detail,'$.task_id','wrong') WHERE kind='launch';",
        "UPDATE intents SET detail=json_set(detail,'$.attempt_id','wrong') WHERE kind='launch';",
        "UPDATE intents SET detail=json_set(detail,'$.session_id','wrong') WHERE kind='launch';",
        "UPDATE intents SET detail=json_set(detail,'$.config_revision','wrong') WHERE kind='launch';",
        "UPDATE intents SET detail=json_set(detail,'$.worktree','/wrong') WHERE kind='launch';",
        "UPDATE intents SET detail=json_set(detail,'$.expected_worktree.head','wrong') WHERE kind='launch';",
        "UPDATE intents SET detail=json_set(detail,'$.session_environment.home','relative') WHERE kind='launch';",
        "UPDATE intents SET detail=json_set(detail,'$.unexpected',1) WHERE kind='launch';",
        "UPDATE intents SET detail=replace(detail,'\"task_id\":\"task\"','\"task_id\":\"task\",\"task_id\":\"task\"') WHERE kind='launch';",
        "UPDATE intents SET detail=replace(detail,'\"home\":','\"home\":\"/tmp\",\"home\":') WHERE kind='launch';",
        "UPDATE intents SET detail=json_set(detail,'$.principal','wrong') WHERE kind='claim_assignment';",
        "UPDATE intents SET detail=json_set(detail,'$.branch','wrong') WHERE kind='worktree_create';",
        "UPDATE evidence SET payload='wrong' WHERE kind='claim_verified';",
        "UPDATE evidence SET payload='{}' WHERE kind='selection';",
        "UPDATE evidence SET payload=json_set(payload,'$.candidate.issue_number',99) WHERE kind='selection';",
        "UPDATE evidence SET payload=json_set(payload,'$.effective_config.assignment_login','wrong') WHERE kind='selection';",
        "UPDATE evidence SET payload=json_set(payload,'$.effective_config.mappings',json('[]')) WHERE kind='selection';",
        "UPDATE tasks SET config_revision='changed';",
        "UPDATE evidence SET payload=json_set(payload,'$.effective_config.capacity',0) WHERE kind='selection';",
        "UPDATE state_meta SET value=2 WHERE key='capacity';",
        "UPDATE evidence SET payload=json_set(payload,'$.candidate.item_id','') WHERE kind='selection';",
        r#"UPDATE evidence SET payload=replace(payload,'"config_revision":"rev"','"config_revision":"rev","config_revision":"rev"') WHERE kind='selection';"#,
        r#"UPDATE intents SET detail=replace(detail,'"principal":"operator"','"principal":"operator","principal":"operator"') WHERE kind='claim_assignment';"#,
        "UPDATE evidence SET payload=json_set(payload,'$.remote','wrong') WHERE kind='worktree_created';",
    ] {
        rejected_mutation(sql);
    }
}

#[test]
fn never_dispatched_authorization_refuses_stale_but_still_eligible_context() {
    let (_dir, config, mut store, _plan) = fixture();
    let context = store.never_dispatched_context("task", "attempt-1").unwrap();
    let db = database(&config);
    db.execute("INSERT INTO evidence(task_id,kind,payload) VALUES('task','held_reason','inspection changed')", []).unwrap();
    let fresh = store.never_dispatched_context("task", "attempt-1").unwrap();
    assert_ne!(fresh, context);
    let before = rows(&db, "evidence");
    assert!(
        context
            .authorize(
                &mut store,
                "operator",
                NeverDispatchedReason::LegacyPreflightRecovery
            )
            .is_err()
    );
    assert_eq!(rows(&db, "evidence"), before);
    fresh
        .authorize(
            &mut store,
            "operator",
            NeverDispatchedReason::LegacyPreflightRecovery,
        )
        .unwrap();
}

#[test]
fn never_dispatched_authorization_bounds_actor_and_binds_state_store() {
    let (_dir, config, mut store, _plan) = fixture();
    let context = store.never_dispatched_context("task", "attempt-1").unwrap();
    let db = database(&config);
    let before = rows(&db, "evidence");
    for actor in [
        "",
        " ",
        " operator",
        "operator\n",
        "other",
        &"x".repeat(129),
    ] {
        assert!(
            context
                .authorize(
                    &mut store,
                    actor,
                    NeverDispatchedReason::LegacyPreflightRecovery
                )
                .is_err()
        );
        assert_eq!(rows(&db, "evidence"), before);
    }
    let (_other_dir, other_config, mut other, _other_plan) = fixture();
    assert!(
        context
            .authorize(
                &mut other,
                "operator",
                NeverDispatchedReason::LegacyPreflightRecovery
            )
            .is_err()
    );
    assert!(
        journal::evidence_payloads(&other, "task", "attempt-1", "never_dispatched_authorized")
            .unwrap()
            .is_empty()
    );
    assert_eq!(scheduling::reservation_count(&other).unwrap(), 1);
    assert!(other_config.state_root.exists());
    let value: serde_json::Value = serde_json::from_str(context.saved_launch_plan()).unwrap();
    db.execute(
        "UPDATE intents SET detail=?1 WHERE kind='launch'",
        params![serde_json::to_string_pretty(&value).unwrap()],
    )
    .unwrap();
    assert!(store.never_dispatched_context("task", "attempt-1").is_ok());
    assert!(
        context
            .authorize(
                &mut store,
                "operator",
                NeverDispatchedReason::LegacyPreflightRecovery
            )
            .is_err()
    );
}
