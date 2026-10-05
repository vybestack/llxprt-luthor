use super::*;
use luthor::state::{journal, scheduling};

#[cfg(unix)]
pub(crate) fn direct_snapshot(root: &Path) -> WorktreeIdentity {
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
pub(crate) fn fake_plan(root: &Path, attempt: &str, code: i32) -> luthor::supervisor::LaunchPlan {
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
pub(crate) fn prepared_fake_worker(
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
pub(crate) fn prepared_fake_worker_with_resume_prompt(
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
pub(crate) fn dispatched_fixture(code: i32) -> (tempfile::TempDir, Config, StateStore) {
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
    supervisor_support::stop_views::completion::wait_for_fixture_exit(&config, &store);
    (dir, config, store)
}

#[cfg(unix)]
pub(crate) fn receipt_path(config: &Config) -> std::path::PathBuf {
    config.state_root.join("attempts/attempt-real.receipt.json")
}

#[cfg(unix)]
pub(crate) fn hold_slot(store: &mut StateStore) {
    assert!(matches!(
        reconcile_attempt(store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { .. }
    ));
    assert_eq!(scheduling::reservation_count(store).unwrap(), 1);
    assert!(
        !journal::evidence_kinds(store, "task")
            .unwrap()
            .contains(&"attempt_exit".into())
    );
}

#[cfg(unix)]
pub(crate) fn edit_receipt(
    config: &Config,
    change: impl FnOnce(&mut luthor::supervisor::ExitReceipt),
) {
    let path = receipt_path(config);
    let mut receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    change(&mut receipt);
    fs::write(path, serde_json::to_vec(&receipt).unwrap()).unwrap();
}

#[cfg(unix)]
pub(crate) fn exit_proof(config: &Config) -> ExitPrEvidence {
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let payload: String = connection.query_row(
        "SELECT payload FROM evidence WHERE task_id='task' AND attempt_id='attempt-real' AND kind='exit_pr_lookup'",
        [], |row| row.get(0),
    ).unwrap();
    serde_json::from_str(&payload).unwrap()
}

#[cfg(unix)]
pub(crate) fn pause_proof(config: &Config) -> PausePrEvidence {
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let payload: String = connection.query_row(
        "SELECT payload FROM evidence WHERE task_id='task' AND attempt_id='attempt-real' AND kind='pause_pr_lookup'",
        [], |row| row.get(0),
    ).unwrap();
    serde_json::from_str(&payload).unwrap()
}

#[cfg(unix)]
pub(crate) fn running_worker(script: &str) -> (tempfile::TempDir, Config, StateStore) {
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
pub(crate) fn stopped_receipt(config: &Config) -> luthor::supervisor::ExitReceipt {
    for _ in 0..200 {
        if let Ok(bytes) = fs::read(receipt_path(config)) {
            return serde_json::from_slice(&bytes).unwrap();
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("missing stopped worker receipt");
}
