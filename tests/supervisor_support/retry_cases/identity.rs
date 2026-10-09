use super::*;
use luthor::WorktreeOwner;
use luthor::state::{journal, scheduling, task_records};
use luthor::supervisor::LaunchPlan;
use std::io::Write;

#[test]
fn two_retries_require_exact_audit_even_when_revision_returns_to_selection() {
    for revision in ["rev", "third-unique"] {
        for authorized in [false, true] {
            exercise_retry_identity(revision, authorized);
        }
    }
}

fn exercise_retry_identity(revision: &str, authorized: bool) {
    let (_dir, mut config, mut store) = retry_fixture();
    let original = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let history = old_retry_rows(&config);
    config
        .resume
        .args
        .extend(["--max-tool-calls".into(), "512".into()]);
    let mut projects = OtherProject(original.candidate.clone(), 1);
    let mut prs = ExitPr::default();
    let mut launcher = OtherLauncher::default();
    let second = retry(&mut store, &config, &mut projects, &mut prs, &mut launcher).unwrap();
    complete_retry(&config, &mut store, &second, &mut projects, &mut prs);
    *config.resume.args.last_mut().unwrap() = "256".into();
    let third = luthor::coordinator::retry_one(
        &mut store,
        luthor::coordinator::RetryDependencies {
            task_id: "task",
            previous_attempt_id: "attempt-retry",
            attempt_id: "attempt-third",
            config: &config,
            config_revision: revision,
            actor: "operator",
            reason: "change retry budget",
            revalidate_terminal_exit: false,
            projects: &mut projects,
            prs: &mut prs,
            launcher: &mut launcher,
        },
    )
    .unwrap();
    assert_eq!(launcher.0, 2);
    assert_eq!(third.config_revision, revision);
    assert!(
        third
            .args
            .windows(2)
            .any(|p| p == ["--max-tool-calls", "256"])
    );
    assert_ne!(third.args, second.args);
    let marker = third.worktree.join("third-worker-ran");
    fs::write(
        &config.resume.executable,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > third-worker-ran\nexit 2\n",
    )
    .unwrap();
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let audit: String = db.query_row(
        "SELECT payload FROM evidence WHERE task_id='task' AND attempt_id='attempt-third' AND kind='retry_authorized'",
        [], |row| row.get(0),
    ).unwrap();
    let audit_value: serde_json::Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(audit_value["previous_plan"]["attempt_id"], "attempt-retry");
    assert_eq!(audit_value["plan"], serde_json::to_value(&third).unwrap());
    if authorized {
        assert_valid_audit_control(&config, &mut store, &third, &db, &audit, &marker);
    } else {
        delete_third_audit(&db);
        assert_missing_audit_refuses_launch(&config, &mut store, &third, &marker);
    }
    assert_eq!(old_retry_rows(&config), history);
    assert_eq!(
        task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap(),
        original
    );
}

fn complete_retry(
    config: &Config,
    store: &mut StateStore,
    plan: &LaunchPlan,
    projects: &mut OtherProject,
    prs: &mut ExitPr,
) {
    let owner = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    execute_with_binary(store, plan, Path::new(env!("CARGO_BIN_EXE_luthor")), &owner).unwrap();
    drop(owner);
    wait_for_retry_exit(config, &plan.attempt_id);
    assert!(matches!(
        luthor::coordinator::reconcile_with_pr(store, "task", &plan.attempt_id, projects, prs)
            .unwrap(),
        Reconciliation::Completed {
            exit_code: Some(2),
            signal: None
        }
    ));
    assert_eq!(
        task_records::task_phase(store, "task").unwrap().as_deref(),
        Some("attention")
    );
}

fn wait_for_retry_exit(config: &Config, attempt: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let path = config
        .state_root
        .join(format!("attempts/{attempt}.receipt.json"));
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "retry receipt missing: {attempt}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let ready: String = db
        .query_row(
            "SELECT payload FROM evidence WHERE attempt_id=?1 AND kind='supervisor_ready'",
            [attempt],
            |row| row.get(0),
        )
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
    wait_for_process_and_group_absence(ready["pid"].as_i64().unwrap() as i32);
}

fn delete_third_audit(db: &rusqlite::Connection) {
    assert_eq!(db.execute(
        "DELETE FROM evidence WHERE task_id='task' AND attempt_id='attempt-third' AND kind='retry_authorized'",
        [],
    ).unwrap(), 1);
}

fn assert_missing_audit_refuses_launch(
    config: &Config,
    store: &mut StateStore,
    plan: &LaunchPlan,
    marker: &Path,
) {
    let owner = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    let result = execute_with_binary(store, plan, Path::new(env!("CARGO_BIN_EXE_luthor")), &owner);
    assert!(
        matches!(result, Err(SupervisorError::ExecutionUnavailable)),
        "missing retry audit must refuse supervisor launch for {}: {result:?}",
        plan.config_revision
    );
    let plan_path = config.state_root.join("attempts/attempt-third.plan.json");
    let mut gate = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .arg("__worker_gate")
        .arg(plan_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    gate.stdin.take().unwrap().write_all(b"R").unwrap();
    let output = gate.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("launch plan does not match verified task or worktree")
    );
    assert!(!marker.exists(), "unauthorized worker ran");
    assert!(
        !config
            .state_root
            .join("attempts/attempt-third.receipt.json")
            .exists()
    );
    for kind in ["child_registered", "supervisor_ready", "gate_sent"] {
        assert!(
            journal::evidence_payloads(store, "task", "attempt-third", kind)
                .unwrap()
                .is_empty()
        );
    }
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let releases: usize = db.query_row(
        "SELECT COUNT(*) FROM intents WHERE task_id='task' AND attempt_id='attempt-third' AND kind='gate_release'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(releases, 0);
    assert!(
        matches!(reconcile_attempt(store, "task", "attempt-third").unwrap(),
        Reconciliation::Held { reason } if reason == "selection mismatch")
    );
    assert_eq!(
        task_records::task_phase(store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(scheduling::reservation_count(store).unwrap(), 1);
}

fn assert_valid_audit_control(
    config: &Config,
    store: &mut StateStore,
    plan: &LaunchPlan,
    db: &rusqlite::Connection,
    audit: &str,
    marker: &Path,
) {
    let owner = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    execute_with_binary(store, plan, Path::new(env!("CARGO_BIN_EXE_luthor")), &owner).unwrap();
    drop(owner);
    wait_for_retry_exit(config, &plan.attempt_id);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(owner) = WorktreeOwner::acquire_existing(store.root(), &plan.task_id) {
            drop(owner);
            break;
        }
        assert!(Instant::now() < deadline, "worktree owner was not released");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        fs::read_to_string(marker)
            .unwrap()
            .contains("--max-tool-calls 256")
    );
    delete_third_audit(db);
    assert!(
        matches!(reconcile_attempt(store, "task", "attempt-third").unwrap(),
        Reconciliation::Held { reason } if reason == "selection mismatch")
    );
    assert_eq!(scheduling::reservation_count(store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(db.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt-third','retry_authorized',?1)",
        [audit],
    ).unwrap(), 1);
    assert!(matches!(
        reconcile_attempt(store, "task", "attempt-third").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(2),
            signal: None
        }
    ));
    assert_eq!(scheduling::reservation_count(store).unwrap(), 0);
}
