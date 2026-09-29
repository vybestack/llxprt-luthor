use luthor::{
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    eligibility::Candidate,
    state::{StateError, StateStore, WorktreeIdentity, WorktreeIntent},
    supervisor::{
        Reconciliation, SupervisorError, execute_with_binary, prepare_initial, prepare_resume,
        reconcile_attempt, request_stop, run_gated_child_with_binary,
    },
};
use std::{fs, io::Cursor, path::Path};
#[cfg(unix)]
use std::{
    io::BufRead,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

fn configured(root: &Path) -> (Config, Candidate) {
    let mapping = Mapping {
        tracker_repository: "org/tracker".into(),
        code_repository: "org/code".into(),
        checkout: root.into(),
        base_branch: "main".into(),
        push_remote: "origin".into(),
        allowed_pr_head_repository: "org/code".into(),
        allowed_pr_author: "operator".into(),
    };
    let source = Source {
        project_id: "project".into(),
        repositories: vec!["org/tracker".into()],
        ready_marker: Marker::Label {
            name: "ready".into(),
        },
        milestone: None,
    };
    let initial = CommandTemplate {
        executable: root.join("worker-that-must-not-run"),
        args: vec![
            "--session".into(),
            "{task.id}".into(),
            "--cwd".into(),
            "{worktree}".into(),
            "-p".into(),
            "Work on {task.issue_url} for {attempt.id}".into(),
        ],
    };
    let config = Config {
        state_root: root.join("state"),
        worktree_root: root.join("worktrees"),
        capacity: 1,
        assignment_login: "operator".into(),
        sources: vec![source.clone()],
        mappings: vec![mapping.clone()],
        initial: initial.clone(),
        resume: CommandTemplate {
            executable: initial.executable.clone(),
            args: vec![
                "--session".into(),
                "{task.id}".into(),
                "--cwd".into(),
                "{worktree}".into(),
                "--prompt".into(),
                "Continue {task.issue_url} for {attempt.id}".into(),
            ],
        },
    };
    let candidate = Candidate {
        project_id: "project".into(),
        item_id: "item".into(),
        repository: "org/tracker".into(),
        issue_node_id: "issue".into(),
        issue_number: 7,
        issue_url: "https://github.com/org/tracker/issues/7".into(),
        tracker_repo_id: "repo".into(),
        milestone_id: None,
        milestone_title: None,
        observed_at_unix_secs: 1,
        observed_state: "open".into(),
        observed_assignees: vec![],
        observed_labels: vec!["ready".into()],
        observed_project_fields: vec![],
        marker: source.ready_marker.clone(),
        mapping,
        source,
    };
    (config, candidate)
}

fn claimed(store: &mut StateStore, config: &Config, candidate: &Candidate, root: &Path) {
    store.create_task("task", candidate, "rev", config).unwrap();
    store
        .record_claim_intent("task", "operator", "org/tracker", 7)
        .unwrap();
    store
        .record_evidence("task", None, "claim_verified", "operator")
        .unwrap();
    store.set_task_phase("task", "claimed").unwrap();
    let path = root.join("worktrees");
    fs::create_dir_all(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let intent = WorktreeIntent {
        path: path.clone(),
        branch: "luthor/task".into(),
        base: "main".into(),
        repository: "org/code".into(),
    };
    store.begin_worktree("task", &intent).unwrap();
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(&path).unwrap();
    let identity = WorktreeIdentity {
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
        branch: intent.branch.clone(),
        base: intent.base.clone(),
        head: "abc".into(),
        repository: intent.repository.clone(),
        git_directory: root.into(),
        remote: "origin".into(),
    };
    store.finish_worktree("task", &identity).unwrap();
}

#[test]
fn intent_and_slot_survive_restart_and_failed_dispatch_stays_held() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    let plan = prepare_initial(&mut store, "task", "attempt-1").unwrap();
    assert_eq!(plan.session_id, "task");
    assert!(plan.args.iter().any(|arg| arg.contains("attempt-1")));
    let initial_prompt = plan.args.windows(2).find(|pair| pair[0] == "-p").unwrap()[1].as_str();
    for required in [
        "Work only in code repository org/code",
        "mapped base branch main",
        "PR head in repository org/code on branch luthor/task, pushed to remote origin",
        "Tracker-Issue: https://github.com/org/tracker/issues/7",
        "authorized PR author is operator",
        "already claimed; do not reassign it",
        "Create only an open PR",
        "Report the PR URL and ID",
    ] {
        assert!(initial_prompt.contains(required), "missing {required}");
    }
    assert_eq!(
        plan.args
            .iter()
            .filter(|arg| arg.as_str() == "-p" || arg.as_str() == "--prompt")
            .count(),
        1
    );
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(execute_with_binary(&mut store, &plan, &dir.path().join("absent-luthor")).is_err());
    assert!(execute_with_binary(&mut store, &plan, &dir.path().join("absent-luthor")).is_err());
    assert!(!plan.executable.exists());
    drop(store);

    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    let persisted = store.launch_intent("attempt-1").unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<luthor::supervisor::LaunchPlan>(&persisted).unwrap(),
        plan
    );
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(matches!(
        store.release_reservation("attempt-1"),
        Err(StateError::LaunchBlocked)
    ));
    assert!(prepare_initial(&mut store, "task", "attempt-2").is_err());
    let mut other = candidate.clone();
    other.issue_node_id = "other-issue".into();
    other.issue_number = 8;
    other.issue_url = "https://github.com/org/tracker/issues/8".into();
    store
        .create_task("other-task", &other, "rev", &config)
        .unwrap();
    assert!(matches!(
        store.reserve("other-task", "other-attempt"),
        Err(StateError::Capacity { .. })
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
}

#[test]
fn unverified_worktree_or_missing_session_cannot_reserve_or_launch() {
    let dir = tempfile::tempdir().unwrap();
    let (mut config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    store
        .create_task("task", &candidate, "rev", &config)
        .unwrap();
    assert!(prepare_initial(&mut store, "task", "attempt-1").is_err());
    assert_eq!(store.reservation_count().unwrap(), 0);
    drop(store);
    config.initial.args = vec![
        "--cwd".into(),
        "{worktree}".into(),
        "-p".into(),
        "prompt".into(),
    ];
    let mut store = StateStore::open(dir.path().join("other-state"), 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    assert!(matches!(
        prepare_initial(&mut store, "task", "attempt-1"),
        Err(SupervisorError::Conflict)
    ));
    assert_eq!(store.reservation_count().unwrap(), 0);
}

#[test]
fn changed_worktree_identity_blocks_before_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    fs::rename(
        dir.path().join("worktrees"),
        dir.path().join("old-worktrees"),
    )
    .unwrap();
    fs::create_dir(dir.path().join("worktrees")).unwrap();
    assert!(prepare_initial(&mut store, "task", "attempt-1").is_err());
    assert_eq!(store.reservation_count().unwrap(), 0);
}

#[cfg(unix)]
fn fake_plan(root: &Path, attempt: &str, code: i32) -> luthor::supervisor::LaunchPlan {
    use std::os::unix::fs::PermissionsExt;
    let executable = root.join(format!("worker-{attempt}"));
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf 'out-{}\\n'\nprintf 'err-{}\\n' >&2\nexit {code}\n",
            attempt, attempt
        ),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    luthor::supervisor::LaunchPlan {
        task_id: "task".into(),
        attempt_id: attempt.into(),
        session_id: "session".into(),
        worktree: root.into(),
        executable,
        args: vec![],
        config_revision: "rev".into(),
        session_environment: luthor::supervisor::SessionEnvironment {
            home: root.into(),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_state_home: None,
            llxprt_config_home: None,
        },
    }
}

#[cfg(unix)]
#[test]
fn gate_eof_never_spawns_and_does_not_write_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("launched");
    let executable = dir.path().join("worker");
    fs::write(
        &executable,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let plan = luthor::supervisor::LaunchPlan {
        task_id: "task".into(),
        attempt_id: "held".into(),
        session_id: "s".into(),
        worktree: dir.path().into(),
        executable,
        args: vec![],
        config_revision: "rev".into(),
        session_environment: luthor::supervisor::SessionEnvironment {
            home: dir.path().into(),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_state_home: None,
            llxprt_config_home: None,
        },
    };
    assert!(matches!(
        run_gated_child_with_binary(
            &plan,
            Cursor::new(Vec::<u8>::new()),
            dir.path(),
            Path::new(env!("CARGO_BIN_EXE_luthor"))
        ),
        Err(SupervisorError::GateClosed)
    ));
    assert!(!marker.exists());
    assert!(!dir.path().join("held.receipt.json").exists());
}

#[cfg(unix)]
#[test]
fn released_gate_captures_durable_logs_and_receipts_real_exit() {
    for code in [0, 7] {
        let dir = tempfile::tempdir().unwrap();
        let attempt = format!("exit-{code}");
        let plan = fake_plan(dir.path(), &attempt, code);
        let status = run_gated_child_with_binary(
            &plan,
            Cursor::new(b"R"),
            dir.path(),
            Path::new(env!("CARGO_BIN_EXE_luthor")),
        )
        .unwrap();
        assert_eq!(status.code(), Some(code));
        let stdout_path = dir.path().join(format!("{attempt}.stdout.log"));
        let stderr_path = dir.path().join(format!("{attempt}.stderr.log"));
        assert_eq!(
            fs::read(&stdout_path).unwrap(),
            format!("out-{attempt}\n").as_bytes()
        );
        assert_eq!(
            fs::read(&stderr_path).unwrap(),
            format!("err-{attempt}\n").as_bytes()
        );
        let receipt_path = dir.path().join(format!("{attempt}.receipt.json"));
        let bytes = fs::read(&receipt_path).unwrap();
        let receipt: luthor::supervisor::ExitReceipt = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipt.exit_code, Some(code));
        assert_eq!(receipt.signal, None);
        assert_eq!(
            receipt.stdout_bytes,
            fs::metadata(stdout_path).unwrap().len()
        );
        assert_eq!(
            receipt.stderr_bytes,
            fs::metadata(stderr_path).unwrap().len()
        );
        assert!(
            receipt.child_pid > 0
                && !receipt.boot_identity.is_empty()
                && !receipt.child_start_identity.is_empty()
        );
        assert_eq!(
            receipt.stdout_path,
            dir.path().join(format!("{attempt}.stdout.log"))
        );
    }
}

#[cfg(unix)]
fn prepared_fake_worker(
    dir: &tempfile::TempDir,
) -> (
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
    std::path::PathBuf,
) {
    use std::os::unix::fs::PermissionsExt;
    let (mut config, candidate) = configured(dir.path());
    let marker = dir.path().join("worker-started");
    let worker = dir.path().join("worker");
    fs::write(&worker, format!("#!/bin/sh\necho started > '{}'\nprintf 'worker stdout\\n'\nprintf 'worker stderr\\n' >&2\n", marker.display())).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    config.initial.executable = worker;
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    let plan = prepare_initial(&mut store, "task", "attempt-real").unwrap();
    (config, store, plan, marker)
}

#[cfg(unix)]
#[test]
fn detached_same_binary_dispatch_records_gate_and_worker_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let receipt = config.state_root.join("attempts/attempt-real.receipt.json");
    for _ in 0..100 {
        if receipt.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(30));
    }
    assert!(marker.exists());
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(fs::read(receipt.stdout_path).unwrap(), b"worker stdout\n");
    assert_eq!(fs::read(receipt.stderr_path).unwrap(), b"worker stderr\n");
    assert_eq!(store.reservation_count().unwrap(), 1);
    let reconciled = reconcile_attempt(&mut store, "task", "attempt-real").unwrap();
    assert_eq!(
        reconciled,
        Reconciliation::Completed {
            exit_code: Some(0),
            signal: None
        }
    );
    assert_eq!(store.reservation_count().unwrap(), 0);
    drop(store);
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(0),
            signal: None
        }
    );
    assert_eq!(store.reservation_count().unwrap(), 0);
    let kinds = store.evidence_kinds("task").unwrap();
    assert!(kinds.contains(&"supervisor_ready".into()));
    assert!(kinds.contains(&"gate_sent".into()));
    assert!(
        execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).is_err()
    );
}
#[cfg(target_os = "macos")]
fn test_process_identity(pid: u32) -> (String, String) {
    let boot = Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .unwrap();
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    assert_eq!(
        unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        },
        size
    );
    (
        String::from_utf8(boot.stdout).unwrap().trim().into(),
        format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
    )
}

#[cfg(target_os = "linux")]
fn test_process_identity(pid: u32) -> (String, String) {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .to_owned();
    (boot.trim().into(), start)
}

#[cfg(unix)]
fn dispatched_fixture(code: i32) -> (tempfile::TempDir, Config, StateStore) {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    if code != 0 {
        fs::write(
            &plan.executable,
            format!(
                "#!/bin/sh\nprintf 'worker stdout\\n'\nprintf 'worker stderr\\n' >&2\nexit {code}\n"
            ),
        )
        .unwrap();
    }
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let receipt = config.state_root.join("attempts/attempt-real.receipt.json");
    for _ in 0..200 {
        if receipt.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(receipt.exists());
    (dir, config, store)
}

#[cfg(unix)]
fn receipt_path(config: &Config) -> std::path::PathBuf {
    config.state_root.join("attempts/attempt-real.receipt.json")
}

#[cfg(unix)]
fn hold_slot(store: &mut StateStore) {
    assert!(matches!(
        reconcile_attempt(store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { .. }
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .contains(&"attempt_exit".into())
    );
}

#[cfg(unix)]
fn edit_receipt(config: &Config, change: impl FnOnce(&mut luthor::supervisor::ExitReceipt)) {
    let path = receipt_path(config);
    let mut receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    change(&mut receipt);
    fs::write(path, serde_json::to_vec(&receipt).unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn verified_exit_seven_releases_once_and_survives_restart() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let expected = Reconciliation::Completed {
        exit_code: Some(7),
        signal: None,
    };
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        expected
    );
    assert_eq!(store.reservation_count().unwrap(), 0);
    drop(store);
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        expected
    );
    assert_eq!(
        store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .filter(|k| *k == "attempt_exit")
            .count(),
        1
    );
    assert_eq!(store.reservation_count().unwrap(), 0);
    edit_receipt(&config, |r| r.exit_code = Some(0));
    assert!(reconcile_attempt(&mut store, "task", "attempt-real").is_err());
    assert_eq!(
        store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .filter(|k| *k == "attempt_exit")
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn absent_and_corrupt_receipt_keep_reservation() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    let receipt = receipt_path(&config);
    fs::remove_file(&receipt).unwrap();
    hold_slot(&mut store);
    fs::write(&receipt, b"{partial").unwrap();
    hold_slot(&mut store);
}

#[cfg(unix)]
#[test]
fn plan_without_persisted_launch_intent_keeps_slot() {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    store
        .create_task("task", &candidate, "rev", &config)
        .unwrap();
    store.reserve("task", "attempt-real").unwrap();
    let attempts = config.state_root.join("attempts");
    fs::DirBuilder::new().mode(0o700).create(&attempts).unwrap();
    let mut plan = fake_plan(dir.path(), "attempt-real", 0);
    plan.session_id = "task".into();
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempts.join("attempt-real.plan.json"))
        .unwrap();
    serde_json::to_writer(file, &plan).unwrap();
    assert!(
        matches!(reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "missing launch intent")
    );
    assert_eq!(store.reservation_count().unwrap(), 1);
}

#[cfg(unix)]
#[test]
fn missing_receipt_after_dispatch_before_worker_start_keeps_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    assert!(execute_with_binary(&mut store, &plan, &dir.path().join("missing-binary")).is_err());
    assert!(!receipt_path(&config).exists());
    hold_slot(&mut store);
}

#[cfg(unix)]
#[test]
fn incomplete_or_nonregular_logs_keep_reservation() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    let stdout = config.state_root.join("attempts/attempt-real.stdout.log");
    let original = fs::read(&stdout).unwrap();
    fs::write(&stdout, &original[..original.len() - 1]).unwrap();
    hold_slot(&mut store);
    fs::remove_file(&stdout).unwrap();
    std::os::unix::fs::symlink(receipt_path(&config), &stdout).unwrap();
    hold_slot(&mut store);
}

#[cfg(unix)]
#[test]
fn contradictory_identity_and_malformed_receipts_keep_reservation() {
    let (_dir, _config, mut store) = dispatched_fixture(0);
    let fake = serde_json::json!({"pid":std::process::id(),"boot_identity":"fake","start_identity":"fake"});
    store
        .record_evidence("task", Some("attempt-real"), "gate_sent", &fake.to_string())
        .unwrap();
    assert!(reconcile_attempt(&mut store, "task", "attempt-real").is_err());
    assert_eq!(store.reservation_count().unwrap(), 1);
    let (_other_dir, config, mut other_store) = dispatched_fixture(0);
    edit_receipt(&config, |r| r.signal = Some(9));
    hold_slot(&mut other_store);
}

#[cfg(unix)]
#[test]
fn reused_child_pid_with_contradictory_start_identity_keeps_slot() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    let (boot, _) = test_process_identity(std::process::id());
    edit_receipt(&config, |r| {
        r.child_pid = std::process::id();
        r.boot_identity = boot;
        r.child_start_identity = "not this process start".into();
    });
    assert!(
        matches!(reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "receipt identity or shape mismatch")
    );
    assert_eq!(store.reservation_count().unwrap(), 1);
}

#[cfg(unix)]
#[test]
fn gate_release_decision_reconciles_without_gate_sent_evidence() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute(
        "DELETE FROM evidence WHERE task_id='task' AND attempt_id='attempt-real' AND kind='gate_sent'",
        [],
    )
    .unwrap();
    let expected = Reconciliation::Completed {
        exit_code: Some(0),
        signal: None,
    };
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        expected
    );
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        expected
    );
    assert_eq!(
        store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .filter(|kind| *kind == "attempt_exit")
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn missing_child_registration_holds_even_with_valid_receipt() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute(
        "DELETE FROM evidence WHERE attempt_id='attempt-real' AND kind='child_registered'",
        [],
    )
    .unwrap();
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "missing child registration"
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
}

#[cfg(unix)]
#[test]
fn live_child_group_is_not_released() {
    use std::os::unix::process::CommandExt;
    let (_dir, config, mut store) = dispatched_fixture(0);
    let mut live = Command::new("/bin/sleep")
        .arg("10")
        .process_group(0)
        .spawn()
        .unwrap();
    assert_eq!(unsafe { libc::kill(-(live.id() as i32), 0) }, 0);
    let (boot, start) = test_process_identity(live.id());
    edit_receipt(&config, |r| {
        r.child_pid = live.id();
        r.boot_identity = boot;
        r.child_start_identity = start;
    });
    assert!(
        matches!(reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "receipt identity or shape mismatch")
    );
    assert_eq!(store.reservation_count().unwrap(), 1);
    live.kill().unwrap();
    live.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn same_binary_ready_without_release_does_not_launch_worker() {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    let attempts = config.state_root.join("attempts");
    fs::DirBuilder::new().mode(0o700).create(&attempts).unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempts.join("attempt-real.plan.json"))
        .unwrap();
    serde_json::to_writer(&mut file, &plan).unwrap();
    file.sync_all().unwrap();
    fs::File::open(&attempts).unwrap().sync_all().unwrap();
    store
        .begin_supervision(
            "task",
            "attempt-real",
            &serde_json::to_string(&plan).unwrap(),
        )
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args([
            "__supervise",
            config.state_root.to_str().unwrap(),
            "attempt-real",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "READY\n");
    drop(child.stdin.take());
    assert!(!child.wait().unwrap().success());
    assert!(!marker.exists());
    assert!(attempts.join("attempt-real.supervisor-error.json").exists());
    assert_eq!(store.reservation_count().unwrap(), 1);
}
#[cfg(unix)]
#[test]
fn registered_shim_stays_gated_and_survives_supervisor_crash_after_release() {
    use std::{
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    fs::write(
        &plan.executable,
        format!(
            "#!/bin/sh\necho started > '{}'\nexec /bin/sleep 30\n",
            marker.display()
        ),
    )
    .unwrap();
    let attempts = config.state_root.join("attempts");
    fs::DirBuilder::new().mode(0o700).create(&attempts).unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempts.join("attempt-real.plan.json"))
        .unwrap();
    serde_json::to_writer(&mut file, &plan).unwrap();
    file.sync_all().unwrap();
    fs::File::open(&attempts).unwrap().sync_all().unwrap();
    store
        .begin_supervision(
            "task",
            "attempt-real",
            &serde_json::to_string(&plan).unwrap(),
        )
        .unwrap();
    let mut supervisor = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args([
            "__supervise",
            config.state_root.to_str().unwrap(),
            "attempt-real",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    std::io::BufReader::new(supervisor.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "READY\n");
    let identity_path = attempts.join("attempt-real.child.json");
    let identity: serde_json::Value =
        serde_json::from_slice(&fs::read(&identity_path).unwrap()).unwrap();
    let pid = identity["pid"].as_i64().unwrap() as i32;
    assert_eq!(identity["group_id"].as_i64().unwrap(), i64::from(pid));
    assert_eq!(unsafe { libc::getpgid(pid) }, pid);
    assert!(!marker.exists());
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "child_registered",
            fs::read_to_string(&identity_path).unwrap().trim(),
        )
        .unwrap();
    assert!(!marker.exists());
    let (boot, start) = test_process_identity(supervisor.id());
    let process =
        serde_json::json!({"pid":supervisor.id(),"boot_identity":boot,"start_identity":start});
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "supervisor_ready",
            &process.to_string(),
        )
        .unwrap();
    store
        .record_intent(
            "gate-attempt-real",
            "task",
            Some("attempt-real"),
            "gate_release",
            &process.to_string(),
        )
        .unwrap();
    assert!(!marker.exists());
    supervisor.stdin.take().unwrap().write_all(b"R").unwrap();
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(marker.exists());
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    let (boot, start) = test_process_identity(pid as u32);
    assert_eq!(identity["boot_identity"], boot);
    assert_eq!(identity["start_identity"], start);
    assert_eq!(unsafe { libc::getpgid(pid) }, pid);
    assert_eq!(
        identity,
        serde_json::from_slice::<serde_json::Value>(&fs::read(identity_path).unwrap()).unwrap()
    );
    assert!(!attempts.join("attempt-real.receipt.json").exists());
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { .. }
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    request_stop(&mut store, "task", "attempt-real").unwrap();
    assert!(store.stop_intent("task", "attempt-real").unwrap().is_some());
    assert_eq!(unsafe { libc::kill(-pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert!(!attempts.join("attempt-real.receipt.json").exists());
    let kinds = store.evidence_kinds("task").unwrap();
    assert!(kinds.contains(&"independent_stop_decision".into()));
    assert!(kinds.contains(&"independent_stop_signal".into()));
    assert!(kinds.contains(&"independent_group_absent".into()));
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "missing or invalid receipt"
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    drop(store);
    let store = StateStore::open(&config.state_root, 1).unwrap();
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(
        store
            .evidence_kinds("task")
            .unwrap()
            .contains(&"independent_group_absent".into())
    );
}

#[cfg(unix)]
#[test]
fn spoofed_child_identity_does_not_signal_live_group() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    store
        .begin_supervision(
            "task",
            "attempt-real",
            &serde_json::to_string(&plan).unwrap(),
        )
        .unwrap();
    let mut dead = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let (boot, start) = test_process_identity(dead.id());
    dead.kill().unwrap();
    dead.wait().unwrap();
    let supervisor =
        serde_json::json!({"pid":dead.id(),"boot_identity":boot,"start_identity":start});
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "supervisor_ready",
            &supervisor.to_string(),
        )
        .unwrap();
    store
        .record_intent(
            "gate-attempt-real",
            "task",
            Some("attempt-real"),
            "gate_release",
            &supervisor.to_string(),
        )
        .unwrap();
    let mut live = Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let (boot, _) = test_process_identity(live.id());
    let spoof = serde_json::json!({"pid":live.id(),"boot_identity":boot,"start_identity":"recycled","group_id":live.id()});
    let attempts = config.state_root.join("attempts");
    fs::create_dir_all(&attempts).unwrap();
    fs::write(attempts.join("attempt-real.child.json"), spoof.to_string()).unwrap();
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "child_registered",
            &spoof.to_string(),
        )
        .unwrap();
    assert!(matches!(
        request_stop(&mut store, "task", "attempt-real"),
        Err(SupervisorError::StopUnavailable)
    ));
    assert!(live.try_wait().unwrap().is_none());
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .contains(&"independent_stop_decision".into())
    );
    live.kill().unwrap();
    live.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn matching_supervisor_without_socket_does_not_fallback_to_group_signal() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    store
        .begin_supervision(
            "task",
            "attempt-real",
            &serde_json::to_string(&plan).unwrap(),
        )
        .unwrap();
    let (boot, start) = test_process_identity(std::process::id());
    let supervisor =
        serde_json::json!({"pid":std::process::id(),"boot_identity":boot,"start_identity":start});
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "supervisor_ready",
            &supervisor.to_string(),
        )
        .unwrap();
    store
        .record_intent(
            "gate-attempt-real",
            "task",
            Some("attempt-real"),
            "gate_release",
            &supervisor.to_string(),
        )
        .unwrap();
    let mut live = Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let (boot, start) = test_process_identity(live.id());
    let child = serde_json::json!({"pid":live.id(),"boot_identity":boot,"start_identity":start,"group_id":live.id()});
    let attempts = config.state_root.join("attempts");
    fs::create_dir_all(&attempts).unwrap();
    fs::write(attempts.join("attempt-real.child.json"), child.to_string()).unwrap();
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "child_registered",
            &child.to_string(),
        )
        .unwrap();
    assert!(matches!(
        request_stop(&mut store, "task", "attempt-real"),
        Err(SupervisorError::StopUnavailable)
    ));
    assert!(live.try_wait().unwrap().is_none());
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .contains(&"independent_stop_decision".into())
    );
    live.kill().unwrap();
    live.wait().unwrap();
}

#[cfg(unix)]
fn running_worker(script: &str) -> (tempfile::TempDir, Config, StateStore) {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    fs::write(&plan.executable, script).unwrap();
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let started = config.state_root.join("attempts/attempt-real.stdout.log");
    for _ in 0..200 {
        if fs::read_to_string(&started).is_ok_and(|output| output.contains("started")) {
            return (dir, config, store);
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("worker failed to start");
}

#[cfg(unix)]
fn stopped_receipt(config: &Config) -> luthor::supervisor::ExitReceipt {
    for _ in 0..200 {
        if let Ok(bytes) = fs::read(receipt_path(config)) {
            return serde_json::from_slice(&bytes).unwrap();
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("missing stopped worker receipt");
}

#[cfg(unix)]
#[test]
fn stop_term_has_durable_intent_and_keeps_slot_until_reconcile() {
    let (_dir, config, mut store) =
        running_worker("#!/bin/sh\necho started\ntrap 'exit 0' TERM\nwhile :; do :; done\n");
    request_stop(&mut store, "task", "attempt-real").unwrap();
    assert!(store.stop_intent("task", "attempt-real").unwrap().is_some());
    let receipt = stopped_receipt(&config);
    assert_eq!(receipt.stop_signals, vec![libc::SIGTERM]);
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    drop(store);
    let mut reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        reopened.task_phase("task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(reopened.reservation_count().unwrap(), 0);
    assert!(matches!(
        reconcile_attempt(&mut reopened, "task", "attempt-real").unwrap(),
        Reconciliation::Completed { .. }
    ));
}

#[cfg(unix)]
#[test]
fn stop_escalates_only_on_live_matching_child() {
    let (_dir, config, mut store) =
        running_worker("#!/bin/sh\necho started\ntrap '' TERM\nwhile :; do :; done\n");
    request_stop(&mut store, "task", "attempt-real").unwrap();
    let receipt = stopped_receipt(&config);
    assert_eq!(receipt.stop_signals, vec![libc::SIGTERM, libc::SIGKILL]);
    assert_eq!(receipt.signal, Some(libc::SIGKILL));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed { .. }
    ));
}

#[cfg(unix)]
#[test]
fn absent_supervisor_and_wrong_identity_never_signal_unrelated_process() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, _plan, _) = prepared_fake_worker(&dir);
    let mut unrelated = Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    assert!(request_stop(&mut store, "task", "attempt-real").is_err());
    assert!(store.stop_intent("task", "attempt-real").unwrap().is_some());
    assert!(unrelated.try_wait().unwrap().is_none());
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(!receipt_path(&config).exists());
    let (boot, _) = test_process_identity(unrelated.id());
    let fake =
        serde_json::json!({"pid":unrelated.id(),"boot_identity":boot,"start_identity":"wrong"});
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "supervisor_ready",
            &fake.to_string(),
        )
        .unwrap();
    assert!(request_stop(&mut store, "task", "attempt-real").is_err());
    assert!(unrelated.try_wait().unwrap().is_none());
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { .. }
    ));
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}

#[cfg(unix)]
fn paused_fixture() -> (
    tempfile::TempDir,
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
) {
    let (dir, config, mut store) = dispatched_fixture(0);
    let initial =
        serde_json::from_str(&store.launch_intent("attempt-real").unwrap().unwrap()).unwrap();
    store.record_stop_intent("task", "attempt-real").unwrap();
    edit_receipt(&config, |receipt| {
        receipt.stop_signals = vec![libc::SIGTERM]
    });
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed { .. }
    ));
    store
        .record_pause_pr_lookup(
            "task",
            "attempt-real",
            &luthor::state::PausePrEvidence {
                observed_at_unix_secs: 2,
                repository: "org/code".into(),
                status: luthor::state::PausePrStatus::Absent,
            },
        )
        .unwrap();
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("paused"));
    (dir, config, store, initial)
}

#[cfg(unix)]
#[test]
#[ignore]
fn resume_environment_mismatch_child() {
    let Ok(root) = std::env::var("LUTHOR_RESUME_STATE_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let task = std::env::var("LUTHOR_RESUME_TASK").unwrap();
    let attempt = std::env::var("LUTHOR_RESUME_ATTEMPT").unwrap();
    let mut store = StateStore::open(&root, 1).unwrap();
    assert!(matches!(
        prepare_resume(&mut store, &task, &attempt),
        Err(SupervisorError::Conflict)
    ));
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert_eq!(
        store.latest_attempt(&task).unwrap().as_deref(),
        Some("attempt-real")
    );
    assert!(store.launch_intent(&attempt).unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn resume_rejects_different_root_environment_before_reservation_in_child_process() {
    let (_dir, config, store, initial) = paused_fixture();
    let root = config.state_root.clone();
    let home_b = tempfile::tempdir().unwrap();
    assert_ne!(
        initial.session_environment.home,
        fs::canonicalize(home_b.path()).unwrap()
    );
    assert_eq!(store.reservation_count().unwrap(), 0);
    drop(store);

    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("resume_environment_mismatch_child")
        .arg("--ignored")
        .env_clear()
        .env("HOME", home_b.path())
        .env("XDG_CONFIG_HOME", home_b.path().join("config"))
        .env("LUTHOR_RESUME_STATE_ROOT", &root)
        .env("LUTHOR_RESUME_TASK", "task")
        .env("LUTHOR_RESUME_ATTEMPT", "attempt-next")
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn paused_attempt_prepares_distinct_continuation_after_reopen() {
    let (_dir, config, store, initial) = paused_fixture();
    drop(store);
    let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
    let plan = prepare_resume(&mut store, "task", "attempt-next").unwrap();
    assert_eq!(plan.task_id, initial.task_id);
    assert_eq!(plan.session_id, initial.session_id);
    assert_eq!(plan.worktree, initial.worktree);
    assert_eq!(plan.config_revision, initial.config_revision);
    assert_eq!(plan.attempt_id, "attempt-next");
    let resume_prompt = plan
        .args
        .windows(2)
        .find(|pair| pair[0] == "--prompt")
        .unwrap()[1]
        .as_str();
    assert!(
        resume_prompt
            .starts_with("Continue https://github.com/org/tracker/issues/7 for attempt-next")
    );
    assert!(resume_prompt.contains("Tracker-Issue: https://github.com/org/tracker/issues/7"));
    assert!(resume_prompt.contains("already claimed; do not reassign it"));
    assert_eq!(
        plan.args
            .iter()
            .filter(|arg| arg.as_str() == "-p" || arg.as_str() == "--prompt")
            .count(),
        1
    );
    assert_eq!(
        store.latest_attempt("task").unwrap().as_deref(),
        Some("attempt-next")
    );
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert_eq!(
        serde_json::from_str::<luthor::supervisor::LaunchPlan>(
            &store.launch_intent("attempt-real").unwrap().unwrap()
        )
        .unwrap(),
        initial
    );
    assert_eq!(
        serde_json::from_str::<luthor::supervisor::LaunchPlan>(
            &store.launch_intent("attempt-next").unwrap().unwrap()
        )
        .unwrap(),
        plan
    );
    drop(store);
    let store = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert_eq!(
        store.launch_intent("attempt-next").unwrap().unwrap(),
        serde_json::to_string(&plan).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn resume_requires_paused_verified_exit_and_unused_attempt_id() {
    let (_dir, config, mut store, _) = paused_fixture();
    assert!(prepare_resume(&mut store, "task", "attempt-real").is_err());
    assert_eq!(store.reservation_count().unwrap(), 0);
    store.set_task_phase("task", "held").unwrap();
    assert!(prepare_resume(&mut store, "task", "fresh").is_err());
    assert_eq!(store.reservation_count().unwrap(), 0);
    store.set_task_phase("task", "paused").unwrap();
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute(
        "DELETE FROM evidence WHERE kind='attempt_exit' AND task_id='task'",
        [],
    )
    .unwrap();
    assert!(prepare_resume(&mut store, "task", "fresh").is_err());
    assert_eq!(store.reservation_count().unwrap(), 0);
}

#[cfg(unix)]
#[test]
fn resume_rejects_same_prompt_and_tampered_session_or_inode_before_reservation() {
    for alteration in ["same-prompt", "session", "inode"] {
        let (_dir, config, mut store, initial) = paused_fixture();
        let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
        match alteration {
            "same-prompt" => {
                let mut selection = store.selection_evidence("task").unwrap().unwrap();
                selection.effective_config.resume.args =
                    selection.effective_config.initial.args.clone();
                db.execute(
                    "UPDATE evidence SET payload=?1 WHERE task_id='task' AND kind='selection'",
                    [serde_json::to_string(&selection).unwrap()],
                )
                .unwrap();
            }
            "session" => {
                let mut prior = initial;
                prior.session_id = "other-task".into();
                db.execute(
                    "UPDATE intents SET detail=?1 WHERE task_id='task' AND kind='launch'",
                    [serde_json::to_string(&prior).unwrap()],
                )
                .unwrap();
            }
            "inode" => {
                let mut identity = store
                    .worktree_record("task")
                    .unwrap()
                    .unwrap()
                    .identity
                    .unwrap();
                identity.inode += 1;
                db.execute("UPDATE evidence SET payload=?1 WHERE task_id='task' AND kind='worktree_created'", [serde_json::to_string(&identity).unwrap()]).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            prepare_resume(&mut store, "task", "fresh").is_err(),
            "{alteration}"
        );
        assert_eq!(store.reservation_count().unwrap(), 0, "{alteration}");
        assert!(store.launch_intent("fresh").unwrap().is_none());
    }
}
