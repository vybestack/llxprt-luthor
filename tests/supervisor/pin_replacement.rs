//! Issue 97: explicit, compatible repins preserve history and launch safety.
use super::*;
use crate::pin_replacement_fixture as fixture;
use luthor::state::{journal, launches, pin_replacement, scheduling, task_records};
use serde_json::json;

fn paused() -> (
    tempfile::TempDir,
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
) {
    let (dir, config, store, first) = paused_fixture();
    let ready =
        journal::evidence_payloads(&store, "task", "attempt-real", "supervisor_ready").unwrap();
    let ready: serde_json::Value = serde_json::from_str(&ready[0]).unwrap();
    wait_for_process_and_group_absence(ready["pid"].as_i64().unwrap() as i32);
    (dir, config, store, first)
}

#[test]
fn repin_preview_preserves_history_execute_audits_and_resume_uses_new_build() {
    let (_dir, mut config, mut store, first) = paused();
    let build = fixture::replacement(&mut config, &mut store);
    let history = crate::pin_adoption_fixture::rows(&config);
    let before = fixture::view(&config);
    let saved_plan = fs::read(config.state_root.join("attempts/attempt-real.plan.json")).unwrap();
    let saved_receipt =
        fs::read(config.state_root.join("attempts/attempt-real.receipt.json")).unwrap();
    let preview = pin_replacement::preview(&config, "task", fixture::request(&build)).unwrap();
    assert_eq!(
        preview.previous_worker_build,
        first.worker_build.clone().unwrap()
    );
    assert_eq!(preview.worker_build, build);
    assert_eq!(fixture::view(&config), before);
    let audit =
        pin_replacement::replace(&mut store, &config, "task", fixture::request(&build)).unwrap();
    let evidence = fixture::view(&config);
    let event = evidence["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "worker_pin_replaced")
        .unwrap();
    assert_eq!(event["detail"], json!(audit));
    assert_eq!(audit.actor, "operator");
    assert_eq!(audit.reason, fixture::request(&build).reason);
    assert_eq!(audit.executable, config.resume.executable);
    assert_eq!(audit.historical_attempts, ["attempt-real"]);
    assert_eq!(audit.session_state_version, 3);
    assert_eq!(audit.capability.session_state_versions, [2, 3]);
    assert!(audit.observed_at_unix_secs > 0);
    assert_eq!(evidence["worker_build"]["sha256"], build.sha256);
    assert_eq!(
        evidence["worker_build"]["reported_session_state_version"],
        3
    );
    assert_eq!(crate::pin_adoption_fixture::rows(&config), history);
    assert_eq!(
        fs::read(config.state_root.join("attempts/attempt-real.plan.json")).unwrap(),
        saved_plan
    );
    assert_eq!(
        fs::read(config.state_root.join("attempts/attempt-real.receipt.json")).unwrap(),
        saved_receipt
    );
    fs::remove_file(&first.executable).unwrap();
    drop(store);
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    let next = prepare_resume(&mut store, "task", "repinned-resume").unwrap();
    assert_eq!(next.worker_build, Some(build.clone()));
    assert_eq!(next.executable, build.path);
    assert_eq!(next.session_id, first.session_id);
    assert_eq!(next.worktree, first.worktree);
    let owner = luthor::WorktreeOwner::acquire_existing(store.root(), "task").unwrap();
    execute_with_binary(
        &mut store,
        &next,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    crate::worker_exit::await_dispatched_worker_exit(&config, "task", &next.attempt_id);
    assert!(first.worktree.join("replacement-ran").exists());
    assert!(matches!(
        reconcile_attempt(&mut store, "task", &next.attempt_id).unwrap(),
        Reconciliation::Completed {
            exit_code: Some(2),
            ..
        }
    ));
}

#[test]
fn repin_refuses_running_receiptless_and_owner_busy_tasks_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let (mut config, mut store, _, _) = prepared_fake_worker(&dir);
    let build = fixture::replacement(&mut config, &mut store);
    assert_refused(&config, &mut store, &build, "running");
    let (_dir, mut config, mut store, _) = paused();
    let build = fixture::replacement(&mut config, &mut store);
    let owner = luthor::WorktreeOwner::acquire_existing(store.root(), "task").unwrap();
    assert_refused(&config, &mut store, &build, "owner");
    drop(owner);
}

fn assert_refused(
    config: &Config,
    store: &mut StateStore,
    build: &luthor::worker_build::WorkerBuild,
    reason: &str,
) {
    let before = fixture::view(config);
    let rows = crate::pin_adoption_fixture::rows(config);
    for error in [
        pin_replacement::preview(config, "task", fixture::request(build)).unwrap_err(),
        pin_replacement::replace(store, config, "task", fixture::request(build)).unwrap_err(),
    ] {
        assert!(error.to_string().contains(reason), "{error}");
    }
    assert_eq!(fixture::view(config), before);
    assert_eq!(crate::pin_adoption_fixture::rows(config), rows);
}

#[test]
fn repin_refuses_registered_process_even_after_completed_database_accounting() {
    let (_dir, mut config, mut store, _) = paused();
    let build = fixture::replacement(&mut config, &mut store);
    for kind in ["supervisor_ready", "child_registered", "tracked_descendant"] {
        let db = fixture::db(&config);
        db.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task','attempt-real',?1,?2)",
            rusqlite::params![kind, json!({"pid":std::process::id(), "group_id":std::process::id(), "boot_identity":"boot", "start_identity":"start"}).to_string()]).unwrap();
        assert_refused(&config, &mut store, &build, "registered process");
        db.execute(
            "DELETE FROM evidence WHERE sequence=(SELECT MAX(sequence) FROM evidence)",
            [],
        )
        .unwrap();
    }
}

#[test]
fn repin_refuses_wrong_hash_alias_old_template_and_unattested_compatibility() {
    let (_dir, mut config, mut store, first) = paused();
    let build = fixture::replacement(&mut config, &mut store);
    let mut wrong = build.clone();
    wrong.sha256 = "0".repeat(64);
    assert_refused(&config, &mut store, &wrong, "SHA-256");
    wrong.sha256 = "not-a-hash".into();
    assert_refused(&config, &mut store, &wrong, "SHA-256");
    wrong = build.clone();
    wrong.path = build.path.with_extension("alias");
    std::os::unix::fs::symlink(&build.path, &wrong.path).unwrap();
    assert_refused(&config, &mut store, &wrong, "current config");
    wrong.path = build
        .path
        .parent()
        .unwrap()
        .join(".")
        .join(build.path.file_name().unwrap());
    assert_refused(&config, &mut store, &wrong, "current config");
    config.initial.executable = build.path.clone();
    assert_refused(
        &config,
        &mut store,
        first.worker_build.as_ref().unwrap(),
        "current config",
    );
    for probe in [
        "exit 7",
        "printf '{}'",
        "printf '{\"session_state_versions\":[4]}'",
        "printf 'help text'",
        "printf '{\"session_state_versions\":[3]} extra'",
    ] {
        let build = fixture::build(&mut config, &build.path, probe);
        // Every refusal leaves the original pin and all history untouched.
        let before = fixture::view(&config);
        assert!(pin_replacement::preview(&config, "task", fixture::request(&build)).is_err());
        assert!(
            pin_replacement::replace(&mut store, &config, "task", fixture::request(&build))
                .is_err()
        );
        assert_eq!(fixture::view(&config), before);
    }
    let build = fixture::build(
        &mut config,
        &build.path,
        "printf '{\"session_state_versions\":[3]}'",
    );
    fixture::db(&config)
        .execute("DELETE FROM evidence WHERE kind='session_state_format'", [])
        .unwrap();
    assert_refused(&config, &mut store, &build, "unrecorded");
}

#[test]
fn repin_cli_preview_execute_rechecks_and_returns_one_json_document() {
    let (_dir, mut config, mut store, _) = paused();
    let build = fixture::replacement(&mut config, &mut store);
    let capabilities = build.path.with_extension("capability");
    fs::write(&capabilities, r#"{"session_state_versions":[3]}"#).unwrap();
    let build = fixture::build(&mut config, &build.path, "cat \"$0.capability\"");
    let args = fixture::args(&config, &build);
    let before = fixture::view(&config);
    drop(store);
    assert_eq!(fixture::cli(&args, false)["status"], "preview");
    assert_eq!(fixture::view(&config), before);
    let mut execute = args;
    execute.push("--execute".into());
    let bytes = fs::read(&build.path).unwrap();
    fs::write(&build.path, "#!/bin/sh\nexit 99\n").unwrap();
    assert!(
        fixture::cli(&execute, true)["reason"]
            .as_str()
            .unwrap()
            .contains("SHA-256")
    );
    assert_eq!(fixture::view(&config), before);
    fs::write(&build.path, bytes).unwrap();
    fs::write(&capabilities, r#"{"session_state_versions":[4]}"#).unwrap();
    assert!(
        fixture::cli(&execute, true)["reason"]
            .as_str()
            .unwrap()
            .contains("did not attest")
    );
    assert_eq!(fixture::view(&config), before);
    fs::write(&capabilities, r#"{"session_state_versions":[3]}"#).unwrap();
    assert_eq!(fixture::cli(&execute, false)["status"], "replaced");
    assert_eq!(fixture::cli(&execute, true)["status"], "refused");
    assert_eq!(fixture::cli(&["repin".into()], true)["status"], "refused");
}

#[test]
fn repin_preserves_unrelated_holds_and_refuses_legacy_unrecorded_sessions() {
    let (_dir, mut config, mut store, _) = paused();
    let old = crate::pin_adoption_fixture::legacy(&config, &store);
    let build = fixture::replacement(&mut config, &mut store);
    assert_refused(&config, &mut store, &build, "legacy_unrecorded");
    crate::pin_adoption_fixture::adopt(&mut store, &old);
    task_records::hold_task(&mut store, "task", "unrelated safety hold").unwrap();
    pin_replacement::replace(&mut store, &config, "task", fixture::request(&build)).unwrap();
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert!(prepare_resume(&mut store, "task", "still-held").is_err());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert!(
        launches::launch_intent(&store, "still-held")
            .unwrap()
            .is_none()
    );
}
