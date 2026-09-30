use luthor::{
    claim::{AssignmentError, AssignmentWriter},
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    coordinator::{
        IdCreator, ScheduleDependencies, SupervisorLauncher, operator_recover_missing_receipt,
        schedule_candidates,
    },
    eligibility::Candidate,
    github::{
        project::{Issue, Page, ProjectItem, ProjectReadError, ProjectReader},
        pull_request::{ErrorCategory, LookupError, PullRequestReader},
    },
    pr_evidence::{ExpectedPrError, expected_for_task},
    state::{
        ExitPrEvidence, PausePrEvidence, PausePrStatus, StateError, StateStore, WorktreeIdentity,
    },
    supervisor::{
        Reconciliation, RecoveryInspection, SupervisorError, ensure_distinct_resume_prompt,
        execute_with_binary, inspect_recovery_quiescence, prepare_initial, prepare_resume,
        reconcile_attempt, request_stop, run_gated_child_with_binary,
        run_gated_child_with_log_writers,
    },
    worktree::ensure_worktree,
};
use std::{fs, io::Cursor, path::Path};
#[cfg(unix)]
use std::{
    io::BufRead,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn configured(root: &Path) -> (Config, Candidate) {
    let mapping = Mapping {
        tracker_repository: "org/tracker".into(),
        code_repository: "org/code".into(),
        checkout: root.join("checkout"),
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

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn claimed(store: &mut StateStore, config: &Config, candidate: &Candidate, _root: &Path) {
    store.create_task("task", candidate, "rev", config).unwrap();
    store
        .record_claim_intent("task", &config.assignment_login, "org/tracker", 7)
        .unwrap();
    store
        .record_evidence("task", None, "claim_verified", &config.assignment_login)
        .unwrap();
    store.set_task_phase("task", "claimed").unwrap();
    let checkout = &candidate.mapping.checkout;
    fs::create_dir(checkout).unwrap();
    git(checkout, &["init", "-b", "main"]);
    git(checkout, &["config", "user.name", "Fixture"]);
    git(checkout, &["config", "user.email", "fixture@example.org"]);
    git(
        checkout,
        &["remote", "add", "origin", "git@github.com:org/code.git"],
    );
    fs::write(checkout.join("README"), "fixture").unwrap();
    git(checkout, &["add", "README"]);
    git(checkout, &["commit", "-m", "initial"]);
    ensure_worktree(store, "task", &config.worktree_root, &candidate.mapping).unwrap();
}

#[test]
fn initial_prompt_distinguishes_issue_assignee_from_author() {
    let dir = tempfile::tempdir().unwrap();
    let (mut config, mut candidate) = configured(dir.path());
    config.assignment_login = "issue-agent".into();
    config.mappings[0].allowed_pr_author = "acoliver".into();
    candidate.mapping = config.mappings[0].clone();
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    let plan = prepare_initial(&mut store, "task", "attempt-1").unwrap();
    let prompt = plan.args.windows(2).find(|pair| pair[0] == "-p").unwrap()[1].as_str();
    assert!(
        prompt.contains("authorized PR author is acoliver"),
        "{prompt}"
    );
    assert!(
        prompt.contains("tracker issue is assigned to issue-agent"),
        "{prompt}"
    );
    assert!(
        !prompt.contains("authorized PR author is issue-agent"),
        "{prompt}"
    );
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
        "PR head in repository org/code on branch luthor/task, pushed to remote git@github.com:org/code.git",
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
fn branch_switch_before_initial_does_not_reserve_or_launch() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    git(
        &config.worktree_root.join("task"),
        &["switch", "-c", "foreign"],
    );
    assert!(prepare_initial(&mut store, "task", "attempt-1").is_err());
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert!(store.launch_intent("attempt-1").unwrap().is_none());
    assert!(
        !config
            .state_root
            .join("attempts/attempt-1.child.json")
            .exists()
    );
}

#[test]
fn changed_worktree_identity_blocks_before_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    fs::rename(
        dir.path().join("worktrees/task"),
        dir.path().join("old-worktrees"),
    )
    .unwrap();
    fs::create_dir(dir.path().join("worktrees/task")).unwrap();
    assert!(prepare_initial(&mut store, "task", "attempt-1").is_err());
    assert_eq!(store.reservation_count().unwrap(), 0);
}

#[cfg(unix)]
fn direct_snapshot(root: &Path) -> WorktreeIdentity {
    use std::os::unix::fs::MetadataExt;
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.org"]);
    fs::write(root.join("README"), "fixture").unwrap();
    git(root, &["add", "README"]);
    git(root, &["commit", "-m", "initial"]);
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    let meta = fs::metadata(root).unwrap();
    WorktreeIdentity {
        path: fs::canonicalize(root).unwrap(),
        device: meta.dev(),
        inode: meta.ino(),
        branch: "main".into(),
        base: "main".into(),
        head: String::from_utf8(head).unwrap().trim().into(),
        repository: "org/code".into(),
        git_directory: fs::canonicalize(root.join(".git")).unwrap(),
        remote: "origin".into(),
    }
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
        expected_worktree: direct_snapshot(root),
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
        expected_worktree: direct_snapshot(dir.path()),
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
        assert_eq!(
            status.code(),
            Some(code),
            "{}",
            fs::read_to_string(dir.path().join(format!("{attempt}.stderr.log"))).unwrap()
        );
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
struct BrokenLog(std::fs::File);

#[cfg(unix)]
impl std::io::Write for BrokenLog {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("injected log write failure"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.0)
    }
}

#[cfg(unix)]
#[test]
fn live_worker_log_write_failure_stops_and_holds_both_streams() {
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
        store
            .begin_supervision("task", "attempt-1", &serde_json::to_string(&plan).unwrap())
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
        assert!(store.stop_intent("task", "attempt-1").unwrap().is_some());
        assert_eq!(store.reservation_count().unwrap(), 1);
        assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
        assert!(!attempts.join("attempt-1.receipt.json").exists());
        let child: serde_json::Value =
            serde_json::from_slice(&fs::read(attempts.join("attempt-1.child.json")).unwrap())
                .unwrap();
        let pgid = child["pid"].as_i64().unwrap() as i32;
        store
            .record_evidence(
                "task",
                Some("attempt-1"),
                "child_registered",
                &child.to_string(),
            )
            .unwrap();
        assert!(matches!(
            reconcile_attempt(&mut store, "task", "attempt-1").unwrap(),
            Reconciliation::Held { reason } if reason == "log drain failed"
        ));
        assert_eq!(store.reservation_count().unwrap(), 1);
        assert_eq!(unsafe { libc::kill(-pgid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}

#[cfg(unix)]
#[test]
fn log_writer_and_evidence_failure_still_stops_registered_child_group() {
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
    store
        .begin_supervision(
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
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(
        store
            .stop_intent("task", "attempt-fault")
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
fn prepared_fake_worker(
    dir: &tempfile::TempDir,
) -> (
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
    std::path::PathBuf,
) {
    prepared_fake_worker_with_resume_prompt(dir, "Continue {task.issue_url} for {attempt.id}")
}

#[cfg(unix)]
fn prepared_fake_worker_with_resume_prompt(
    dir: &tempfile::TempDir,
    resume_prompt: &str,
) -> (
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
    std::path::PathBuf,
) {
    use std::os::unix::fs::PermissionsExt;
    let (mut config, candidate) = configured(dir.path());
    config.resume.args[5] = resume_prompt.into();
    let marker = dir.path().join("worker-started");
    let worker = dir.path().join("worker");
    fs::write(&worker, format!("#!/bin/sh\necho started > '{}'\nprintf 'worker stdout\\n'\nprintf 'worker stderr\\n' >&2\n", marker.display())).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    config.resume.executable = worker.clone();
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
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if marker.exists() && receipt.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        marker.exists(),
        "released worker did not create its start marker"
    );
    assert!(
        receipt.exists(),
        "started worker did not produce its exit receipt"
    );
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
fn observed_process_identity(pid: u32) -> Option<(String, String)> {
    let boot = Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()?;
    if !boot.status.success() {
        return None;
    }
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    (unsafe {
        libc::proc_pidinfo(
            i32::try_from(pid).ok()?,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    } == size
        && info.pbi_pid == pid
        && info.pbi_start_tvsec != 0)
        .then(|| {
            (
                String::from_utf8_lossy(&boot.stdout).trim().into(),
                format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
            )
        })
}

#[cfg(target_os = "linux")]
fn observed_process_identity(pid: u32) -> Option<(String, String)> {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let start = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    Some((boot.trim().into(), start.into()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn test_process_identity(pid: u32) -> (String, String) {
    observed_process_identity(pid).expect("fixture process identity")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct FixtureGroupGuard(std::path::PathBuf);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for FixtureGroupGuard {
    fn drop(&mut self) {
        let Ok(bytes) = fs::read(&self.0) else { return };
        let Ok(child) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return;
        };
        let Some(pid) = child["pid"].as_u64().and_then(|id| i32::try_from(id).ok()) else {
            return;
        };
        if pid > 0
            && child["group_id"].as_i64() == Some(i64::from(pid))
            && unsafe { libc::getpgid(pid) } == pid
            && observed_process_identity(pid as u32).as_ref()
                == Some(&(
                    child["boot_identity"].as_str().unwrap_or_default().into(),
                    child["start_identity"].as_str().unwrap_or_default().into(),
                ))
        {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }
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
fn exit_proof(config: &Config) -> ExitPrEvidence {
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let payload: String = connection.query_row(
        "SELECT payload FROM evidence WHERE task_id='task' AND attempt_id='attempt-real' AND kind='exit_pr_lookup'",
        [], |row| row.get(0),
    ).unwrap();
    serde_json::from_str(&payload).unwrap()
}

#[cfg(unix)]
fn pause_proof(config: &Config) -> PausePrEvidence {
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let payload: String = connection.query_row(
        "SELECT payload FROM evidence WHERE task_id='task' AND attempt_id='attempt-real' AND kind='pause_pr_lookup'",
        [], |row| row.get(0),
    ).unwrap();
    serde_json::from_str(&payload).unwrap()
}

struct ExpectedIdentityReader {
    login: String,
    fail_target: bool,
    target_id: u64,
    head_id: u64,
}
impl PullRequestReader for ExpectedIdentityReader {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok(self.login.clone())
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<serde_json::Value>, LookupError> {
        Ok(vec![])
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<serde_json::Value, LookupError> {
        unreachable!()
    }
    fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError> {
        if name == "org/code" && self.fail_target {
            return Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            });
        }
        match name {
            "org/code" => Ok(self.target_id),
            "org/head" => Ok(self.head_id),
            _ => unreachable!(),
        }
    }
}

#[test]
fn expected_for_task_uses_selection_mapping_and_verified_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let (mut config, mut candidate) = configured(dir.path());
    config.mappings[0].allowed_pr_head_repository = "org/head".into();
    candidate.mapping = config.mappings[0].clone();
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    prepare_initial(&mut store, "task", "attempt-1").unwrap();
    let mut reader = ExpectedIdentityReader {
        login: "operator".into(),
        fail_target: false,
        target_id: 10,
        head_id: 20,
    };
    let expected = expected_for_task(&store, "task", &mut reader, "operator").unwrap();
    assert_eq!(
        (expected.repository_id, expected.head_repository_id),
        (10, 20)
    );
    assert_eq!(expected.task_branch, "luthor/task");
}

#[test]
fn expected_for_task_rejects_changed_login_and_repository_id_lookup_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    prepare_initial(&mut store, "task", "attempt-1").unwrap();
    let mut reader = ExpectedIdentityReader {
        login: "attacker".into(),
        fail_target: false,
        target_id: 10,
        head_id: 20,
    };
    assert!(matches!(
        expected_for_task(&store, "task", &mut reader, "attacker"),
        Err(ExpectedPrError::AuthorMismatch)
    ));
    let mut reader = ExpectedIdentityReader {
        login: "operator".into(),
        fail_target: true,
        target_id: 10,
        head_id: 20,
    };
    assert!(matches!(
        expected_for_task(&store, "task", &mut reader, "operator"),
        Err(ExpectedPrError::RepositoryLookup(_))
    ));
}

#[cfg(unix)]
#[derive(Default)]
struct ExitPr {
    reads: usize,
    fail: bool,
    matching: Option<(String, String)>,
    author: Option<String>,
}
#[cfg(unix)]
impl PullRequestReader for ExitPr {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok("operator".into())
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<serde_json::Value>, LookupError> {
        self.reads += 1;
        if self.fail {
            Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            })
        } else if let Some((issue_url, _)) = &self.matching {
            Ok(vec![
                serde_json::json!({"number": 42, "body": format!("Tracker-Issue: {issue_url}")}),
            ])
        } else {
            Ok(vec![])
        }
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<serde_json::Value, LookupError> {
        let Some((issue_url, branch)) = &self.matching else {
            unreachable!()
        };
        Ok(serde_json::json!({
            "id": 4242, "number": 42, "state": "open",
            "html_url": "https://github.com/org/code/pull/42",
            "body": format!("Tracker-Issue: {issue_url}"), "draft": true,
            "created_at": "2026-01-01T00:00:00Z",
            "base": {"repo": {"id": 10, "full_name": "org/code"}, "ref": "main"},
            "head": {"repo": {"id": 10, "full_name": "org/code"}, "ref": branch, "sha": "abc123"},
            "user": {"login": self.author.as_deref().unwrap_or("operator")}
        }))
    }
    fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError> {
        match name {
            "org/code" => Ok(10),
            "org/head" => Ok(20),
            _ => Err(LookupError {
                category: ErrorCategory::Malformed,
                code: "unexpected-repository",
                status: None,
            }),
        }
    }
}

#[cfg(unix)]
#[test]
fn natural_exit_matching_pr_is_proved_and_persisted() {
    let (_dir, mut config, mut store) = dispatched_fixture(7);
    config.capacity = 1;
    drop(store);
    store = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert!(store.ensure_dispatch_capacity().is_err());
    let selection = store.selection_evidence("task").unwrap().unwrap();
    let identity = store
        .worktree_record("task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url, identity.branch)),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(
        store.selection_evidence("task").unwrap().unwrap().candidate,
        1,
    );
    let result = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut projects,
        &mut prs,
    )
    .unwrap();
    assert!(
        matches!(
            result,
            Reconciliation::Completed {
                exit_code: Some(7),
                signal: None
            }
        ),
        "unexpected reconciliation: {result:?}; held reason: {:?}",
        store.held_reason("task").unwrap()
    );
    assert_eq!(
        store.task_phase("task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert_eq!(exit_proof(&config).status, PausePrStatus::Open);
    assert!(
        store
            .evidence_kinds("task")
            .unwrap()
            .contains(&"attempt_exit".into())
    );
    assert!(store.ensure_dispatch_capacity().is_ok());
    drop(store);
    let reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        reopened.task_phase("task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert!(reopened.ensure_dispatch_capacity().is_ok());
    assert_eq!(exit_proof(&config).status, PausePrStatus::Open);
    let output = luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap();
    let shown: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        shown["attempts"][0]["outcome"],
        "exit_code=Some(7);signal=None"
    );
    assert_eq!(prs.reads, 1);
}

#[cfg(unix)]
#[test]
fn missing_receipt_cannot_be_overridden_by_matching_open_pr() {
    let (_dir, mut config, mut store) = dispatched_fixture(0);
    config.capacity = 1;
    let receipt = receipt_path(&config);
    assert!(receipt.exists());
    fs::remove_file(&receipt).unwrap();

    let selection = store.selection_evidence("task").unwrap().unwrap();
    let identity = store
        .worktree_record("task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url, identity.branch)),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(
        store.selection_evidence("task").unwrap().unwrap().candidate,
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
        Reconciliation::Held { .. }
    ));
    assert_eq!(prs.reads, 0);
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(matches!(
        store.ensure_dispatch_capacity(),
        Err(StateError::Capacity { .. })
    ));
    let kinds = store.evidence_kinds("task").unwrap();
    assert!(!kinds.iter().any(|kind| kind == "verified_open_pr"));
    assert!(!kinds.iter().any(|kind| kind == "attempt_exit"));
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let outcome: Option<String> = connection
        .query_row(
            "SELECT outcome FROM attempts WHERE id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, None);

    drop(store);
    let mut reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    let mut startup_prs = ExitPr {
        matching: prs.matching.clone(),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(
        reopened
            .selection_evidence("task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    let startup =
        luthor::coordinator::startup_reconcile_all(&mut reopened, &mut projects, &mut startup_prs)
            .unwrap();
    assert!(matches!(
        startup.attempts[0].review,
        luthor::coordinator::AttemptReview::Held(_)
    ));
    assert_eq!(startup_prs.reads, 0);
    assert_eq!(
        reopened.task_phase("task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(reopened.reservation_count().unwrap(), 1);
    assert!(matches!(
        reopened.ensure_dispatch_capacity(),
        Err(StateError::Capacity { .. })
    ));
    assert!(
        !reopened
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

#[cfg(unix)]
#[test]
fn stopped_exit_matching_pr_is_proved_and_persisted() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    store.record_stop_intent("task", "attempt-real").unwrap();
    edit_receipt(&config, |receipt| {
        receipt.stop_signals = vec![libc::SIGTERM]
    });
    let selection = store.selection_evidence("task").unwrap().unwrap();
    let identity = store
        .worktree_record("task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url, identity.branch)),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(
        store.selection_evidence("task").unwrap().unwrap().candidate,
        1,
    );
    let result = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut projects,
        &mut prs,
    )
    .unwrap();
    assert_eq!(
        result,
        Reconciliation::Completed {
            exit_code: Some(7),
            signal: None
        }
    );
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt_path(&config)).unwrap()).unwrap();
    assert_eq!(receipt.stop_signals, vec![libc::SIGTERM]);
    assert_eq!(
        store.task_phase("task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert_eq!(pause_proof(&config).status, PausePrStatus::Open);
    let output = luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap();
    let shown: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        shown["attempts"][0]["outcome"],
        "exit_code=Some(7);signal=None"
    );
    assert_eq!(prs.reads, 1);
    drop(store);

    let mut reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        reopened.task_phase("task").unwrap().as_deref(),
        Some("pr_complete")
    );
    let output = luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap();
    let shown: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        shown["attempts"][0]["outcome"],
        "exit_code=Some(7);signal=None"
    );
    assert_eq!(pause_proof(&config).status, PausePrStatus::Open);
    assert!(matches!(
        prepare_resume(&mut reopened, "task", "attempt-next"),
        Err(SupervisorError::State(StateError::LaunchBlocked))
    ));
}

#[cfg(unix)]
#[test]
fn natural_exit_stale_assignee_is_held_despite_matching_open_pr() {
    let (_dir, mut config, mut store) = dispatched_fixture(7);
    config.capacity = 1;
    let selection = store.selection_evidence("task").unwrap().unwrap();
    let identity = store
        .worktree_record("task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url.clone(), identity.branch)),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(selection.candidate, 0);
    let result = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut projects,
        &mut prs,
    )
    .unwrap();
    assert!(matches!(result, Reconciliation::Held { .. }));
    assert!(
        store
            .held_reason("task")
            .unwrap()
            .unwrap()
            .contains("completion claim changed")
    );
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "verified_open_pr")
    );
    assert!(
        store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let outcome: Option<String> = connection
        .query_row(
            "SELECT outcome FROM attempts WHERE id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome.as_deref(), Some("exit_code=Some(7);signal=None"));
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert!(store.ensure_dispatch_capacity().is_err());
    assert_eq!(prs.reads, 1);
    assert_eq!(projects.1, 1);
}

#[cfg(unix)]
#[test]
fn stopped_exit_stale_assignee_is_held_despite_matching_open_pr() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    store.record_stop_intent("task", "attempt-real").unwrap();
    edit_receipt(&config, |receipt| {
        receipt.stop_signals = vec![libc::SIGTERM]
    });
    let selection = store.selection_evidence("task").unwrap().unwrap();
    let identity = store
        .worktree_record("task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url.clone(), identity.branch)),
        ..ExitPr::default()
    };
    let mut projects = OtherProject(selection.candidate, 0);
    let result = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut projects,
        &mut prs,
    )
    .unwrap();
    assert!(matches!(result, Reconciliation::Held { .. }));
    assert!(
        store
            .held_reason("task")
            .unwrap()
            .unwrap()
            .contains("completion claim changed")
    );
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "verified_open_pr")
    );
    assert!(
        store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let outcome: Option<String> = connection
        .query_row(
            "SELECT outcome FROM attempts WHERE id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome.as_deref(), Some("exit_code=Some(7);signal=None"));
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert!(store.ensure_dispatch_capacity().is_err());
    assert_eq!(prs.reads, 1);
    assert_eq!(projects.1, 1);
}

#[cfg(unix)]
struct OtherProject(Candidate, usize);
#[cfg(unix)]
impl ProjectReader for OtherProject {
    fn page(&mut self, _: &str, _: Option<&str>) -> Result<Page<ProjectItem>, ProjectReadError> {
        let c = &self.0;
        Ok(Page {
            items: vec![ProjectItem {
                item_id: c.item_id.clone(),
                issue_node_id: c.issue_node_id.clone(),
                repository: c.repository.clone(),
                tracker_repo_id: c.tracker_repo_id.clone(),
                issue_number: c.issue_number,
                fields: vec![],
                unsupported_fields: vec![],
            }],
            has_next_page: false,
            end_cursor: None,
        })
    }
    fn issue(&mut self, _: &ProjectItem) -> Result<Issue, ProjectReadError> {
        self.1 += 1;
        let c = &self.0;
        Ok(Issue {
            node_id: c.issue_node_id.clone(),
            repository: c.repository.clone(),
            tracker_repo_id: c.tracker_repo_id.clone(),
            number: c.issue_number,
            url: c.issue_url.clone(),
            state: "open".into(),
            assignees: if self.1 == 1 {
                vec![]
            } else {
                vec!["operator".into()]
            },
            labels: vec!["ready".into()],
            milestone: None,
            milestone_id: None,
            observed_at_unix_secs: 1,
        })
    }
}
#[cfg(unix)]
struct OtherWriter;
#[cfg(unix)]
impl AssignmentWriter for OtherWriter {
    fn assign(&mut self, _: &str, _: u64, _: &str) -> Result<(), AssignmentError> {
        Ok(())
    }
}
#[cfg(unix)]
#[derive(Default)]
struct OtherLauncher(usize);
#[cfg(unix)]
impl SupervisorLauncher for OtherLauncher {
    fn launch(
        &mut self,
        _: &mut StateStore,
        _: &luthor::supervisor::LaunchPlan,
    ) -> Result<(), SupervisorError> {
        self.0 += 1;
        Ok(())
    }
}
#[cfg(unix)]
#[derive(Default)]
struct OtherIds(usize);
#[cfg(unix)]
impl IdCreator for OtherIds {
    fn create(&mut self) -> Result<String, std::io::Error> {
        self.0 += 1;
        Ok(self.0.to_string())
    }
}

#[cfg(unix)]
#[test]
fn natural_exit_seven_attends_and_scheduler_dispatches_only_other_issue() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let mut prs = ExitPr::default();
    let (_, mut other) = configured(config.state_root.parent().unwrap());
    other.item_id = "other-item".into();
    other.issue_node_id = "other-issue".into();
    other.issue_number = 8;
    other.issue_url = "https://github.com/org/tracker/issues/8".into();
    let mut project = OtherProject(other.clone(), 0);
    let mut writer = OtherWriter;
    let mut launcher = OtherLauncher::default();
    let mut ids = OtherIds::default();
    let report = schedule_candidates(
        &mut store,
        vec![other.clone()],
        ScheduleDependencies {
            config: &config,
            config_revision: "rev",
            projects: &mut project,
            prs: &mut prs,
            assignments: &mut writer,
            launcher: &mut launcher,
            ids: &mut ids,
        },
    )
    .unwrap();
    assert_eq!(prs.reads, 4); // one fresh exit read, then claim and prelaunch reads
    assert!(matches!(
        report.startup.attempts[0].review,
        luthor::coordinator::AttemptReview::Completed(Reconciliation::Completed {
            exit_code: Some(7),
            signal: None
        })
    ));
    assert_eq!(
        store.task_phase("task").unwrap().as_deref(),
        Some("attention")
    );
    let proof = exit_proof(&config);
    assert_eq!(proof.status, PausePrStatus::Absent);
    assert!(proof.observed_at_unix_secs > 0);
    assert!(
        store
            .evidence_kinds("task")
            .unwrap()
            .contains(&"attention_reason".into())
    );
    assert_eq!(
        store.latest_attempt("task").unwrap().as_deref(),
        Some("attempt-real")
    );
    assert_eq!(store.reservation_count().unwrap(), 1); // only the other issue
    assert_eq!(report.launched.len(), 1);
    assert_eq!(launcher.0, 1);
    assert_eq!(store.pending_attempts().unwrap().len(), 1); // other task awaits worker
    assert!(
        store
            .existing_issue(&other.tracker_repo_id, &other.issue_node_id)
            .unwrap()
    );
    let again = schedule_candidates(
        &mut store,
        vec![other],
        ScheduleDependencies {
            config: &config,
            config_revision: "rev",
            projects: &mut project,
            prs: &mut prs,
            assignments: &mut writer,
            launcher: &mut launcher,
            ids: &mut ids,
        },
    )
    .unwrap();
    assert!(again.launched.is_empty());
    assert_eq!(launcher.0, 1);
    assert_eq!(
        store.latest_attempt("task").unwrap().as_deref(),
        Some("attempt-real")
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct StopRaceCleanup(std::path::PathBuf);

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
fn natural_stop_race_fixture() -> (
    tempfile::TempDir,
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
    StopRaceCleanup,
) {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    let cleanup = StopRaceCleanup(config.state_root.clone());
    let release = config.state_root.join("release-natural-exit");
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
    assert!(store.stop_intent("task", "attempt-real").unwrap().is_none());
    let child: serde_json::Value = serde_json::from_slice(
        &fs::read(config.state_root.join("attempts/attempt-real.child.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        unsafe { libc::kill(-(child["pid"].as_i64().unwrap() as i32), 0) },
        0
    );
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(matches!(
        store.ensure_dispatch_capacity(),
        Err(StateError::Capacity { .. })
    ));
    (dir, config, store, plan, cleanup)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn release_natural_exit(config: &Config) -> Vec<u8> {
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
fn assert_natural_stop_accounted(
    config: &Config,
    mut store: StateStore,
    plan: &luthor::supervisor::LaunchPlan,
    receipt: &[u8],
) {
    assert_eq!(fs::read(receipt_path(config)).unwrap(), receipt);
    assert!(store.stop_intent("task", "attempt-real").unwrap().is_some());
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(matches!(
        store.ensure_dispatch_capacity(),
        Err(StateError::Capacity { .. })
    ));
    let mut prs = ExitPr::default();
    let mut projects = OtherProject(
        store.selection_evidence("task").unwrap().unwrap().candidate,
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
        store.task_phase("task").unwrap().as_deref(),
        Some("attention")
    );
    assert_eq!(store.reservation_count().unwrap(), 0);
    store.ensure_dispatch_capacity().unwrap();
    assert!(matches!(
        prepare_resume(&mut store, "task", "attempt-next"),
        Err(SupervisorError::State(StateError::LaunchBlocked))
    ));
    drop(store);

    let config_path = config.state_root.join("config.json");
    fs::write(&config_path, serde_json::to_vec(config).unwrap()).unwrap();
    let resume = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(["resume", "task", "--config"])
        .arg(&config_path)
        .arg("--execute")
        .output()
        .unwrap();
    assert!(!resume.status.success());
    assert!(String::from_utf8_lossy(&resume.stderr).contains("resume held: task is not resumable"));

    let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
    let view = |args: &[&str]| -> serde_json::Value {
        let output = luthor::cli::execute(
            &config.state_root,
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap();
        serde_json::from_str(&output).unwrap()
    };
    let status = view(&["status"]);
    let shown = view(&["show", "task"]);
    assert_eq!(status["tasks"].as_array().unwrap().len(), 1);
    let task = &status["tasks"][0];
    assert_eq!(task["phase"], "attention");
    assert_eq!(shown["phase"], task["phase"]);
    assert_eq!(task["latest_attempt_id"], "attempt-real");
    assert_eq!(shown["latest_attempt_id"], task["latest_attempt_id"]);
    assert_eq!(task["latest_attempt_lifecycle"], "completed");
    assert_eq!(
        shown["attempts"][0]["lifecycle"],
        task["latest_attempt_lifecycle"]
    );
    assert_eq!(
        task["latest_attempt_outcome"],
        "exit_code=Some(7);signal=None"
    );
    assert_eq!(
        shown["latest_attempt_outcome"],
        task["latest_attempt_outcome"]
    );
    assert_eq!(task["reserved_slot"], false);
    assert_eq!(shown["reserved_slot"], task["reserved_slot"]);
    assert_eq!(status["capacity"]["reserved"], 0);
    assert_eq!(shown["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(shown["attempts"][0]["reservation"], "released");
    assert_eq!(shown["session"], plan.session_id);
    assert_eq!(
        shown["worktree"],
        serde_json::to_value(&plan.expected_worktree).unwrap()
    );
    let claims: Vec<_> = shown["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "claim_verified")
        .collect();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0]["detail"], config.assignment_login);
    let launch: luthor::supervisor::LaunchPlan =
        serde_json::from_str(&store.launch_intent("attempt-real").unwrap().unwrap()).unwrap();
    assert_eq!(&launch, plan);
    assert!(!store.has_attempt("task", "attempt-next").unwrap());
    let kinds = store.evidence_kinds("task").unwrap();
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "attempt_exit").count(),
        1
    );
    assert!(!kinds.contains(&"independent_stop_signal".into()));
    assert!(!kinds.contains(&"independent_stop_decision".into()));
    assert_eq!(fs::read(receipt_path(config)).unwrap(), receipt);
    let mut projects = OtherProject(
        store.selection_evidence("task").unwrap().unwrap().candidate,
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
        Reconciliation::Completed { .. }
    ));
    assert_eq!(prs.reads, 1);
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert_eq!(
        store.task_phase("task").unwrap().as_deref(),
        Some("attention")
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn pause_after_natural_exit_preserves_natural_attention_path() {
    let (_dir, config, mut store, plan, _cleanup) = natural_stop_race_fixture();
    let receipt = release_natural_exit(&config);
    assert!(store.stop_intent("task", "attempt-real").unwrap().is_none());
    assert!(matches!(
        request_stop(&mut store, "task", "attempt-real"),
        Ok(()) | Err(SupervisorError::StopUnavailable)
    ));
    assert_natural_stop_accounted(&config, store, &plan, &receipt);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn pause_intent_before_natural_exit_before_signal_preserves_attention_path() {
    let (_dir, config, mut store, plan, _cleanup) = natural_stop_race_fixture();
    let stop = luthor::supervisor::prepare_stop(&mut store, "task", "attempt-real").unwrap();
    let db = rusqlite::Connection::open_with_flags(
        config.state_root.join("state.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let intent: String = db.query_row(
        "SELECT detail FROM intents WHERE kind='stop' AND task_id='task' AND attempt_id='attempt-real'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&intent).unwrap(),
        serde_json::json!({"task_id":"task","attempt_id":"attempt-real"})
    );
    assert!(!receipt_path(&config).exists());
    let child: serde_json::Value = serde_json::from_slice(
        &fs::read(config.state_root.join("attempts/attempt-real.child.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        unsafe { libc::kill(-(child["pid"].as_i64().unwrap() as i32), 0) },
        0
    );
    let receipt = release_natural_exit(&config);
    assert!(matches!(
        stop.finish(),
        Ok(()) | Err(SupervisorError::StopUnavailable)
    ));
    assert_natural_stop_accounted(&config, store, &plan, &receipt);
}

#[cfg(unix)]
#[test]
fn natural_exit_pr_error_keeps_held_slot_and_evidence() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let mut prs = ExitPr {
        fail: true,
        ..Default::default()
    };
    let mut projects = OtherProject(
        store.selection_evidence("task").unwrap().unwrap().candidate,
        1,
    );
    let report =
        luthor::coordinator::startup_reconcile_all(&mut store, &mut projects, &mut prs).unwrap();
    assert!(matches!(
        report.attempts[0].review,
        luthor::coordinator::AttemptReview::Held(_)
    ));
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert!(matches!(
        store.ensure_dispatch_capacity(),
        Err(StateError::Capacity { .. })
    ));
    let proof = exit_proof(&config);
    assert!(matches!(
        proof.status,
        PausePrStatus::Error {
            category: ErrorCategory::Transport,
            ..
        }
    ));
    assert!(store.pending_attempts().unwrap().is_empty());
    assert_eq!(prs.reads, 1);
    let outcome_before: String =
        rusqlite::Connection::open(config.state_root.join("state.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT outcome FROM attempts WHERE id='attempt-real'",
                [],
                |row| row.get(0),
            )
            .unwrap();

    let mut recovered_prs = ExitPr::default();
    let mut projects = OtherProject(
        store.selection_evidence("task").unwrap().unwrap().candidate,
        1,
    );
    assert!(matches!(
        luthor::coordinator::reconcile_with_pr(
            &mut store,
            "task",
            "attempt-real",
            &mut projects,
            &mut recovered_prs
        )
        .unwrap(),
        Reconciliation::Completed {
            exit_code: Some(7),
            signal: None
        }
    ));
    assert_eq!(recovered_prs.reads, 1);
    assert_eq!(
        store.task_phase("task").unwrap().as_deref(),
        Some("attention")
    );
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert_eq!(
        store.latest_attempt("task").unwrap().as_deref(),
        Some("attempt-real")
    );
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let records: Vec<String> = connection
        .prepare("SELECT payload FROM evidence WHERE task_id='task' AND attempt_id='attempt-real' AND kind='exit_pr_lookup' ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let proofs: Vec<ExitPrEvidence> = records
        .iter()
        .map(|record| serde_json::from_str(record).unwrap())
        .collect();
    assert_eq!(proofs.len(), 2);
    assert!(matches!(proofs[0].status, PausePrStatus::Error { .. }));
    assert_eq!(proofs[1].status, PausePrStatus::Absent);
    let outcome_after: String = connection
        .query_row(
            "SELECT outcome FROM attempts WHERE id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome_after, outcome_before);

    drop(store);
    let mut reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    let mut startup_prs = ExitPr::default();
    let mut projects = OtherProject(
        reopened
            .selection_evidence("task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    let startup =
        luthor::coordinator::startup_reconcile_all(&mut reopened, &mut projects, &mut startup_prs)
            .unwrap();
    assert!(startup.attempts.is_empty());
    assert_eq!(startup_prs.reads, 0);
    assert_eq!(
        reopened.task_phase("task").unwrap().as_deref(),
        Some("attention")
    );
}

#[cfg(unix)]
#[test]
fn uncertain_child_group_never_reads_pr_or_frees_capacity() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    edit_receipt(&config, |receipt| receipt.child_pid = std::process::id());
    let mut prs = ExitPr::default();
    let mut projects = OtherProject(
        store.selection_evidence("task").unwrap().unwrap().candidate,
        1,
    );
    let report =
        luthor::coordinator::startup_reconcile_all(&mut store, &mut projects, &mut prs).unwrap();
    assert!(matches!(
        report.attempts[0].review,
        luthor::coordinator::AttemptReview::Held(_)
    ));
    assert_eq!(prs.reads, 0);
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(matches!(
        store.ensure_dispatch_capacity(),
        Err(StateError::Capacity { .. })
    ));
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
fn live_tracked_descendant_holds_valid_receipt_reconciliation() {
    let (_dir, _config, mut store) = dispatched_fixture(0);
    let (boot_identity, start_identity) = test_process_identity(std::process::id());
    let tracked = serde_json::json!({
        "pid": std::process::id(),
        "boot_identity": boot_identity,
        "start_identity": start_identity,
    });
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "tracked_descendant",
            &tracked.to_string(),
        )
        .unwrap();

    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "tracked descendant is alive"
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn escaped_tracked_descendant_holds_valid_receipt_until_reaped() {
    use std::os::unix::process::CommandExt;

    struct EscapedWorker {
        child: std::process::Child,
        identity: (String, String),
    }

    impl Drop for EscapedWorker {
        fn drop(&mut self) {
            let pid = self.child.id() as i32;
            if unsafe { libc::getpgid(pid) } == pid
                && observed_process_identity(self.child.id()).as_ref() == Some(&self.identity)
            {
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            let _ = self.child.wait();
        }
    }

    let (_dir, config, mut store) = dispatched_fixture(7);
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt_path(&config)).unwrap()).unwrap();
    assert_eq!(receipt.exit_code, Some(7));
    assert_eq!(unsafe { libc::kill(-(receipt.child_pid as i32), 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );

    let mut command = Command::new("/bin/sleep");
    command
        .arg("10")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    let pid = child.id();
    let (boot_identity, start_identity) = test_process_identity(pid);
    let identity = (boot_identity.clone(), start_identity.clone());
    let mut worker = EscapedWorker { child, identity };
    assert_eq!(unsafe { libc::getpgid(pid as i32) }, pid as i32);
    let tracked = serde_json::json!({
        "pid": pid,
        "boot_identity": boot_identity,
        "start_identity": start_identity,
    });
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "tracked_descendant",
            &tracked.to_string(),
        )
        .unwrap();
    assert_eq!(
        observed_process_identity(pid).as_ref(),
        Some(&(boot_identity.clone(), start_identity.clone()))
    );
    assert!(worker.child.try_wait().unwrap().is_none());
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "tracked descendant is alive"
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert_ne!(
        store.task_phase("task").unwrap().as_deref(),
        Some("completed")
    );
    assert!(matches!(
        store.ensure_dispatch_capacity(),
        Err(StateError::Capacity { .. })
    ));
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );

    assert_eq!(unsafe { libc::getpgid(pid as i32) }, pid as i32);
    assert_eq!(
        observed_process_identity(pid),
        Some((boot_identity, start_identity))
    );
    assert_eq!(unsafe { libc::kill(-(pid as i32), libc::SIGKILL) }, 0);
    let status = worker.child.wait().unwrap();
    assert!(!status.success());
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(7),
            signal: None
        }
    ));
    assert_eq!(store.reservation_count().unwrap(), 0);
}

#[cfg(unix)]
#[test]
fn malformed_tracked_identity_holds_valid_receipt_reconciliation() {
    let (_dir, _config, mut store) = dispatched_fixture(0);
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "tracked_descendant",
            r#"{"pid":0,"boot_identity":"boot","start_identity":"start"}"#,
        )
        .unwrap();

    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "invalid tracked descendant identity"
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

#[test]
fn malformed_tracked_descendant_evidence_holds_missing_receipt_attempt() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    fs::remove_file(receipt_path(&config)).unwrap();
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "tracked_descendant",
            "{malformed",
        )
        .unwrap();

    assert!(matches!(
        inspect_recovery_quiescence(&store, "task", "attempt-real").unwrap(),
        RecoveryInspection::Held("invalid tracked descendant identity")
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "invalid tracked descendant identity"
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

#[cfg(unix)]
#[test]
fn live_tracked_descendant_prevents_missing_receipt_absence_reconciliation() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    fs::remove_file(receipt_path(&config)).unwrap();
    let (boot_identity, start_identity) = test_process_identity(std::process::id());
    let tracked = serde_json::json!({
        "pid": std::process::id(),
        "boot_identity": boot_identity,
        "start_identity": start_identity,
    });
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "tracked_descendant",
            &tracked.to_string(),
        )
        .unwrap();

    assert!(matches!(
        inspect_recovery_quiescence(&store, "task", "attempt-real").unwrap(),
        RecoveryInspection::Held("registered processes may still be live")
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason != "receipt missing; registered processes absent; operator recovery required"
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

#[cfg(unix)]
#[test]
fn reaped_supervisor_with_removed_receipt_proves_recovery_quiescence() {
    let (_dir, config, store) = dispatched_fixture(7);
    let payloads = store
        .evidence_payloads("task", "attempt-real", "supervisor_ready")
        .unwrap();
    assert_eq!(payloads.len(), 1);
    let identity: serde_json::Value = serde_json::from_str(&payloads[0]).unwrap();
    let supervisor_pid = identity["pid"].as_u64().expect("supervisor PID");
    let supervisor_pid = libc::pid_t::try_from(supervisor_pid).unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(supervisor_pid, &mut status, libc::WNOHANG) };
        if result == supervisor_pid {
            break;
        }
        let error = std::io::Error::last_os_error();
        assert_eq!(result, 0, "waitpid failed: {error}");
        assert!(Instant::now() < deadline, "supervisor was not reaped");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(libc::WIFEXITED(status), "supervisor did not exit normally");
    fs::remove_file(receipt_path(&config)).unwrap();

    assert_eq!(
        inspect_recovery_quiescence(&store, "task", "attempt-real").unwrap(),
        RecoveryInspection::Quiescent
    );
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

#[cfg(unix)]
#[test]
fn resumed_missing_receipt_accepts_attempt_snapshot_descending_from_original() {
    let (_dir, config, mut store, initial) = paused_fixture();
    let worktree = &initial.worktree;
    fs::write(worktree.join("recovery-advance"), "committed").unwrap();
    git(worktree, &["add", "recovery-advance"]);
    git(worktree, &["commit", "-m", "advance before resume"]);

    let resumed = prepare_resume(&mut store, "task", "attempt-next").unwrap();
    assert_ne!(
        resumed.expected_worktree.head,
        initial.expected_worktree.head
    );
    execute_with_binary(
        &mut store,
        &resumed,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
    )
    .unwrap();
    let receipt = config.state_root.join("attempts/attempt-next.receipt.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !receipt.exists() {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(receipt.exists(), "resumed worker did not finish");

    let payload = store
        .evidence_payloads("task", "attempt-next", "supervisor_ready")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if result == pid {
            break;
        }
        assert_eq!(
            result,
            0,
            "waitpid failed: {}",
            std::io::Error::last_os_error()
        );
        assert!(
            Instant::now() < deadline,
            "resumed supervisor was not reaped"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(libc::WIFEXITED(status));
    fs::remove_file(receipt).unwrap();
    assert_eq!(
        inspect_recovery_quiescence(&store, "task", "attempt-next").unwrap(),
        RecoveryInspection::Quiescent
    );

    git(
        worktree,
        &["reset", "--hard", &initial.expected_worktree.head],
    );
    assert!(matches!(
        inspect_recovery_quiescence(&store, "task", "attempt-next").unwrap(),
        RecoveryInspection::Held("worktree snapshot mismatch")
    ));
    git(worktree, &["checkout", "--orphan", "replacement"]);
    git(worktree, &["rm", "-rf", "."]);
    fs::write(worktree.join("replacement"), "unrelated history").unwrap();
    git(worktree, &["add", "replacement"]);
    git(worktree, &["commit", "-m", "unrelated replacement history"]);
    assert!(matches!(
        inspect_recovery_quiescence(&store, "task", "attempt-next").unwrap(),
        RecoveryInspection::Held(_)
    ));
}

#[cfg(unix)]
#[test]
fn operator_recovery_pr_lookup_failure_keeps_slot_reserved() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = store
        .evidence_payloads("task", "attempt-real", "supervisor_ready")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    fs::remove_file(receipt_path(&config)).unwrap();
    let selection = store.selection_evidence("task").unwrap().unwrap();
    let mut projects = OtherProject(selection.candidate, 1);
    let mut prs = ExitPr {
        fail: true,
        ..ExitPr::default()
    };
    assert!(matches!(
        operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "receipt lost",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        luthor::coordinator::RecoveryResult::Held(_)
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert_eq!(prs.reads, 1);
}

#[cfg(unix)]
#[test]
fn operator_recovery_absent_pr_records_telemetry_loss_and_releases_slot() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = store
        .evidence_payloads("task", "attempt-real", "supervisor_ready")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    fs::remove_file(receipt_path(&config)).unwrap();
    let selection = store.selection_evidence("task").unwrap().unwrap();
    let mut projects = OtherProject(selection.candidate, 1);
    let mut prs = ExitPr::default();
    assert_eq!(
        operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "receipt lost",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        luthor::coordinator::RecoveryResult::RecoveredHeld
    );
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert_eq!(prs.reads, 1);
    assert!(
        store
            .evidence_kinds("task")
            .unwrap()
            .contains(&"telemetry_lost".into())
    );
    assert!(
        store
            .evidence_kinds("task")
            .unwrap()
            .contains(&"exit_pr_lookup".into())
    );
    assert!(
        !store
            .evidence_kinds("task")
            .unwrap()
            .contains(&"attempt_exit".into())
    );
    let db = rusqlite::Connection::open_with_flags(
        config.state_root.join("state.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (lifecycle, outcome): (String, Option<String>) = db
        .query_row(
            "SELECT lifecycle, outcome FROM attempts WHERE task_id='task' AND id='attempt-real'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(lifecycle, "telemetry_lost");
    assert_eq!(outcome, None);
    drop(db);
    drop(store);
    let reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        reopened.task_phase("task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(reopened.reservation_count().unwrap(), 0);
    assert_eq!(
        reopened.pending_attempts().unwrap(),
        vec![("task".to_owned(), "attempt-real".to_owned())]
    );
    assert!(reopened.ensure_dispatch_capacity().is_err());
    assert!(
        reopened
            .evidence_kinds("task")
            .unwrap()
            .contains(&"telemetry_lost".into())
    );
    assert!(
        reopened
            .evidence_kinds("task")
            .unwrap()
            .contains(&"exit_pr_lookup".into())
    );
    assert!(
        !reopened
            .evidence_kinds("task")
            .unwrap()
            .contains(&"attempt_exit".into())
    );
}

#[cfg(unix)]
#[test]
fn operator_recovery_matching_pr_completes_task_and_persists_proof() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = store
        .evidence_payloads("task", "attempt-real", "supervisor_ready")
        .unwrap()
        .remove(0);
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    fs::remove_file(receipt_path(&config)).unwrap();
    let selection = store.selection_evidence("task").unwrap().unwrap();
    let branch = store
        .worktree_record("task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap()
        .branch;
    let mut projects = OtherProject(selection.candidate.clone(), 1);
    let mut prs = ExitPr {
        matching: Some((selection.candidate.issue_url.clone(), branch)),
        ..ExitPr::default()
    };
    assert_eq!(
        operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "receipt lost",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        luthor::coordinator::RecoveryResult::RecoveredPrComplete { pr_id: 4242 }
    );
    assert_eq!(
        store.task_phase("task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert_eq!(store.reservation_count().unwrap(), 0);
    let kinds = store.evidence_kinds("task").unwrap();
    for kind in ["telemetry_lost", "exit_pr_lookup", "verified_open_pr"] {
        assert_eq!(
            kinds.iter().filter(|seen| *seen == kind).count(),
            1,
            "{kind}"
        );
    }
    assert!(!kinds.iter().any(|kind| kind == "attempt_exit"));
    drop(store);
    let reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        reopened.task_phase("task").unwrap().as_deref(),
        Some("pr_complete")
    );
    assert_eq!(
        reopened
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .filter(|kind| *kind == "verified_open_pr")
            .count(),
        1
    );
    assert!(reopened.pending_attempts().unwrap().is_empty());
    assert_eq!(
        reopened.task_phase("task").unwrap().as_deref(),
        Some("pr_complete")
    );
    reopened.ensure_dispatch_capacity().unwrap();
    let db = rusqlite::Connection::open_with_flags(
        config.state_root.join("state.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let outcome: Option<String> = db
        .query_row(
            "SELECT outcome FROM attempts WHERE task_id='task' AND id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, None);
    assert!(
        !reopened
            .evidence_kinds("task")
            .unwrap()
            .contains(&"attempt_exit".to_owned())
    );
}

#[cfg(unix)]
#[test]
fn operator_recovery_release_failure_rolls_back_for_absent_and_matching_pr() {
    for matching_pr in [false, true] {
        let (_dir, config, mut store) = dispatched_fixture(7);
        let payload = store
            .evidence_payloads("task", "attempt-real", "supervisor_ready")
            .unwrap()
            .remove(0);
        let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
            .as_u64()
            .unwrap() as libc::pid_t;
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status), "supervisor did not exit normally");
        fs::remove_file(receipt_path(&config)).unwrap();

        let selection = store.selection_evidence("task").unwrap().unwrap();
        let branch = store
            .worktree_record("task")
            .unwrap()
            .unwrap()
            .identity
            .unwrap()
            .branch;
        let mut projects = OtherProject(selection.candidate.clone(), 1);
        let mut prs = ExitPr {
            matching: matching_pr.then_some((selection.candidate.issue_url, branch)),
            ..ExitPr::default()
        };
        let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER reject_release BEFORE UPDATE OF status ON reservations \
             WHEN NEW.status='released' AND OLD.status='reserved' \
             BEGIN SELECT RAISE(ABORT,'injected release failure'); END;",
        )
        .unwrap();

        let result = operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "receipt lost",
            &mut projects,
            &mut prs,
        );
        assert!(result.is_err(), "release trigger must abort recovery");
        assert_eq!(store.reservation_count().unwrap(), 1);
        assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
        let kinds = store.evidence_kinds("task").unwrap();
        for kind in ["telemetry_lost", "exit_pr_lookup", "verified_open_pr"] {
            assert!(!kinds.iter().any(|seen| seen == kind), "unexpected {kind}");
        }
        assert!(!kinds.iter().any(|kind| kind == "attempt_exit"));
        let (lifecycle, outcome): (String, Option<String>) = db
            .query_row(
                "SELECT lifecycle, outcome FROM attempts WHERE task_id='task' AND id='attempt-real'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(lifecycle, "launch_intended");
        assert_eq!(outcome, None);
        drop(db);
        drop(store);

        let reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
        assert_eq!(reopened.reservation_count().unwrap(), 1);
        assert_eq!(
            reopened.task_phase("task").unwrap().as_deref(),
            Some("held")
        );
        assert!(reopened.evidence_kinds("task").unwrap().iter().all(|kind| {
            ![
                "telemetry_lost",
                "exit_pr_lookup",
                "verified_open_pr",
                "attempt_exit",
            ]
            .contains(&kind.as_str())
        }));
        let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
        let (lifecycle, outcome): (String, Option<String>) = db
            .query_row(
                "SELECT lifecycle, outcome FROM attempts WHERE task_id='task' AND id='attempt-real'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(lifecycle, "launch_intended");
        assert_eq!(outcome, None);
    }
}

#[cfg(unix)]
#[test]
fn operator_recovery_does_not_count_nonmatching_pr() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = store
        .evidence_payloads("task", "attempt-real", "supervisor_ready")
        .unwrap()
        .remove(0);
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    fs::remove_file(receipt_path(&config)).unwrap();
    let selection = store.selection_evidence("task").unwrap().unwrap();
    let branch = store
        .worktree_record("task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap()
        .branch;
    let mut projects = OtherProject(selection.candidate.clone(), 1);
    let mut prs = ExitPr {
        matching: Some(("https://github.com/org/tracker/issues/999".into(), branch)),
        ..ExitPr::default()
    };
    assert_eq!(
        operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "receipt lost",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        luthor::coordinator::RecoveryResult::RecoveredHeld
    );
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
    assert_eq!(
        store
            .evidence_kinds("task")
            .unwrap()
            .iter()
            .filter(|kind| *kind == "verified_open_pr")
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn operator_recovery_rejects_wrong_pr_head_and_author() {
    for wrong_head in [true, false] {
        let (_dir, config, mut store) = dispatched_fixture(7);
        let payload = store
            .evidence_payloads("task", "attempt-real", "supervisor_ready")
            .unwrap()
            .remove(0);
        let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
            .as_u64()
            .unwrap() as libc::pid_t;
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        fs::remove_file(receipt_path(&config)).unwrap();
        let selection = store.selection_evidence("task").unwrap().unwrap();
        let branch = store
            .worktree_record("task")
            .unwrap()
            .unwrap()
            .identity
            .unwrap()
            .branch;
        let mut projects = OtherProject(selection.candidate.clone(), 1);
        let mut prs = ExitPr {
            matching: Some((
                selection.candidate.issue_url,
                if wrong_head {
                    "wrong-branch".into()
                } else {
                    branch
                },
            )),
            author: (!wrong_head).then(|| "attacker".into()),
            ..ExitPr::default()
        };
        assert!(matches!(
            operator_recover_missing_receipt(
                &mut store,
                "task",
                "attempt-real",
                "operator",
                "receipt lost",
                &mut projects,
                &mut prs
            )
            .unwrap(),
            luthor::coordinator::RecoveryResult::Held(_)
        ));
        assert_eq!(store.reservation_count().unwrap(), 1);
        assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
        let kinds = store.evidence_kinds("task").unwrap();
        assert!(
            !kinds
                .iter()
                .any(|kind| kind == "telemetry_lost" || kind == "verified_open_pr")
        );
    }
}

#[cfg(unix)]
#[test]
fn operator_recovery_stale_claim_keeps_slot_reserved_without_pr_lookup() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let payload = store
        .evidence_payloads("task", "attempt-real", "supervisor_ready")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["pid"]
        .as_u64()
        .unwrap() as libc::pid_t;
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    fs::remove_file(receipt_path(&config)).unwrap();
    let selection = store.selection_evidence("task").unwrap().unwrap();
    let mut projects = OtherProject(selection.candidate, 0);
    let mut prs = ExitPr::default();
    assert!(matches!(
        operator_recover_missing_receipt(
            &mut store,
            "task",
            "attempt-real",
            "operator",
            "receipt lost",
            &mut projects,
            &mut prs
        )
        .unwrap(),
        luthor::coordinator::RecoveryResult::Held(_)
    ));
    assert_eq!(prs.reads, 0);
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
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
fn branch_switch_after_ready_blocks_release_and_holds_slot() {
    use std::{
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
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
    git(&plan.worktree, &["switch", "-c", "foreign"]);
    let registered = fs::read_to_string(attempts.join("attempt-real.child.json")).unwrap();
    store
        .record_evidence(
            "task",
            Some("attempt-real"),
            "child_registered",
            registered.trim(),
        )
        .unwrap();
    let (boot, start) = test_process_identity(child.id());
    let process = serde_json::json!({"pid":child.id(),"boot_identity":boot,"start_identity":start});
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
    child.stdin.take().unwrap().write_all(b"R").unwrap();
    assert!(!child.wait().unwrap().success());
    assert!(!marker.exists());
    assert!(!attempts.join("attempt-real.receipt.json").exists());
    assert_eq!(store.reservation_count().unwrap(), 1);
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
    #[cfg(target_os = "linux")]
    if std::env::var_os("LUTHOR_STOP_TEST_SUBREAPER").is_none() {
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("registered_shim_stays_gated_and_survives_supervisor_crash_after_release")
            .env("LUTHOR_STOP_TEST_SUBREAPER", "1")
            .status()
            .unwrap();
        assert!(
            status.success(),
            "isolated subreaper test process failed: {status}"
        );
        return;
    }
    #[cfg(target_os = "linux")]
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
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
    #[cfg(target_os = "linux")]
    let reaper = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let result = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
            if result == pid {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    });
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
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if marker.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    if !marker.exists() {
        let supervisor_status = supervisor.try_wait();
        let _ = supervisor.kill();
        let _ = supervisor.wait();
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        assert!(
            marker.exists(),
            "fake worker marker was not created before deadline; supervisor.try_wait() = {supervisor_status:?}"
        );
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
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { .. }
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
    request_stop(&mut store, "task", "attempt-real").unwrap_or_else(|error| {
        panic!(
            "request_stop failed: {error}; evidence_kinds={:?}",
            store.evidence_kinds("task").unwrap()
        )
    });
    #[cfg(target_os = "linux")]
    assert!(
        reaper.join().unwrap(),
        "adopted worker was not reaped within the bound"
    );
    assert!(!attempts.join("attempt-real.receipt.json").exists());
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
        Reconciliation::Held { reason } if reason == "live worker identity or reservation unverified"
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
    let (_dir, config, mut store) = running_worker(
        "#!/bin/sh\necho started\ntrap 'exit 0' INT\ntrap 'exit 0' TERM\nwhile :; do sleep 0.01; done\n",
    );
    request_stop(&mut store, "task", "attempt-real").unwrap();
    assert!(store.stop_intent("task", "attempt-real").unwrap().is_some());
    let receipt = stopped_receipt(&config);
    assert_eq!(receipt.stop_signals.first(), Some(&libc::SIGINT));
    assert!(receipt.stop_signals.len() <= 2);
    assert!(
        receipt
            .stop_signals
            .iter()
            .all(|signal| *signal != libc::SIGKILL)
    );
    assert!(
        receipt
            .stop_signals
            .windows(2)
            .all(|signals| { signals == [libc::SIGINT, libc::SIGTERM] })
    );
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
        running_worker("#!/bin/sh\necho started\ntrap '' INT TERM\nwhile :; do :; done\n");
    request_stop(&mut store, "task", "attempt-real").unwrap();
    let receipt = stopped_receipt(&config);
    assert_eq!(
        receipt.stop_signals,
        vec![libc::SIGINT, libc::SIGTERM, libc::SIGKILL]
    );
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
    paused_fixture_with_resume_prompt("Continue {task.issue_url} for {attempt.id}")
}

#[cfg(unix)]
fn paused_fixture_with_resume_prompt(
    resume_prompt: &str,
) -> (
    tempfile::TempDir,
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
) {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker_with_resume_prompt(&dir, resume_prompt);
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let receipt = config.state_root.join("attempts/attempt-real.receipt.json");
    for _ in 0..200 {
        if receipt.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(receipt.exists());
    let initial =
        serde_json::from_str(&store.launch_intent("attempt-real").unwrap().unwrap()).unwrap();
    store.record_stop_intent("task", "attempt-real").unwrap();
    edit_receipt(&config, |receipt| {
        receipt.stop_signals = vec![libc::SIGTERM]
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last_reason = String::from("worker process group is still running");
    loop {
        match reconcile_attempt(&mut store, "task", "attempt-real").unwrap() {
            Reconciliation::Completed { .. } => break,
            Reconciliation::Held { reason } => {
                assert_eq!(store.reservation_count().unwrap(), 1);
                last_reason = reason;
            }
            Reconciliation::Running => {
                assert_eq!(store.reservation_count().unwrap(), 1);
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker reconciliation remained uncertain: {last_reason}"
        );
        thread::sleep(Duration::from_millis(20));
    }
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
fn branch_switch_before_resume_does_not_reserve_or_launch() {
    let (_dir, config, mut store, initial) = paused_fixture();
    git(&initial.worktree, &["switch", "-c", "foreign"]);
    assert!(prepare_resume(&mut store, "task", "attempt-next").is_err());
    assert_eq!(store.reservation_count().unwrap(), 0);
    assert_eq!(
        store.latest_attempt("task").unwrap().as_deref(),
        Some("attempt-real")
    );
    assert!(store.launch_intent("attempt-next").unwrap().is_none());
    assert!(
        !config
            .state_root
            .join("attempts/attempt-next.child.json")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn committed_worker_resumes_same_session_and_worktree_after_verified_stop() {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, first, _) = prepared_fake_worker(&dir);
    fs::write(
        &first.executable,
        r#"#!/bin/sh
case "$*" in
  *attempt-real*)
    echo committed > committed-file
    git add committed-file
    git commit -m 'worker commit' >/dev/null || exit 2
    echo committed-and-running
    trap 'exit 0' INT TERM
    while :; do :; done
    ;;
  *attempt-next*)
    test "$(cat committed-file)" = committed || exit 3
    echo resumed-same-worktree
    ;;
  *) exit 4;;
esac
"#,
    )
    .unwrap();
    execute_with_binary(&mut store, &first, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let stdout = config.state_root.join("attempts/attempt-real.stdout.log");
    for _ in 0..200 {
        if fs::read_to_string(&stdout).is_ok_and(|s| s.contains("committed-and-running")) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        fs::read_to_string(&stdout)
            .unwrap()
            .contains("committed-and-running")
    );
    request_stop(&mut store, "task", "attempt-real").unwrap();
    assert!(!stopped_receipt(&config).stop_signals.is_empty());
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
    drop(store);

    let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
    let next = prepare_resume(&mut store, "task", "attempt-next").unwrap();
    assert_eq!(next.session_id, first.session_id);
    assert_eq!(next.worktree, first.worktree);
    assert_eq!(
        store.latest_attempt("task").unwrap().as_deref(),
        Some("attempt-next")
    );
    assert_ne!(next.expected_worktree.head, first.expected_worktree.head);
    execute_with_binary(&mut store, &next, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let receipt = config.state_root.join("attempts/attempt-next.receipt.json");
    for _ in 0..200 {
        if receipt.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert_eq!(
        receipt.exit_code,
        Some(0),
        "stdout: {}; stderr: {}",
        fs::read_to_string(&receipt.stdout_path).unwrap(),
        fs::read_to_string(&receipt.stderr_path).unwrap()
    );
    assert!(
        fs::read_to_string(receipt.stdout_path)
            .unwrap()
            .contains("resumed-same-worktree")
    );
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-next").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(0),
            ..
        }
    ));
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
fn minimal_resume_template_gets_interrupted_worktree_inspection_guidance() {
    let (_dir, config, mut store, initial) =
        paused_fixture_with_resume_prompt("Continue {attempt.id}");
    let plan = prepare_resume(&mut store, "task", "attempt-next").unwrap();
    let rendered = plan
        .args
        .windows(2)
        .find(|pair| pair[0] == "--prompt")
        .unwrap()[1]
        .as_str();
    assert!(rendered.starts_with("Continue attempt-next"));
    assert!(
        rendered
            .contains("inspect the files left in the worktree by the interrupted or canceled turn")
    );
    assert!(rendered.contains("Do not assume its transcript was restored"));
    assert!(rendered.contains("Tracker-Issue: https://github.com/org/tracker/issues/7"));
    assert!(rendered.contains("already claimed; do not reassign it"));
    assert_eq!(plan.attempt_id, "attempt-next");
    assert_eq!(plan.session_id, initial.session_id);
    assert_eq!(plan.session_id, "task");
    assert_eq!(
        store.latest_attempt("task").unwrap().as_deref(),
        Some("attempt-next")
    );
    assert!(config.state_root.exists());
}

#[cfg(unix)]
#[test]
fn resume_rejects_same_final_prompt_but_allows_distinct_rendered_attempt() {
    let (_dir, _config, mut store, _) =
        paused_fixture_with_resume_prompt("Continue {task.issue_url}");
    let first = prepare_resume(&mut store, "task", "attempt-next").unwrap();
    let first_prompt = first
        .args
        .windows(2)
        .find(|pair| pair[0] == "--prompt")
        .unwrap()[1]
        .clone();
    let latest = first.clone();
    assert!(matches!(
        ensure_distinct_resume_prompt(&first, &latest, &first.args),
        Err(SupervisorError::Conflict)
    ));

    let (_dir, _config, mut distinct_store, _) =
        paused_fixture_with_resume_prompt("Continue {task.issue_url} for {attempt.id}");
    let distinct = prepare_resume(&mut distinct_store, "task", "attempt-next").unwrap();
    let distinct_prompt = distinct
        .args
        .windows(2)
        .find(|pair| pair[0] == "--prompt")
        .unwrap()[1]
        .clone();
    assert_ne!(first_prompt, distinct_prompt);
    assert!(ensure_distinct_resume_prompt(&first, &first, &distinct.args).is_ok());
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

#[test]
fn initial_rejects_conflicting_session_and_cwd_arguments_before_reservation() {
    let alterations: &[&[&str]] = &[
        &["--session", "other-task"],
        &["--cwd", "/tmp/other"],
        &["--session=other-task"],
        &["--cwd=/tmp/other"],
    ];
    for (index, alteration) in alterations.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let (mut config, candidate) = configured(dir.path());
        config
            .initial
            .args
            .extend(alteration.iter().map(|arg| (*arg).into()));
        let mut store = StateStore::open(&config.state_root, 1).unwrap();
        claimed(&mut store, &config, &candidate, dir.path());

        let attempt = format!("attempt-invalid-{index}");
        assert!(prepare_initial(&mut store, "task", &attempt).is_err());
        assert_eq!(store.reservation_count().unwrap(), 0, "{alteration:?}");
        assert!(store.launch_intent(&attempt).unwrap().is_none());
    }
}
