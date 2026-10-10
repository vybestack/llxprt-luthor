use luthor::{
    cli::{CliError, execute},
    state::StateStore,
    supervisor::{ExitReceipt, LaunchPlan},
};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) struct Fixture {
    dir: tempfile::TempDir,
    _lock: StateStore,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let lock = StateStore::open(dir.path(), 2).unwrap();
        Self { dir, _lock: lock }
    }
    pub(super) fn root(&self) -> &Path {
        self.dir.path()
    }
    pub(super) fn db(&self) -> Connection {
        Connection::open(self.root().join("state.sqlite3")).unwrap()
    }
    pub(super) fn run(&self, args: &[&str]) -> Result<Value, CliError> {
        execute(
            self.root(),
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .map(|s| serde_json::from_str(&s).unwrap())
    }
    pub(super) fn task(&self, id: &str) {
        self.db().execute("INSERT INTO tasks(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision)
            VALUES(?1,?2,?3,'org/tracker',7,'held','rev')",params![id,format!("repo-{id}"),format!("issue-{id}")]).unwrap();
    }
    pub(super) fn attempt(&self, task: &str, id: &str, session: &str) {
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
    pub(super) fn evidence(&self, task: &str, attempt: Option<&str>, kind: &str, payload: &str) {
        self.db()
            .execute(
                "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,?3,?4)",
                params![task, attempt, kind, payload],
            )
            .unwrap();
    }
    pub(super) fn logs(&self, id: &str) -> (PathBuf, PathBuf) {
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
    pub(super) fn receipt(&self, task: &str, id: &str, stdout: PathBuf, stderr: PathBuf) {
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
