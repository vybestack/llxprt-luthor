use super::*;
use luthor::state::{journal, scheduling, task_records};

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) struct StopRaceCleanup(pub(crate) std::path::PathBuf);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for StopRaceCleanup {
    fn drop(&mut self) {
        drop(FixtureGroupGuard(
            self.0.join("attempts/attempt-real.child.json"),
        ));
        let Ok(db) = rusqlite::Connection::open_with_flags(
            self.0.join("state.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) else {
            return;
        };
        let Ok(payload) = db.query_row(
            "SELECT payload FROM evidence WHERE kind='supervisor_ready' AND attempt_id='attempt-real'",
            [],
            |row| row.get::<_, String>(0),
        ) else {
            return;
        };
        let Ok(process) = serde_json::from_str::<serde_json::Value>(&payload) else {
            return;
        };
        let Some(pid) = process["pid"]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
        else {
            return;
        };
        let expected = (
            process["boot_identity"].as_str().unwrap_or_default().into(),
            process["start_identity"]
                .as_str()
                .unwrap_or_default()
                .into(),
        );
        for _ in 0..200 {
            if observed_process_identity(pid).as_ref() != Some(&expected) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if observed_process_identity(pid).as_ref() == Some(&expected) {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn natural_stop_race_fixture() -> (
    tempfile::TempDir,
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
    StopRaceCleanup,
) {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    let cleanup = StopRaceCleanup(config.state_root.clone());
    let release = std::env::current_dir()
        .unwrap()
        .join(&config.state_root)
        .join("release-natural-exit");
    fs::write(
        &plan.executable,
        format!(
            "#!/bin/sh
echo started > '{}'
while [ ! -f '{}' ]; do :; done
exit 7
",
            marker.display(),
            release.display(),
        ),
    )
    .unwrap();
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    for _ in 0..200 {
        if marker.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        marker.exists(),
        "natural-exit worker did not reach release gate"
    );
    assert!(!receipt_path(&config).exists());
    assert!(
        journal::stop_intent(&store, "task", "attempt-real")
            .unwrap()
            .is_none()
    );
    let child: serde_json::Value = serde_json::from_slice(
        &fs::read(config.state_root.join("attempts/attempt-real.child.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        unsafe { libc::kill(-(child["pid"].as_i64().unwrap() as i32), 0) },
        0
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(matches!(
        scheduling::ensure_dispatch_capacity(&store),
        Err(StateError::Capacity { .. })
    ));
    (dir, config, store, plan, cleanup)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn release_natural_exit(config: &Config) -> Vec<u8> {
    fs::write(config.state_root.join("release-natural-exit"), b"exit now").unwrap();
    let receipt = stopped_receipt(config);
    assert_eq!(receipt.exit_code, Some(7));
    assert_eq!(receipt.signal, None);
    assert!(receipt.stop_signals.is_empty());
    assert_eq!(unsafe { libc::kill(-(receipt.child_pid as i32), 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    fs::read(receipt_path(config)).unwrap()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn assert_natural_stop_accounted(
    config: &Config,
    mut store: StateStore,
    plan: &luthor::supervisor::LaunchPlan,
    receipt: &[u8],
) {
    assert_eq!(fs::read(receipt_path(config)).unwrap(), receipt);
    assert!(
        journal::stop_intent(&store, "task", "attempt-real")
            .unwrap()
            .is_some()
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(matches!(
        scheduling::ensure_dispatch_capacity(&store),
        Err(StateError::Capacity { .. })
    ));
    let mut prs = ExitPr::default();
    let mut projects = OtherProject(
        task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    assert!(matches!(
        luthor::coordinator::reconcile_with_pr(
            &mut store,
            "task",
            "attempt-real",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        Reconciliation::Completed {
            exit_code: Some(7),
            signal: None
        }
    ));
    assert_eq!(prs.reads, 1);
    assert_eq!(exit_proof(config).status, PausePrStatus::Absent);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("attention")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    scheduling::ensure_dispatch_capacity(&store).unwrap();
    assert!(matches!(
        prepare_resume(&mut store, "task", "attempt-next"),
        Err(SupervisorError::State(StateError::LaunchBlocked))
    ));
    drop(store);

    assert_natural_stop_views_and_restart(config, plan, receipt, &mut prs);
}
