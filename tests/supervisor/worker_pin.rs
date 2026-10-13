//! Issue 53 behavioral coverage through durable plans and the real worker gate.
use super::*;
use luthor::{
    state::{journal, launches, scheduling, task_records},
    worker_build::WorkerBuild,
};
use serde_json::{Value, json};

#[test]
fn worker_pin_survives_restart_and_resume_ignores_replacement_template() {
    let (_dir, config, store, first) = paused_fixture();
    let build = first.worker_build.clone().unwrap();
    assert_eq!(
        build,
        WorkerBuild::capture(&first.executable, &first.worktree).unwrap()
    );
    let replacement = config
        .worktree_root
        .parent()
        .unwrap()
        .join("upgraded-worker");
    fs::write(&replacement, "#!/bin/sh\nexit 99\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
    let mut selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    selection.effective_config.resume.executable = replacement;
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE kind='selection'",
        [serde_json::to_string(&selection).unwrap()],
    )
    .unwrap();
    drop(store);
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    let next = prepare_resume(&mut store, "task", "pinned-resume").unwrap();
    assert_eq!(next.executable, build.path);
    assert_eq!(next.worker_build, Some(build.clone()));
    assert_eq!(next.session_id, first.session_id);
    assert_views(&config, &build);
}

fn assert_views(config: &Config, build: &WorkerBuild) {
    for args in [vec!["status".into()], vec!["show".into(), "task".into()]] {
        let output: Value =
            serde_json::from_str(&luthor::cli::execute(&config.state_root, &args).unwrap())
                .unwrap();
        let summary = if args[0] == "status" {
            &output["tasks"][0]
        } else {
            &output
        };
        assert_eq!(summary["worker_build"]["path"], json!(build.path));
        assert_eq!(summary["worker_build"]["sha256"], build.sha256);
        assert_eq!(summary["worker_build"]["pin_status"], "pinned");
    }
}

#[test]
fn worker_pin_missing_replaced_and_legacy_builds_hold_before_new_attempt() {
    for scenario in ["missing", "replaced", "legacy"] {
        let (_dir, config, mut store, first) = paused_fixture();
        let saved = launches::launch_intent(&store, "attempt-real")
            .unwrap()
            .unwrap();
        match scenario {
            "missing" => fs::remove_file(&first.executable).unwrap(),
            "replaced" => fs::write(&first.executable, "#!/bin/sh\nexit 99\n").unwrap(),
            "legacy" => {
                let mut legacy: Value = serde_json::from_str(&saved).unwrap();
                legacy.as_object_mut().unwrap().remove("worker_build");
                let db =
                    rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
                db.execute(
                    "UPDATE intents SET detail=?1 WHERE kind='launch'",
                    [legacy.to_string()],
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            matches!(
                prepare_resume(&mut store, "task", "blocked"),
                Err(SupervisorError::WorkerBuildHeld(_))
            ),
            "{scenario}"
        );
        assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
        assert!(
            launches::launch_intent(&store, "blocked")
                .unwrap()
                .is_none()
        );
        let reason = task_records::held_reason(&store, "task").unwrap().unwrap();
        assert!(
            reason.contains("template") && reason.contains("migrate"),
            "{reason}"
        );
        assert_eq!(
            task_records::task_phase(&store, "task").unwrap().as_deref(),
            Some("held")
        );
    }
}

#[test]
fn worker_pin_replaced_after_planning_never_executes_new_build() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    fs::write(
        &plan.executable,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    let owner = luthor::WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    assert!(matches!(
        execute_with_binary(
            &mut store,
            &plan,
            Path::new(env!("CARGO_BIN_EXE_luthor")),
            &owner
        ),
        Err(SupervisorError::WorkerBuildHeld(_))
    ));
    assert!(!marker.exists());
    assert!(
        !config
            .state_root
            .join("attempts/attempt-real.plan.json")
            .exists()
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}

#[test]
fn worker_pin_session_incompatibility_holds_without_pr_read_or_retry_and_survives_restart() {
    for diagnostic in [
        r#"{"error":{"code":"turn","message":"corrupt session state: unsupported state version 3"}}"#,
        r#"{"error":{"code":"session_incompatible","message":"cannot read this session"}}"#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (config, mut store, plan, _) = prepared_fake_worker(&dir);
        fs::write(
            plan.executable.with_extension("behavior"),
            format!("#!/bin/sh\nprintf '%s\\n' '{diagnostic}'\nexit 2\n"),
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
        worker_exit::await_dispatched_worker_exit(&config, "task", "attempt-real");
        let mut projects = OtherProject(
            task_records::selection_evidence(&store, "task")
                .unwrap()
                .unwrap()
                .candidate,
            1,
        );
        let mut prs = ExitPr::default();
        let result = luthor::coordinator::reconcile_with_pr(
            &mut store,
            "task",
            "attempt-real",
            &mut projects,
            &mut prs,
        )
        .unwrap();
        assert!(
            matches!(result, Reconciliation::Held { reason } if reason.contains("session incompatible") && reason.contains("pinned worker") && reason.contains("attempted worker") && reason.contains(&plan.worker_build.as_ref().unwrap().sha256))
        );
        assert_eq!(prs.reads, 0);
        assert_eq!(projects.1, 1);
        assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
        let evidence =
            journal::evidence_payloads(&store, "task", "attempt-real", "session_incompatible")
                .unwrap();
        assert_eq!(evidence.len(), 1);
        let proof: Value = serde_json::from_str(&evidence[0]).unwrap();
        assert_eq!(proof["class"], "session_incompatible");
        if diagnostic.contains("unsupported") {
            assert_eq!(proof["reported_state_version"], "3");
        }
        drop(store);
        store = StateStore::open(&config.state_root, 1).unwrap();
        assert!(matches!(
            reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
            Reconciliation::Held { .. }
        ));
        assert!(luthor::state::retry_context_for_task(&store, "task", "attempt-real").is_err());
        assert!(prepare_resume(&mut store, "task", "no-retry").is_err());
        assert_eq!(
            journal::evidence_payloads(&store, "task", "attempt-real", "session_incompatible")
                .unwrap(),
            evidence
        );
    }
}

#[test]
fn worker_pin_records_worker_reported_session_format_without_inference() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    fs::write(
        plan.executable.with_extension("behavior"),
        "#!/bin/sh\nprintf '%s\\n' '{\"session_state_version\":3}'\nexit 0\n",
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
    worker_exit::await_dispatched_worker_exit(&config, "task", "attempt-real");
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed { .. }
    ));
    let evidence =
        journal::evidence_payloads(&store, "task", "attempt-real", "session_state_format").unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(
        serde_json::from_str::<Value>(&evidence[0]).unwrap()["version"],
        3
    );
    let output: Value = serde_json::from_str(
        &luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap(),
    )
    .unwrap();
    assert_eq!(output["worker_build"]["reported_session_state_version"], 3);
    reconcile_attempt(&mut store, "task", "attempt-real").unwrap();
    assert_eq!(
        journal::evidence_payloads(&store, "task", "attempt-real", "session_state_format").unwrap(),
        evidence
    );
}

#[test]
fn worker_pin_gate_rechecks_after_build_changed_since_plan_selection() {
    let dir = tempfile::tempdir().unwrap();
    let plan = fake_plan(dir.path(), "gate-build-change", 0);
    let marker = dir.path().join("replacement-started");
    fs::write(
        &plan.executable,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let owner = luthor::WorktreeOwner::acquire(dir.path(), "task").unwrap();
    let status = run_gated_child_with_binary(
        &plan,
        Cursor::new(b"R"),
        dir.path(),
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    assert_eq!(status.code(), Some(1));
    let stderr = fs::read_to_string(dir.path().join("gate-build-change.stderr.log")).unwrap();
    assert!(
        stderr.contains("worker build held")
            && stderr.contains(&plan.worker_build.as_ref().unwrap().sha256),
        "{stderr}"
    );
    assert!(!marker.exists());
}
