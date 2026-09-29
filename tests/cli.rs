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
            expected_worktree: luthor::state::WorktreeIdentity {
                path: PathBuf::from("/worktree"),
                device: 1,
                inode: 1,
                branch: "luthor/task".into(),
                base: "main".into(),
                head: "abc".into(),
                repository: "org/code".into(),
                git_directory: PathBuf::from("/checkout/.git"),
                remote: "origin".into(),
            },
            executable: PathBuf::from("/bin/worker"),
            args: vec!["--secret-prompt".into()],
            config_revision: "rev".into(),
            session_environment: luthor::supervisor::SessionEnvironment {
                home: PathBuf::from("/"),
                xdg_config_home: None,
                xdg_data_home: None,
                xdg_state_home: None,
                llxprt_config_home: None,
            },
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
    assert_eq!(status["tasks"][0]["latest_attempt_id"], Value::Null);
    assert_eq!(status["tasks"][0]["latest_attempt_outcome"], Value::Null);
    assert_eq!(status["tasks"][0]["pr_state"], "unavailable");
    assert!(
        status["tasks"][0]["pr_unavailable_reason"]
            .as_str()
            .is_some()
    );
    assert_eq!(status["reserved_slot_count"], 0);
    assert_eq!(status["latest_telemetry"], Value::Null);
    assert_eq!(status["tasks"][0]["last_output_age_seconds"], Value::Null);
    assert_eq!(status["tasks"][0]["output_silence_warning"], false);
    assert_eq!(
        status["tasks"][0]["output_age_unavailable_reason"],
        "no reserved attempt"
    );
    let shown = f.run(&["show", "task"]).unwrap();
    assert_eq!(shown["pr_state"], "unavailable");
    assert_eq!(shown["last_observed_pr"], Value::Null);
    assert_eq!(
        shown["last_observed_pr_unavailable_reason"],
        "no verified stored PR proof"
    );
    assert_eq!(shown["output_log_status"], "unavailable");
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
fn status_reports_output_age_and_silence_without_exposing_log_contents() {
    let f = Fixture::new();
    f.task("active");
    f.attempt("active", "old-attempt", "session");
    let (stdout, stderr) = f.logs("old-attempt");
    fs::write(&stderr, "private-log-secret").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    let times = fs::FileTimes::new().set_modified(old);
    fs::File::options()
        .write(true)
        .open(&stdout)
        .unwrap()
        .set_times(times)
        .unwrap();
    fs::File::options()
        .write(true)
        .open(&stderr)
        .unwrap()
        .set_times(times)
        .unwrap();

    let status = f.run(&["status"]).unwrap();
    let task = &status["tasks"][0];
    assert!(task["last_output_age_seconds"].as_u64().unwrap() >= 300);
    assert_eq!(task["output_age_unavailable_reason"], Value::Null);
    assert_eq!(task["output_silence_warning"], true);
    assert_eq!(task["silence_warning_threshold_seconds"], 300);
    assert_eq!(task["latest_attempt_id"], "old-attempt");
    assert!(!status.to_string().contains("hello"));
    assert!(!status.to_string().contains("private-log-secret"));

    fs::write(stdout, "").unwrap();
    fs::write(stderr, "").unwrap();
    let status = f.run(&["status"]).unwrap();
    let task = &status["tasks"][0];
    assert!(task["last_output_age_seconds"].is_number());
    assert_eq!(task["output_silence_warning"], false);
    assert_eq!(task["output_log_status"], "no_output_yet");
    assert_eq!(task["output_age_unavailable_reason"], Value::Null);
}

#[test]
fn status_warns_on_durable_attempt_age_with_empty_logs_without_requesting_stop() {
    let f = Fixture::new();
    f.task("old");
    f.attempt("old", "silent", "session");
    let (stdout, stderr) = f.logs("silent");
    fs::write(stdout, "").unwrap();
    fs::write(stderr, "").unwrap();
    f.db()
        .execute(
            "UPDATE attempts SET created_at=datetime('now','-600 seconds') WHERE id='silent'",
            [],
        )
        .unwrap();

    let status = f.run(&["status"]).unwrap();
    let task = &status["tasks"][0];
    assert_eq!(task["output_log_status"], "no_output_yet");
    assert!(task["last_output_age_seconds"].as_u64().unwrap() >= 300);
    assert_eq!(task["output_age_unavailable_reason"], Value::Null);
    assert_eq!(task["output_silence_warning"], true);
    assert_eq!(task["phase"], "held");
    let shown = f.run(&["show", "old"]).unwrap();
    assert_eq!(shown["phase"], "held");
    assert!(
        shown["intents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["kind"] != "stop")
    );
    assert_eq!(
        f.db()
            .query_row("SELECT state FROM tasks WHERE id='old'", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "held"
    );
    assert_eq!(
        f.db()
            .query_row(
                "SELECT lifecycle FROM attempts WHERE id='silent'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "launch_intended"
    );

    f.task("young");
    f.attempt("young", "quiet", "session");
    let (stdout, stderr) = f.logs("quiet");
    fs::write(&stdout, "").unwrap();
    fs::write(&stderr, "").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    let times = fs::FileTimes::new().set_modified(old);
    for path in [&stdout, &stderr] {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(times)
            .unwrap();
    }
    let status = f.run(&["status"]).unwrap();
    let young = status["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == "young")
        .unwrap();
    assert_eq!(young["output_log_status"], "no_output_yet");
    assert!(young["last_output_age_seconds"].as_u64().unwrap() < 300);
    assert_eq!(young["output_silence_warning"], false);
}

#[test]
fn status_does_not_warn_without_a_valid_reserved_attempt_start() {
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "silent", "session");
    let (stdout, stderr) = f.logs("silent");
    fs::write(stdout, "").unwrap();
    fs::write(stderr, "").unwrap();
    f.db()
        .execute(
            "UPDATE attempts SET created_at='invalid' WHERE id='silent'",
            [],
        )
        .unwrap();
    let task = &f.run(&["status"]).unwrap()["tasks"][0];
    assert_eq!(task["output_silence_warning"], false);
    assert_eq!(task["last_output_age_seconds"], Value::Null);
    assert_eq!(task["output_log_status"], "unavailable");
    assert_eq!(
        task["output_age_unavailable_reason"],
        "no verified output log data or valid attempt start time"
    );

    f.db()
        .execute(
            "UPDATE attempts SET created_at=datetime('now','-600 seconds') WHERE id='silent'",
            [],
        )
        .unwrap();
    f.db()
        .execute(
            "UPDATE reservations SET status='released' WHERE attempt_id='silent'",
            [],
        )
        .unwrap();
    let task = &f.run(&["status"]).unwrap()["tasks"][0];
    assert_eq!(task["output_silence_warning"], false);
    assert_eq!(task["last_output_age_seconds"], Value::Null);
    assert_eq!(task["output_age_unavailable_reason"], "no reserved attempt");
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

#[cfg(unix)]
mod resume_cli {
    use luthor::{
        config::{CommandTemplate, Config, Mapping, Marker, Source},
        eligibility::Candidate,
        state::StateStore,
    };
    use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};
    use tempfile::{TempDir, tempdir_in};

    struct Harness {
        _dir: TempDir,
        config: std::path::PathBuf,
        state: std::path::PathBuf,
        log: std::path::PathBuf,
        path: String,
    }

    impl Harness {
        fn new() -> Self {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp");
            fs::create_dir_all(&root).unwrap();
            let dir = tempdir_in(root).unwrap();
            let gh = dir.path().join("gh");
            let log = dir.path().join("invocations.log");
            fs::write(
                &gh,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 1\n",
                    log.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
            let state = dir.path().join("state");
            let config = dir.path().join("config.json");
            let value = Config {
                state_root: state.clone(),
                worktree_root: dir.path().to_path_buf(),
                capacity: 2,
                assignment_login: "agent".into(),
                sources: vec![Source {
                    project_id: "PROJECT".into(),
                    repositories: vec!["org/tracker".into()],
                    ready_marker: Marker::Label {
                        name: "ready".into(),
                    },
                    milestone: None,
                }],
                mappings: vec![Mapping {
                    tracker_repository: "org/tracker".into(),
                    code_repository: "org/code".into(),
                    checkout: dir.path().join("checkout"),
                    base_branch: "main".into(),
                    push_remote: "origin".into(),
                    allowed_pr_head_repository: "org/head".into(),
                    allowed_pr_author: "acoliver".into(),
                }],
                initial: CommandTemplate {
                    executable: gh.clone(),
                    args: vec!["initial-worker".into()],
                },
                resume: CommandTemplate {
                    executable: gh,
                    args: vec!["resume-worker".into()],
                },
            };
            value.validate().unwrap();
            fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
            let path = format!("{}:/usr/bin:/bin", dir.path().display());
            Self {
                _dir: dir,
                config,
                state,
                log,
                path,
            }
        }
        fn run(&self, args: &[&str]) -> std::process::Output {
            Command::new(env!("CARGO_BIN_EXE_luthor"))
                .args(args)
                .env("PATH", &self.path)
                .output()
                .unwrap()
        }
        fn seed_held_task(&self) {
            let config = Config::from_json(&fs::read_to_string(&self.config).unwrap()).unwrap();
            let source = config.sources[0].clone();
            let mapping = config.mappings[0].clone();
            let candidate = Candidate {
                project_id: "PROJECT".into(),
                item_id: "ITEM".into(),
                repository: "org/tracker".into(),
                issue_node_id: "ISSUE".into(),
                issue_number: 7,
                issue_url: "https://github.com/org/tracker/issues/7".into(),
                tracker_repo_id: "REPO".into(),
                milestone_id: None,
                milestone_title: None,
                observed_at_unix_secs: 1,
                observed_state: "open".into(),
                observed_assignees: vec!["agent".into()],
                observed_labels: vec!["ready".into()],
                observed_project_fields: vec![],
                marker: source.ready_marker.clone(),
                source,
                mapping,
            };
            let mut store = StateStore::open(&self.state, config.capacity).unwrap();
            store
                .create_task("task", &candidate, "rev", &config)
                .unwrap();
            store.hold_task("task", "fixture paused").unwrap();
        }
        fn dispatch_gh(&self) -> (std::path::PathBuf, std::path::PathBuf) {
            let assignments = self._dir.path().join("assignments.log");
            let worker = self._dir.path().join("worker.marker");
            fs::write(&assignments, "").unwrap();
            let gh = self._dir.path().join("gh");
            fs::write(&gh, format!(r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  *initial-worker*|*resume-worker*) touch '{}' ;;
  *"api -X POST "*) printf '%s\n' "$*" >> '{}' ;;
  *graphql*) printf '%s\n' '{{"data":{{"node":{{"items":{{"nodes":[{{"id":"ITEM-8","content":{{"__typename":"Issue","id":"ISSUE-8","number":8,"repository":{{"id":"REPO","nameWithOwner":"org/tracker"}}}},"fieldValues":{{"nodes":[],"pageInfo":{{"hasNextPage":false}}}}}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}' ;;
  *repos/org/tracker/issues/8*) printf '%s\n' '{{"node_id":"ISSUE-8","number":8,"repository_url":"https://api.github.com/repos/org/tracker","html_url":"https://github.com/org/tracker/issues/8","state":"open","assignees":[],"labels":[{{"name":"ready"}}],"milestone":null}}' ;;
  *repos/org/tracker*) printf '%s\n' '{{"node_id":"REPO"}}' ;;
  *"api user --jq .login"*) printf '%s\n' 'acoliver' ;;
  *) exit 91 ;;
esac
"#, self.log.display(), worker.display(), assignments.display())).unwrap();
            fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
            (assignments, worker)
        }
    }
    fn stderr(output: &std::process::Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    #[test]
    fn dispatch_execute_holds_unresolved_source_before_project_selection_with_spare_capacity() {
        let h = Harness::new();
        h.seed_held_task();
        let mut store = StateStore::open(&h.state, 2).unwrap();
        store
            .record_claim_intent("task", "agent", "org/tracker", 7)
            .unwrap();
        assert_eq!(store.unresolved_sources().unwrap()[0].1, "claim_assignment");
        drop(store);
        let (assignments, worker) = h.dispatch_gh();
        let output = h.run(&[
            "dispatch",
            "--execute",
            "--config",
            h.config.to_str().unwrap(),
            "--repository",
            "org/tracker",
            "--issue",
            "8",
            "--config-revision",
            "rev",
        ]);
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("startup reconciliation"),
            "{}",
            stderr(&output)
        );
        assert!(
            !h.log.exists(),
            "project selection ran before reconciliation"
        );
        assert_eq!(fs::read_to_string(assignments).unwrap(), "");
        assert!(!worker.exists());
        let store = StateStore::open(&h.state, 2).unwrap();
        assert_eq!(store.unresolved_sources().unwrap()[0].1, "claim_assignment");
    }

    #[test]
    fn dispatch_execute_holds_uncertain_attempt_with_spare_capacity() {
        let h = Harness::new();
        h.seed_held_task();
        let db = rusqlite::Connection::open(h.state.join("state.sqlite3")).unwrap();
        db.execute(
            "INSERT INTO attempts(id,task_id,lifecycle) VALUES('old-attempt','task','launch_intended')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO reservations(attempt_id,task_id,status) VALUES('old-attempt','task','reserved')",
            [],
        )
        .unwrap();
        drop(db);
        let store = StateStore::open(&h.state, 2).unwrap();
        assert!(
            store
                .pending_attempts()
                .unwrap()
                .contains(&("task".into(), "old-attempt".into()))
        );
        assert!(store.unresolved_sources().unwrap().is_empty());
        drop(store);
        let (assignments, worker) = h.dispatch_gh();
        let output = h.run(&[
            "dispatch",
            "--execute",
            "--config",
            h.config.to_str().unwrap(),
            "--repository",
            "org/tracker",
            "--issue",
            "8",
            "--config-revision",
            "rev",
        ]);
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("startup reconciliation"),
            "{}",
            stderr(&output)
        );
        assert!(!h.log.exists());
        assert_eq!(fs::read_to_string(assignments).unwrap(), "");
        assert!(!worker.exists());
    }

    #[test]
    fn dispatch_execute_clean_store_reaches_eligible_project_candidate() {
        let h = Harness::new();
        let (assignments, worker) = h.dispatch_gh();
        let output = h.run(&[
            "dispatch",
            "--execute",
            "--config",
            h.config.to_str().unwrap(),
            "--repository",
            "org/tracker",
            "--issue",
            "8",
            "--config-revision",
            "rev",
        ]);
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("worktree failed"),
            "{}",
            stderr(&output)
        );
        assert!(!stderr(&output).contains("authorized PR author"));
        let calls = fs::read_to_string(&h.log).unwrap();
        assert!(calls.contains("api graphql"), "{calls}");
        assert!(calls.contains("api user --jq .login"), "{calls}");
        assert_eq!(fs::read_to_string(assignments).unwrap(), "");
        assert!(!worker.exists());
    }

    #[test]
    fn dispatch_without_execute_does_not_open_state_or_call_github() {
        let h = Harness::new();
        let (assignments, worker) = h.dispatch_gh();
        let output = h.run(&[
            "dispatch",
            "--config",
            h.config.to_str().unwrap(),
            "--repository",
            "org/tracker",
            "--issue",
            "8",
            "--config-revision",
            "rev",
        ]);
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("pass --execute"),
            "{}",
            stderr(&output)
        );
        assert!(!h.state.exists());
        assert!(!h.log.exists());
        assert_eq!(fs::read_to_string(assignments).unwrap(), "");
        assert!(!worker.exists());
    }

    #[test]
    fn resume_without_execute_is_held_without_github_or_worker_invocations() {
        let h = Harness::new();
        let output = h.run(&["resume", "task", "--config", h.config.to_str().unwrap()]);
        assert!(!output.status.success());
        assert!(stderr(&output).contains("pass --execute"));
        assert!(!h.log.exists());
    }
    #[test]
    fn resume_rejects_malformed_and_duplicate_execute_arguments_without_invocation() {
        let h = Harness::new();
        let config = h.config.to_str().unwrap();
        for args in [
            vec![
                "resume",
                "task",
                "--config",
                config,
                "--execute",
                "--execute",
            ],
            vec!["resume", "task", "--execute", "--config", config],
            vec!["resume", "task", "--config", config, "--unknown"],
        ] {
            let output = h.run(&args);
            assert!(!output.status.success(), "{args:?}");
        }
        assert!(!h.log.exists());
    }
    #[test]
    fn resume_unknown_task_fails_before_github_invocation() {
        let h = Harness::new();
        let output = h.run(&[
            "resume",
            "unknown",
            "--config",
            h.config.to_str().unwrap(),
            "--execute",
        ]);
        assert!(!output.status.success());
        assert!(stderr(&output).contains("task not found"));
        assert!(!h.log.exists());
    }
    #[test]
    fn resume_rejects_non_resumable_held_task_without_github_or_worker_invocation() {
        let h = Harness::new();
        h.seed_held_task();
        let output = h.run(&[
            "resume",
            "task",
            "--config",
            h.config.to_str().unwrap(),
            "--execute",
        ]);
        assert!(!output.status.success());
        assert!(stderr(&output).contains("resume held"));
        let store = StateStore::open(&h.state, 2).unwrap();
        assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
        assert_eq!(
            store.held_reason("task").unwrap().as_deref(),
            Some("fixture paused")
        );
        assert!(!h.log.exists());
    }
    #[test]
    fn reconcile_prelaunch_claim_uses_only_read_only_gh_and_pause_still_needs_attempt() {
        let h = Harness::new();
        h.seed_held_task();
        let mut store = StateStore::open(&h.state, 2).unwrap();
        store
            .record_claim_intent("task", "agent", "org/tracker", 7)
            .unwrap();
        drop(store);
        let gh = h._dir.path().join("gh");
        fs::write(&gh, format!(r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  *graphql*) printf '%s\n' '{{"data":{{"node":{{"items":{{"nodes":[{{"id":"ITEM","content":{{"__typename":"Issue","id":"ISSUE","number":7,"repository":{{"id":"REPO","nameWithOwner":"org/tracker"}}}},"fieldValues":{{"nodes":[],"pageInfo":{{"hasNextPage":false}}}}}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}' ;;
  *repos/org/tracker/issues/7*) printf '%s\n' '{{"node_id":"ISSUE","number":7,"repository_url":"https://api.github.com/repos/org/tracker","html_url":"https://github.com/org/tracker/issues/7","state":"open","assignees":[{{"login":"agent"}}],"labels":[{{"name":"ready"}}],"milestone":null}}' ;;
  *repos/org/tracker*) printf '%s\n' '{{"node_id":"REPO"}}' ;;
  *) exit 91 ;;
esac
"#, h.log.display())).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let config = h.config.to_str().unwrap();
        let output = h.run(&["reconcile", "task", "--config", config]);
        assert!(output.status.success(), "{}", stderr(&output));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["status"], "held");
        assert_eq!(report["project_membership"], true);
        assert_eq!(report["marker_present"], true);
        assert_eq!(report["assignees"], serde_json::json!(["agent"]));
        assert!(
            report["reasons"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("claim_intent_unverified"))
        );
        let calls = fs::read_to_string(&h.log).unwrap();
        assert_eq!(calls.lines().count(), 3);
        assert!(calls.lines().all(|line| line.starts_with("api ")
            && !line.contains("-X")
            && !line.contains("POST")));
        let pause = h.run(&["pause", "task", "--config", config]);
        assert!(!pause.status.success());
        assert!(stderr(&pause).contains("attempt not found"));
        let store = StateStore::open(&h.state, 2).unwrap();
        assert_eq!(store.latest_attempt("task").unwrap(), None);
        assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("held"));
        assert_eq!(store.unresolved_sources().unwrap()[0].1, "claim_assignment");
    }
}
