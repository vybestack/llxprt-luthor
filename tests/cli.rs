use luthor::{
    cli::{CliError, execute},
    state::StateStore,
    supervisor::{ExitReceipt, LaunchPlan},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    dir: tempfile::TempDir,
    _lock: StateStore,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let lock = StateStore::open(dir.path(), 2).unwrap();
        Self { dir, _lock: lock }
    }
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn db(&self) -> Connection {
        Connection::open(self.root().join("state.sqlite3")).unwrap()
    }
    fn run(&self, args: &[&str]) -> Result<Value, CliError> {
        execute(
            self.root(),
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .map(|s| serde_json::from_str(&s).unwrap())
    }
    fn task(&self, id: &str) {
        self.db().execute("INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision)
            VALUES(?1,?2,?3,'org/tracker',7,'held','rev')",params![id,format!("repo-{id}"),format!("issue-{id}")]).unwrap();
    }
    fn attempt(&self, task: &str, id: &str, session: &str) {
        let db = self.db();
        db.execute(
            "INSERT INTO attempts(id,task_id,lifecycle) VALUES(?1,?2,'launch_intended')",
            [id, task],
        )
        .unwrap();
        db.execute(
            "INSERT INTO reservations(attempt_id,task_id,status) VALUES(?1,?2,'reserved')",
            [id, task],
        )
        .unwrap();
        let plan = LaunchPlan {
            task_id: task.into(),
            attempt_id: id.into(),
            session_id: session.into(),
            worktree: PathBuf::from("/worktree"),
            executable: PathBuf::from("/bin/worker"),
            args: vec!["--secret-prompt".into()],
            config_revision: "rev".into(),
        };
        db.execute(
            "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,'launch',?4)",
            params![
                format!("launch-{id}"),
                task,
                id,
                serde_json::to_string(&plan).unwrap()
            ],
        )
        .unwrap();
    }
    fn evidence(&self, task: &str, attempt: Option<&str>, kind: &str, payload: &str) {
        self.db()
            .execute(
                "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,?3,?4)",
                params![task, attempt, kind, payload],
            )
            .unwrap();
    }
    fn logs(&self, id: &str) -> (PathBuf, PathBuf) {
        let dir = self.root().join("attempts");
        fs::create_dir_all(&dir).unwrap();
        let stdout = dir.join(format!("{id}.stdout.log"));
        let stderr = dir.join(format!("{id}.stderr.log"));
        fs::write(&stdout, "hello").unwrap();
        fs::write(&stderr, "warning").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&stdout, fs::Permissions::from_mode(0o600)).unwrap();
            fs::set_permissions(&stderr, fs::Permissions::from_mode(0o600)).unwrap();
        }
        (stdout, stderr)
    }
    fn receipt(&self, task: &str, id: &str, stdout: PathBuf, stderr: PathBuf) {
        let receipt = ExitReceipt {
            attempt_id: id.into(),
            child_pid: 42,
            boot_identity: "boot".into(),
            child_start_identity: "start".into(),
            exit_code: Some(0),
            signal: None,
            stdout_path: stdout,
            stdout_bytes: 5,
            stderr_path: stderr,
            stderr_bytes: 7,
            stop_signals: vec![],
        };
        let payload = serde_json::to_string(&receipt).unwrap();
        fs::write(
            self.root()
                .join("attempts")
                .join(format!("{id}.receipt.json")),
            &payload,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                self.root()
                    .join("attempts")
                    .join(format!("{id}.receipt.json")),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        self.evidence(task, Some(id), "attempt_exit", &payload);
    }
}

#[test]
fn show_reports_events_issue_mapping_session_and_receipt_without_secrets() {
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "attempt", "persisted-session");
    let candidate = json!({"issue_url":"https://github.com/org/tracker/issues/7",
        "source":{"project_id":"project"},"mapping":{"code_repository":"org/code"}});
    f.evidence(
        "task",
        None,
        "selection",
        &json!({"candidate":candidate,"effective_config":{"secret":"do-not-show"}}).to_string(),
    );
    f.evidence("task", None, "held_reason", "needs inspection");
    let (stdout, stderr) = f.logs("attempt");
    f.receipt("task", "attempt", stdout, stderr);
    let shown = f.run(&["show", "task"]).unwrap();
    assert_eq!(shown["session"], "persisted-session");
    assert_eq!(shown["task"]["source"]["project_id"], "project");
    assert_eq!(shown["task"]["mapping"]["code_repository"], "org/code");
    assert_eq!(
        shown["task"]["issue_url"],
        "https://github.com/org/tracker/issues/7"
    );
    assert_eq!(shown["reason"], "needs inspection");
    assert_eq!(shown["reserved_slot"], true);
    assert_eq!(shown["attempts"][0]["id"], "attempt");
    assert_eq!(shown["evidence"][2]["detail"]["stdout_bytes"], 5);
    assert!(
        shown["evidence"][0]["created_at"]
            .as_str()
            .unwrap()
            .contains(' ')
    );
    assert!(
        shown["evidence"][0]["created_at_unix_secs"]
            .as_i64()
            .unwrap()
            > 0
    );
    assert!(shown["last_output_age_seconds"].is_number());
    let rendered = shown.to_string();
    assert!(!rendered.contains("do-not-show"));
    assert!(!rendered.contains("secret-prompt"));
    assert_eq!(
        f.run(&["logs", "task", "--attempt", "attempt"]).unwrap()["receipt"]["exit_code"],
        0
    );
    assert_eq!(f.run(&["logs", "task"]).unwrap()["logs"]["stdout"], "hello");
}

#[test]
fn status_and_show_work_while_coordinator_owns_lock() {
    let f = Fixture::new();
    f.task("task");
    let status = f.run(&["status"]).unwrap();
    assert_eq!(status["capacity"]["limit"], 2);
    assert_eq!(status["tasks"][0]["phase"], "held");
    assert_eq!(status["tasks"][0]["reserved_slot"], false);
    let shown = f.run(&["show", "task"]).unwrap();
    assert_eq!(shown["last_output_age_seconds"], Value::Null);
    assert_eq!(
        shown["output_age_unavailable_reason"],
        "no reserved attempt"
    );
    assert_eq!(f.run(&["show", "missing"]), Err(CliError::TaskNotFound));
    assert_eq!(f.run(&["logs", "missing"]), Err(CliError::TaskNotFound));
    assert_eq!(f.run(&["logs", "task"]), Err(CliError::AttemptNotFound));
}

#[test]
fn logs_reject_foreign_attempt_traversal_and_world_readable_files() {
    let f = Fixture::new();
    f.task("one");
    f.task("two");
    f.attempt("two", "second", "session");
    f.logs("second");
    assert_eq!(
        f.run(&["logs", "one", "--attempt", "second"]),
        Err(CliError::AttemptNotFound)
    );
    for id in ["../second", "a/b", "..", "second\\other"] {
        assert_eq!(
            f.run(&["logs", "two", "--attempt", id]),
            Err(CliError::Arguments)
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            f.root().join("attempts/second.stdout.log"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert_eq!(f.run(&["logs", "two"]), Err(CliError::UnsafeLog));
    }
}

#[cfg(unix)]
#[test]
fn logs_reject_symlinks_and_receipt_path_mismatch() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "attempt", "session");
    let (stdout, stderr) = f.logs("attempt");
    fs::remove_file(&stdout).unwrap();
    symlink(&stderr, &stdout).unwrap();
    assert_eq!(f.run(&["logs", "task"]), Err(CliError::UnsafeLog));
    fs::remove_file(&stdout).unwrap();
    fs::write(&stdout, "hello").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&stdout, fs::Permissions::from_mode(0o600)).unwrap();
    let other = f.root().join("outside");
    fs::write(&other, "hello").unwrap();
    f.receipt("task", "attempt", other, stderr);
    assert_eq!(f.run(&["logs", "task"]), Err(CliError::UnsafeLog));
}

#[test]
fn pause_and_reconcile_reject_bad_arguments_and_unknown_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let state = dir.path().join("state");
    fs::write(&config, format!(r#"{{"state_root":"{}","worktree_root":"{}","capacity":1,"assignment_login":"operator","sources":[{{"project_id":"project","repositories":["org/tracker"],"ready_marker":{{"kind":"label","name":"ready"}},"milestone":null}}],"mappings":[{{"tracker_repository":"org/tracker","code_repository":"org/code","checkout":"/code","base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/code","allowed_pr_author":"operator"}}],"initial":{{"executable":"/worker","args":[]}},"resume":{{"executable":"/worker","args":[]}}}}"#, state.display(), dir.path().display())).unwrap();
    let binary = env!("CARGO_BIN_EXE_luthor");
    let bad = Command::new(binary)
        .args(["pause", "--config", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!bad.status.success());

    for command in ["pause", "reconcile"] {
        let output = Command::new(binary)
            .args([
                command,
                "missing-task",
                "--config",
                config.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("task not found"));
    }
}
