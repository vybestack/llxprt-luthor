#![cfg(unix)]
use super::*;
use luthor::{
    WorktreeOwner,
    coordinator::{RecoveryResult, RetryDependencies, retry_one},
    state::{launches, scheduling, task_records},
};
use std::{
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::PathBuf,
};

struct BuiltLauncher;
impl SupervisorLauncher for BuiltLauncher {
    fn launch(
        &mut self,
        store: &mut StateStore,
        plan: &luthor::supervisor::LaunchPlan,
        owner: &WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        execute_with_binary(store, plan, Path::new(env!("CARGO_BIN_EXE_luthor")), owner)
    }
}

struct Aborted {
    _dir: tempfile::TempDir,
    config: Config,
    store: StateStore,
    plan: luthor::supervisor::LaunchPlan,
    marker: PathBuf,
    abort: PathBuf,
}

impl Aborted {
    fn delayed() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
        let owner = WorktreeOwner::acquire(store.root(), "task").unwrap();
        let binary = dir.path().join("delayed-supervisor");
        let release = dir.path().join("start-supervisor");
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nwhile [ ! -f '{}' ]; do sleep 0.02; done\nexec '{}' \"$@\"\n",
                release.display(),
                env!("CARGO_BIN_EXE_luthor")
            ),
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            execute_with_binary(&mut store, &plan, &binary, &owner),
            Err(SupervisorError::ReadyTimeout)
        ));
        drop(owner);
        assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
        assert!(matches!(
            inspect_recovery_quiescence(&store, "task", "attempt-real").unwrap(),
            RecoveryInspection::Held(_)
        ));
        fs::write(release, "release after timeout").unwrap();
        let abort = config
            .state_root
            .join("attempts/attempt-real.dispatch-aborted.json");
        wait_for_abort(&abort);
        Self {
            _dir: dir,
            config,
            store,
            plan,
            marker,
            abort,
        }
    }

    fn recover(&mut self) -> (RecoveryResult, usize) {
        let selection = task_records::selection_evidence(&self.store, "task")
            .unwrap()
            .unwrap();
        let mut projects = OtherProject(selection.candidate, 1);
        let mut prs = ExitPr::default();
        let result = operator_recover_missing_receipt(
            &mut self.store,
            "task",
            "attempt-real",
            "operator",
            "handshake cancelled",
            &mut projects,
            &mut prs,
        )
        .unwrap();
        (result, prs.reads)
    }
}

fn wait_for_abort(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "no dispatch abort marker");
        thread::sleep(Duration::from_millis(10));
    }
    let proof: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    wait_for_process_and_group_absence(proof["supervisor"]["pid"].as_i64().unwrap() as i32);
}

#[test]
fn delayed_supervisor_aborts_without_child_and_recovers_to_retry_after_restart() {
    let mut f = Aborted::delayed();
    assert!(!f.marker.exists());
    assert!(
        !f.config
            .state_root
            .join("attempts/attempt-real.child.json")
            .exists()
    );
    assert!(!receipt_path(&f.config).exists());
    let log = fs::read_to_string(
        f.config
            .state_root
            .join("attempts/attempt-real.supervisor.log"),
    )
    .unwrap();
    assert!(!log.contains("panicked"), "{log}");
    assert_eq!(
        inspect_recovery_quiescence(&f.store, "task", "attempt-real").unwrap(),
        RecoveryInspection::Quiescent
    );
    assert_eq!(f.recover(), (RecoveryResult::RecoveredAttention, 1));
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    drop(f.store);
    let mut store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("attention")
    );
    assert!(scheduling::pending_attempts(&store).unwrap().is_empty());
    // An abort never qualifies for automatic natural-exit retry or receipt loss.
    assert!(luthor::state::retry_context_for_task(&store, "task", "attempt-real").is_err());
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let mut projects = OtherProject(selection.candidate, 1);
    let mut prs = ExitPr::default();
    let mut launcher = BuiltLauncher;
    let retry = retry_one(
        &mut store,
        RetryDependencies {
            task_id: "task",
            previous_attempt_id: "attempt-real",
            attempt_id: "attempt-retry",
            config: &f.config,
            config_revision: "retry-after-abort",
            actor: "operator",
            reason: "retry cancelled dispatch",
            revalidate_terminal_exit: false,
            projects: &mut projects,
            prs: &mut prs,
            launcher: &mut launcher,
        },
    )
    .unwrap();
    assert_eq!(retry.session_id, f.plan.session_id);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f.marker.exists() {
        assert!(Instant::now() < deadline, "retried worker not started");
        thread::sleep(Duration::from_millis(10));
    }
    let receipt = f
        .config
        .state_root
        .join("attempts/attempt-retry.receipt.json");
    while !receipt.exists() {
        assert!(Instant::now() < deadline, "retried worker not finished");
        thread::sleep(Duration::from_millis(10));
    }
}

fn prepare_managed_supervisor_plan(
    store: &mut StateStore,
    plan: &luthor::supervisor::LaunchPlan,
) -> WorktreeOwner {
    luthor::supervisor::preflight_attempt_storage(store.root()).unwrap();
    let plan_path = store
        .root()
        .join("attempts")
        .join(format!("{}.plan.json", plan.attempt_id));
    fs::write(&plan_path, serde_json::to_vec(plan).unwrap()).unwrap();
    fs::set_permissions(&plan_path, fs::Permissions::from_mode(0o600)).unwrap();
    let owner = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    let proof = owner
        .protocol_evidence(store.root(), &plan.task_id, &plan.attempt_id)
        .unwrap();
    launches::begin_supervision(
        store,
        &plan.task_id,
        &plan.attempt_id,
        &serde_json::to_string(plan).unwrap(),
        &proof,
    )
    .unwrap();
    owner
}

#[test]
fn closed_ready_pipe_reaps_unreleased_child_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    let attempts = config.state_root.join("attempts");
    let owner = prepare_managed_supervisor_plan(&mut store, &plan);
    let binary = dir.path().join("closed-output-supervisor");
    let release = dir.path().join("release");
    fs::write(
        &binary,
        format!(
            "#!/bin/sh\nwhile [ ! -f '{}' ]; do sleep 0.02; done\nexec '{}' \"$@\"\n",
            release.display(),
            env!("CARGO_BIN_EXE_luthor")
        ),
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = Command::new(binary);
    command
        .args([
            "__supervise",
            config.state_root.to_str().unwrap(),
            "attempt-real",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    owner.inherit_into(&mut command);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut supervisor = command.spawn().unwrap();
    drop(owner);
    drop(supervisor.stdout.take());
    // Keep the cancellation pipe OPEN: force the fallible READY write, not the pre-spawn check.
    fs::write(release, "go").unwrap();
    let abort = attempts.join("attempt-real.dispatch-aborted.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !abort.exists() {
        assert!(
            Instant::now() < deadline,
            "closed READY pipe left no terminal proof"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let status = supervisor.wait().unwrap();
    assert!(!status.success());
    let mut error = String::new();
    std::io::Read::read_to_string(&mut supervisor.stderr.take().unwrap(), &mut error).unwrap();
    assert!(!error.contains("panicked"), "{error}");
    assert!(!marker.exists());
    let proof: serde_json::Value = serde_json::from_slice(&fs::read(abort).unwrap()).unwrap();
    assert!(proof["child"]["pid"].as_u64().unwrap() > 0);
    assert_eq!(
        inspect_recovery_quiescence(&store, "task", "attempt-real").unwrap(),
        RecoveryInspection::Quiescent
    );
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    assert_eq!(
        operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "closed READY pipe stopped the unreleased shim",
            &mut OtherProject(selection.candidate, 1),
            &mut ExitPr::default(),
        )
        .unwrap(),
        RecoveryResult::RecoveredAttention
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
}

#[test]
fn dispatch_abort_recovery_refuses_missing_unsafe_contradictory_or_live_proof() {
    let mut f = Aborted::delayed();
    let saved = fs::read(&f.abort).unwrap();
    for change in [
        "missing",
        "malformed",
        "plan",
        "live_supervisor",
        "boot",
        "child",
        "unknown_field",
        "public_mode",
        "symlink",
    ] {
        let mut proof: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        match change {
            "plan" => proof["plan"]["attempt_id"] = "other-attempt".into(),
            "live_supervisor" => proof["supervisor"]["pid"] = std::process::id().into(),
            "boot" => proof["supervisor"]["boot_identity"] = "another-boot".into(),
            "child" => proof["child"] = proof["supervisor"].clone(),
            "unknown_field" => proof["plan"]["unrecognized"] = true.into(),
            _ => {}
        }
        fs::write(&f.abort, serde_json::to_vec(&proof).unwrap()).unwrap();
        match change {
            "missing" => fs::remove_file(&f.abort).unwrap(),
            "malformed" => fs::write(&f.abort, "{").unwrap(),
            "public_mode" => {
                fs::set_permissions(&f.abort, fs::Permissions::from_mode(0o644)).unwrap()
            }
            "symlink" => {
                fs::remove_file(&f.abort).unwrap();
                std::os::unix::fs::symlink(
                    f.config.state_root.join("attempts/attempt-real.plan.json"),
                    &f.abort,
                )
                .unwrap();
            }
            _ => {}
        }
        let (result, reads) = f.recover();
        assert!(
            matches!(result, RecoveryResult::Held(_)),
            "{change}: {result:?}"
        );
        assert_eq!(reads, 0, "{change}");
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
        if change == "symlink" {
            fs::remove_file(&f.abort).unwrap();
        }
        fs::write(&f.abort, &saved).unwrap();
        fs::set_permissions(&f.abort, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let owner = WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap();
    assert!(matches!(f.recover().0, RecoveryResult::Held(_)));
    drop(owner);
    let db = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    db.execute("INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES('contradiction','task','attempt-real','gate_release','{}')", []).unwrap();
    assert!(matches!(f.recover().0, RecoveryResult::Held(_)));
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    db.execute("DELETE FROM intents WHERE id='contradiction'", [])
        .unwrap();
    db.execute(
        "DELETE FROM evidence WHERE kind='worktree_owner_protocol'",
        [],
    )
    .unwrap();
    assert!(matches!(f.recover().0, RecoveryResult::Held(_)));
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
}

#[test]
fn dispatch_abort_recovery_keeps_capacity_when_pr_read_fails() {
    let mut f = Aborted::delayed();
    let selection = task_records::selection_evidence(&f.store, "task")
        .unwrap()
        .unwrap();
    let mut projects = OtherProject(selection.candidate, 1);
    let mut prs = ExitPr {
        fail: true,
        ..ExitPr::default()
    };
    assert!(matches!(
        operator_recover_missing_receipt(
            &mut f.store,
            "task",
            "attempt-real",
            "operator",
            "cancelled",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        RecoveryResult::Held(_)
    ));
    assert_eq!(prs.reads, 1);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("held")
    );
}

#[test]
fn recovered_dispatch_abort_retry_rechecks_the_exact_audited_marker() {
    let mut f = Aborted::delayed();
    assert_eq!(f.recover(), (RecoveryResult::RecoveredAttention, 1));
    let saved = fs::read(&f.abort).unwrap();
    let selection = task_records::selection_evidence(&f.store, "task")
        .unwrap()
        .unwrap();
    for change in ["missing", "identity"] {
        if change == "missing" {
            fs::remove_file(&f.abort).unwrap();
        } else {
            let mut marker: serde_json::Value = serde_json::from_slice(&saved).unwrap();
            marker["supervisor"]["start_identity"] = "different-start".into();
            fs::write(&f.abort, serde_json::to_vec(&marker).unwrap()).unwrap();
            fs::set_permissions(&f.abort, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut projects = OtherProject(selection.candidate.clone(), 1);
        let mut prs = ExitPr::default();
        let mut launcher = BuiltLauncher;
        assert!(
            retry_one(
                &mut f.store,
                RetryDependencies {
                    task_id: "task",
                    previous_attempt_id: "attempt-real",
                    attempt_id: "attempt-retry",
                    config: &f.config,
                    config_revision: "retry-after-abort",
                    actor: "operator",
                    reason: "retry cancelled dispatch",
                    revalidate_terminal_exit: false,
                    projects: &mut projects,
                    prs: &mut prs,
                    launcher: &mut launcher,
                }
            )
            .is_err(),
            "{change}"
        );
        assert_eq!(prs.reads, 0, "{change}");
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_records::latest_attempt(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("attempt-real")
        );
        assert!(!f.marker.exists());
        fs::write(&f.abort, &saved).unwrap();
        fs::set_permissions(&f.abort, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
