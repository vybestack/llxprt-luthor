use super::*;
use crate::pin_adoption_fixture as fixture;
use luthor::state::{
    launches,
    pin_adoption::{self, PinAdoptionRequest},
    scheduling, task_records,
};
use serde_json::Value;

#[test]
fn pin_adoption_preview_is_read_only_and_resume_uses_durable_audit() {
    let (_dir, config, mut store, _) = paused_fixture();
    let build = fixture::legacy(&config, &store);
    let history = fixture::rows(&config);
    let before = view(&config);
    let preview =
        pin_adoption::preview(&config.state_root, "task", fixture::request(&build)).unwrap();
    assert_eq!(preview.worker_build, build);
    assert_eq!(view(&config), before);
    assert_eq!(fixture::rows(&config), history);
    fixture::adopt(&mut store, &build);
    let shown = view(&config);
    assert_eq!(shown["worker_build"]["pin_status"], "pinned");
    assert_eq!(shown["worker_build"]["sha256"], build.sha256);
    let event = shown["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "worker_pin_adopted")
        .unwrap();
    assert_eq!(event["detail"]["actor"], "operator");
    assert_eq!(
        event["detail"]["reason"],
        "unchanged executable for the task lifetime"
    );
    drop(store);
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    let next = prepare_resume(&mut store, "task", "adopted-resume").unwrap();
    assert_eq!(next.worker_build, Some(build.clone()));
    assert_eq!(next.executable, build.path);
    assert_eq!(next.session_id, "task");
}

fn view(config: &Config) -> Value {
    serde_json::from_str(
        &luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap(),
    )
    .unwrap()
}

#[test]
fn pin_adoption_refuses_wrong_hash_unsaved_path_and_repins_without_state_changes() {
    let (_dir, config, mut store, _) = paused_fixture();
    let build = fixture::legacy(&config, &store);
    let wrong_path = build.path.with_extension("alias");
    std::os::unix::fs::symlink(&build.path, &wrong_path).unwrap();
    for (path, hash, reason) in [
        (build.path.as_path(), "0".repeat(64), "SHA-256"),
        (build.path.as_path(), "not-a-hash".into(), "SHA-256"),
        (wrong_path.as_path(), build.sha256.clone(), "snapshot"),
    ] {
        let before = view(&config);
        let request = || PinAdoptionRequest {
            executable: path,
            sha256: &hash,
            actor: "operator",
            reason: "verified",
        };
        assert!(
            pin_adoption::preview(&config.state_root, "task", request())
                .unwrap_err()
                .to_string()
                .contains(reason)
        );
        assert!(
            pin_adoption::adopt(&mut store, "task", request())
                .unwrap_err()
                .to_string()
                .contains(reason)
        );
        assert_eq!(view(&config), before);
    }
    fixture::adopt(&mut store, &build);
    let before = view(&config);
    assert!(
        pin_adoption::adopt(&mut store, "task", fixture::request(&build))
            .unwrap_err()
            .to_string()
            .contains("repinning")
    );
    assert_eq!(view(&config), before);
}

#[test]
fn pin_adoption_refuses_recorded_pin_and_unfinished_running_attempt() {
    let (_dir, config, mut store, first) = paused_fixture();
    let build = first.worker_build.unwrap();
    let before = view(&config);
    assert!(
        pin_adoption::adopt(&mut store, "task", fixture::request(&build))
            .unwrap_err()
            .to_string()
            .contains("repinning")
    );
    assert_eq!(view(&config), before);
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    let build = fixture::legacy(&config, &store);
    let old = launches::launch_intent(&store, &plan.attempt_id).unwrap();
    assert!(
        pin_adoption::adopt(&mut store, "task", fixture::request(&build))
            .unwrap_err()
            .to_string()
            .contains("running")
    );
    assert_eq!(
        launches::launch_intent(&store, &plan.attempt_id).unwrap(),
        old
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        !task_records::selection_evidence(&store, "task")
            .unwrap()
            .is_none()
    );
    assert!(
        !luthor::state::journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"worker_pin_adopted".into())
    );
}

#[test]
fn pin_adoption_refuses_live_owner_even_with_completed_database_accounting() {
    let (_dir, config, mut store, _) = paused_fixture();
    let build = fixture::legacy(&config, &store);
    let owner = luthor::WorktreeOwner::acquire_existing(store.root(), "task").unwrap();
    let before = view(&config);
    assert!(
        pin_adoption::adopt(&mut store, "task", fixture::request(&build))
            .unwrap_err()
            .to_string()
            .contains("owner")
    );
    assert_eq!(view(&config), before);
    drop(owner);
    fixture::adopt(&mut store, &build);
}

#[test]
fn pin_adoption_accepts_saved_resume_executable_but_preserves_unrelated_hold() {
    let (_dir, config, mut store, first) = paused_fixture();
    fixture::legacy(&config, &store);
    let replacement = first.executable.with_extension("resume");
    fs::copy(&first.executable, &replacement).unwrap();
    let build = luthor::worker_build::WorkerBuild::capture(&replacement, &first.worktree).unwrap();
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let mut selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    selection.effective_config.resume.executable = replacement;
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE kind='selection'",
        [serde_json::to_string(&selection).unwrap()],
    )
    .unwrap();
    task_records::hold_task(&mut store, "task", "session incompatible; retries blocked").unwrap();
    fixture::adopt(&mut store, &build);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert!(prepare_resume(&mut store, "task", "still-blocked").is_err());
}

#[test]
fn pin_adoption_cli_outputs_one_document_and_rechecks_preview_on_execute() {
    let (_dir, config, store, _) = paused_fixture();
    let build = fixture::legacy(&config, &store);
    let path = config.worktree_root.parent().unwrap().join("config.json");
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let args = vec![
        "adopt-pin".to_owned(),
        "task".into(),
        "--config".into(),
        path.to_str().unwrap().into(),
        "--executable".into(),
        build.path.to_str().unwrap().into(),
        "--sha256".into(),
        build.sha256.clone(),
        "--actor".into(),
        "operator".into(),
        "--reason".into(),
        "operator verified lifetime build".into(),
    ];
    let before = view(&config);
    let history = fixture::rows(&config);
    drop(store);
    let preview = cli(&args, false);
    assert_eq!(preview["status"], "preview");
    assert_eq!(preview["adoption"]["worker_build"]["sha256"], build.sha256);
    assert_eq!(view(&config), before);
    let mut execute = args.clone();
    execute.push("--execute".into());
    let bytes = fs::read(&build.path).unwrap();
    fs::write(&build.path, "#!/bin/sh\nexit 99\n").unwrap();
    let refused = cli(&execute, true);
    assert!(refused["reason"].as_str().unwrap().contains("SHA-256"));
    assert_eq!(view(&config), before);
    fs::write(&build.path, bytes).unwrap();
    let adopted = cli(&execute, false);
    assert_eq!(adopted["status"], "adopted");
    assert_eq!(adopted["pin_status"], "pinned");
    assert_eq!(fixture::rows(&config), history);
    assert_eq!(cli(&execute, true)["status"], "refused");
    assert_eq!(cli(&["adopt-pin".into()], true)["status"], "refused");
}

fn cli(args: &[String], failure: bool) -> Value {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        !output.status.success(),
        failure,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn pin_adoption_refuses_a_live_worker_without_touching_its_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    let marker = dir.path().join("live-worker");
    fs::write(
        plan.executable.with_extension("behavior"),
        format!("#!/bin/sh\ntouch '{}'\nsleep 2\n", marker.display()),
    )
    .unwrap();
    let owner = luthor::WorktreeOwner::acquire(store.root(), "task").unwrap();
    execute_with_binary(
        &mut store,
        &plan,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "worker did not start");
        thread::sleep(Duration::from_millis(10));
    }
    let build = fixture::legacy(&config, &store);
    let history = fixture::rows(&config);
    assert!(
        pin_adoption::adopt(&mut store, "task", fixture::request(&build))
            .unwrap_err()
            .to_string()
            .contains("running")
    );
    assert_eq!(fixture::rows(&config), history);
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    // Restore the fixture's actual launch provenance before accounting its exit.
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute(
        "UPDATE intents SET detail=?1 WHERE kind IN ('launch','supervisor_dispatch')",
        [serde_json::to_string(&plan).unwrap()],
    )
    .unwrap();
    fs::write(
        config.state_root.join("attempts/attempt-real.plan.json"),
        serde_json::to_vec(&plan).unwrap(),
    )
    .unwrap();
    crate::worker_exit::await_dispatched_worker_exit(&config, "task", "attempt-real");
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed { .. }
    ));
}

#[test]
fn pin_adoption_keeps_hash_checks_after_adoption_and_refuses_missing_evidence() {
    let (_dir, config, mut store, _) = paused_fixture();
    let build = fixture::legacy(&config, &store);
    fixture::adopt(&mut store, &build);
    let next = prepare_resume(&mut store, "task", "adopted-gate").unwrap();
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let audit: String = db
        .query_row(
            "SELECT payload FROM evidence WHERE kind='worker_pin_adopted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute("DELETE FROM evidence WHERE kind='worker_pin_adopted'", [])
        .unwrap();
    let owner = luthor::WorktreeOwner::acquire_existing(store.root(), "task").unwrap();
    assert!(
        execute_with_binary(
            &mut store,
            &next,
            Path::new(env!("CARGO_BIN_EXE_luthor")),
            &owner
        )
        .is_err()
    );
    db.execute(
        "INSERT INTO evidence(task_id,kind,payload) VALUES('task','worker_pin_adopted',?1)",
        [audit],
    )
    .unwrap();
    fs::write(&build.path, "#!/bin/sh\nexit 99\n").unwrap();
    assert!(matches!(
        execute_with_binary(
            &mut store,
            &next,
            Path::new(env!("CARGO_BIN_EXE_luthor")),
            &owner
        ),
        Err(SupervisorError::WorkerBuildHeld(_))
    ));
    assert!(
        !config
            .state_root
            .join("attempts/adopted-gate.plan.json")
            .exists()
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}
