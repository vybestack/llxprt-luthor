use super::*;
use luthor::state::{journal, launches, scheduling, task_records};

#[cfg(unix)]
pub(crate) fn live_worker_log_write_failure_stops_and_holds_both_streams() {
    use std::os::unix::fs::PermissionsExt;
    use std::{io::Write, time::Instant};
    for stream in ["stdout", "stderr"] {
        let dir = tempfile::tempdir().unwrap();
        let (config, candidate) = configured(dir.path());
        let mut store = StateStore::open(&config.state_root, 1).unwrap();
        claimed(&mut store, &config, &candidate, dir.path());
        let worker = dir.path().join("worker-that-must-not-run");
        fs::write(
            &worker,
            "#!/bin/sh\nwhile :; do printf 'out\\n'; printf 'err\\n' >&2; done\n",
        )
        .unwrap();
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
        let plan = prepare_initial(&mut store, "task", "attempt-1").unwrap();
        launches::begin_supervision(
            &mut store,
            "task",
            "attempt-1",
            &serde_json::to_string(&plan).unwrap(),
        )
        .unwrap();
        let attempts = config.state_root.join("attempts");
        fs::create_dir(&attempts).unwrap();
        fs::set_permissions(&attempts, fs::Permissions::from_mode(0o700)).unwrap();
        let _guard = FixtureGroupGuard(attempts.join("attempt-1.child.json"));
        let started = Instant::now();
        let result = run_gated_child_with_log_writers(
            &plan,
            Cursor::new(b"R"),
            &attempts,
            Path::new(env!("CARGO_BIN_EXE_luthor")),
            |out, err| {
                if stream == "stdout" {
                    (
                        Box::new(BrokenLog(out)) as Box<dyn Write + Send>,
                        Box::new(err) as Box<dyn Write + Send>,
                    )
                } else {
                    (
                        Box::new(out) as Box<dyn Write + Send>,
                        Box::new(BrokenLog(err)) as Box<dyn Write + Send>,
                    )
                }
            },
        );
        assert!(
            matches!(result, Err(SupervisorError::ExecutionUnavailable)),
            "{stream}: {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{stream}: stop took too long"
        );
        assert_log_failure_held(&config, &mut store, &attempts, stream);
    }
}

#[cfg(unix)]
pub(crate) fn log_writer_and_evidence_failure_still_stops_registered_child_group() {
    use std::{io::Write, os::unix::fs::PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    let worker = dir.path().join("worker-that-must-not-run");
    fs::write(
        &worker,
        "#!/bin/sh\nprintf 'trigger\\n'\nwhile :; do :; done\n",
    )
    .unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    let plan = prepare_initial(&mut store, "task", "attempt-fault").unwrap();
    launches::begin_supervision(
        &mut store,
        "task",
        "attempt-fault",
        &serde_json::to_string(&plan).unwrap(),
    )
    .unwrap();
    let attempts = config.state_root.join("attempts");
    fs::create_dir(&attempts).unwrap();
    fs::set_permissions(&attempts, fs::Permissions::from_mode(0o700)).unwrap();
    let _guard = FixtureGroupGuard(attempts.join("attempt-fault.child.json"));
    rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap().execute_batch(
        "CREATE TRIGGER reject_log_failure BEFORE INSERT ON evidence WHEN NEW.kind='log_failure' BEGIN SELECT RAISE(ABORT, 'injected evidence failure'); END;"
    ).unwrap();
    let result = run_gated_child_with_log_writers(
        &plan,
        Cursor::new(b"R"),
        &attempts,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        |out, err| {
            (
                Box::new(BrokenLog(out)) as Box<dyn Write + Send>,
                Box::new(err) as Box<dyn Write + Send>,
            )
        },
    );
    assert!(matches!(result, Err(SupervisorError::Sql(_))), "{result:?}");
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        journal::stop_intent(&store, "task", "attempt-fault")
            .unwrap()
            .is_none()
    );
    assert!(!attempts.join("attempt-fault.receipt.json").exists());
    let child: serde_json::Value =
        serde_json::from_slice(&fs::read(attempts.join("attempt-fault.child.json")).unwrap())
            .unwrap();
    let pgid = child["pid"].as_i64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(-pgid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[cfg(unix)]
fn assert_log_failure_held(config: &Config, store: &mut StateStore, attempts: &Path, stream: &str) {
    let failure: String = rusqlite::Connection::open(config.state_root.join("state.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT payload FROM evidence WHERE task_id='task' AND attempt_id='attempt-1' AND kind='log_failure'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    let failure: serde_json::Value = serde_json::from_str(&failure).unwrap();
    assert_eq!(failure["stream"], stream);
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .contains("injected log write failure")
    );
    assert!(
        journal::stop_intent(store, "task", "attempt-1")
            .unwrap()
            .is_some()
    );
    assert_eq!(scheduling::reservation_count(store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert!(!attempts.join("attempt-1.receipt.json").exists());
    let child: serde_json::Value =
        serde_json::from_slice(&fs::read(attempts.join("attempt-1.child.json")).unwrap()).unwrap();
    let pgid = child["pid"].as_i64().unwrap() as i32;
    journal::record_evidence(
        store,
        "task",
        Some("attempt-1"),
        "child_registered",
        &child.to_string(),
    )
    .unwrap();
    assert!(matches!(
        reconcile_attempt(store, "task", "attempt-1").unwrap(),
        Reconciliation::Held { reason } if reason == "log drain failed"
    ));
    assert_eq!(scheduling::reservation_count(store).unwrap(), 1);
    assert_eq!(unsafe { libc::kill(-pgid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
