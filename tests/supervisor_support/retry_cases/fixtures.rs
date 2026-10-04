use super::*;

#[cfg(unix)]
pub(crate) fn retry_fixture() -> (tempfile::TempDir, Config, StateStore) {
    retry_fixture_with_saved_budget(None)
}

#[cfg(unix)]
pub(crate) fn retry_fixture_with_saved_budget(
    budget: Option<&str>,
) -> (tempfile::TempDir, Config, StateStore) {
    retry_fixture_for_mapping(budget, false)
}

#[cfg(unix)]
pub(crate) fn retry_fixture_for_mapping(
    budget: Option<&str>,
    same_repository: bool,
) -> (tempfile::TempDir, Config, StateStore) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (mut config, candidate) = prompts::configured_mapping(dir.path(), same_repository);
    let worker = dir.path().join("llxprt-code-rs");
    fs::write(&worker, r#"#!/bin/sh
session=''
budget=''
while test "$#" -gt 0; do
  case "$1" in
    --session) session="$2"; shift ;;
    --max-tool-calls) budget="$2"; shift ;;
  esac
  shift
done
if test "$budget" = 1024; then
  printf '{"error":{"code":"max-tool-calls","message":"--max-tool-calls must be -1 or an integer from 1 through 512 (got 1024)"},"session_id":"%s","status":"error"}\n' "$session"
fi
exit 2
"#).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    config.initial.executable = worker.clone();
    config.resume.executable = worker;
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    prompts::claimed_for_mapping(&mut store, &config, &candidate);
    if let Some(budget) = budget {
        // Reproduce a selection saved by a build that accepted the unsupported budget.
        let mut saved = store.selection_evidence("task").unwrap().unwrap();
        saved
            .effective_config
            .initial
            .args
            .extend(["--max-tool-calls".into(), budget.into()]);
        saved
            .effective_config
            .resume
            .args
            .extend(["--max-tool-calls".into(), budget.into()]);
        let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
        db.execute(
            "UPDATE evidence SET payload=?1 WHERE task_id='task' AND kind='selection'",
            [serde_json::to_string(&saved).unwrap()],
        )
        .unwrap();
    }
    let plan = prepare_initial(&mut store, "task", "attempt-real").unwrap();
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    reconcile_fixture_exit(&config, &mut store);
    // A completed receipt may precede the detached supervisor's own termination.
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let ready: String = db.query_row("SELECT payload FROM evidence WHERE attempt_id='attempt-real' AND kind='supervisor_ready'", [], |r| r.get(0)).unwrap();
    let ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
    wait_for_process_and_group_absence(ready["pid"].as_i64().unwrap() as i32);
    assert_eq!(
        store.task_phase("task").unwrap().as_deref(),
        Some("attention")
    );
    (dir, config, store)
}

#[cfg(unix)]
pub(crate) fn retry(
    store: &mut StateStore,
    config: &Config,
    projects: &mut OtherProject,
    prs: &mut ExitPr,
    launcher: &mut impl SupervisorLauncher,
) -> Result<luthor::supervisor::LaunchPlan, luthor::coordinator::DispatchError> {
    luthor::coordinator::retry_one(
        store,
        luthor::coordinator::RetryDependencies {
            task_id: "task",
            previous_attempt_id: "attempt-real",
            attempt_id: "attempt-retry",
            config,
            config_revision: "corrected",
            actor: "operator",
            reason: "correct worker budget",
            revalidate_terminal_exit: false,
            projects,
            prs,
            launcher,
        },
    )
}

#[cfg(unix)]
pub(crate) fn historical_retry_fixture() -> (tempfile::TempDir, Config, StateStore) {
    historical_retry_fixture_with_boot(
        "{ sec = 1790533213, usec = 116017 } Sun Sep 27 15:20:13 2026",
    )
}

#[cfg(unix)]
pub(crate) fn historical_retry_fixture_with_boot(
    old: &str,
) -> (tempfile::TempDir, Config, StateStore) {
    let (dir, mut config, store) = retry_fixture_with_saved_budget(Some("1024"));
    let path = receipt_path(&config);
    let mut receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    receipt.boot_identity = old.into();
    fs::write(path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let path = config.state_root.join("attempts/attempt-real.child.json");
    let mut child: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    child["boot_identity"] = old.into();
    fs::write(path, serde_json::to_vec(&child).unwrap()).unwrap();
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute("UPDATE evidence SET payload=json_set(payload,'$.boot_identity',?1) WHERE attempt_id='attempt-real' AND kind IN ('child_registered','supervisor_ready','gate_sent','tracked_descendant')", [old]).unwrap();
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind='attempt_exit'",
        [serde_json::to_string(&receipt).unwrap()],
    )
    .unwrap();
    db.execute("UPDATE intents SET detail=json_set(detail,'$.boot_identity',?1) WHERE attempt_id='attempt-real' AND kind='gate_release'", [old]).unwrap();
    config
        .initial
        .args
        .extend(["--max-tool-calls".into(), "512".into()]);
    config
        .resume
        .args
        .extend(["--max-tool-calls".into(), "512".into()]);
    (dir, config, store)
}

#[cfg(unix)]
pub(crate) fn historical_retry(
    store: &mut StateStore,
    config: &Config,
    projects: &mut OtherProject,
    prs: &mut ExitPr,
    launcher: &mut impl SupervisorLauncher,
) -> Result<luthor::supervisor::LaunchPlan, luthor::coordinator::DispatchError> {
    luthor::coordinator::retry_one(
        store,
        luthor::coordinator::RetryDependencies {
            task_id: "task",
            previous_attempt_id: "attempt-real",
            attempt_id: "attempt-retry",
            config,
            config_revision: "corrected",
            actor: "operator",
            reason: "revalidate native startup rejection and correct worker budget",
            revalidate_terminal_exit: true,
            projects,
            prs,
            launcher,
        },
    )
}

#[cfg(unix)]
pub(crate) fn old_retry_rows(config: &Config) -> Vec<String> {
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    ["SELECT json_array(id,task_id,lifecycle,outcome) FROM attempts WHERE id='attempt-real'",
     "SELECT json_array(attempt_id,task_id,status) FROM reservations WHERE attempt_id='attempt-real'",
     "SELECT json_array(id,task_id,attempt_id,kind,detail) FROM intents WHERE attempt_id='attempt-real' OR attempt_id IS NULL ORDER BY rowid",
     "SELECT json_array(task_id,attempt_id,kind,payload) FROM evidence WHERE attempt_id='attempt-real' OR attempt_id IS NULL ORDER BY rowid"]
    .into_iter().flat_map(|sql| db.prepare(sql).unwrap().query_map([], |row| row.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap()).collect()
}

fn reconcile_fixture_exit(config: &Config, store: &mut StateStore) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !receipt_path(config).exists() {
        assert!(
            Instant::now() < deadline,
            "retry fixture receipt missing: {:?}",
            fs::read_to_string(
                config
                    .state_root
                    .join("attempts/attempt-real.supervisor-error.json")
            )
        );
        thread::sleep(Duration::from_millis(20));
    }
    let selection = store.selection_evidence("task").unwrap().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let result = luthor::coordinator::reconcile_with_pr(
            store,
            "task",
            "attempt-real",
            &mut OtherProject(selection.candidate.clone(), 1),
            &mut ExitPr::default(),
        )
        .unwrap();
        if store.task_phase("task").unwrap().as_deref() == Some("attention") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fixture exit not reconciled: {result:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
