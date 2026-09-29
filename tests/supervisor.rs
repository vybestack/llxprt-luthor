use luthor::{
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    eligibility::Candidate,
    state::{StateError, StateStore, WorktreeIdentity, WorktreeIntent},
    supervisor::{SupervisorError, execute_with_binary, prepare_initial, run_gated_child},
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
        resume: initial,
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
        .record_evidence("task", None, "claim_verified", "sole assignee")
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
    };
    assert!(matches!(
        run_gated_child(&plan, Cursor::new(Vec::<u8>::new()), dir.path()),
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
        let status = run_gated_child(&plan, Cursor::new(b"R"), dir.path()).unwrap();
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
    let kinds = store.evidence_kinds("task").unwrap();
    assert!(kinds.contains(&"supervisor_ready".into()));
    assert!(kinds.contains(&"gate_sent".into()));
    assert!(
        execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).is_err()
    );
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
