use luthor::{
    WorktreeOwner,
    config::Config,
    eligibility::Candidate,
    state::{
        StateError, StateStore,
        capacity::{self, CapacityChange, CapacityChangeRequest},
        journal, scheduling, task_records,
    },
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub config: Config,
}

impl Fixture {
    pub fn new(capacity: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let config = Config::from_json(
            &json!({
                "state_root":root.join("state"), "worktree_root":root.join("worktrees"),
                "capacity":capacity, "assignment_login":"operator",
                "sources":[{"project_id":"project", "repositories":["org/tracker"],
                    "ready_marker":{"kind":"label", "name":"ready"}}],
                "mappings":[{"tracker_repository":"org/tracker", "code_repository":"org/code",
                    "checkout":root.join("checkout"), "base_branch":"main", "push_remote":"origin",
                    "allowed_pr_head_repository":"org/code", "allowed_pr_author":"operator"}],
                "initial":{"executable":"/worker-must-not-run", "args":["{task.id}"]},
                "resume":{"executable":"/worker-must-not-run", "args":["{task.id}"]}
            })
            .to_string(),
        )
        .unwrap();
        drop(StateStore::open(&config.state_root, capacity).unwrap());
        Self { dir, config }
    }

    pub fn root(&self) -> &Path {
        &self.config.state_root
    }
    pub fn db(&self) -> Connection {
        Connection::open(self.root().join("state.sqlite3")).unwrap()
    }

    pub fn task(&self, id: &str, number: u64) {
        let candidate = Candidate {
            project_id: "project".into(),
            item_id: format!("item-{id}"),
            repository: "org/tracker".into(),
            issue_node_id: format!("issue-{id}"),
            issue_number: number,
            issue_url: format!("https://github.com/org/tracker/issues/{number}"),
            tracker_repo_id: "repo-id".into(),
            milestone_id: None,
            milestone_title: None,
            observed_at_unix_secs: 123,
            observed_state: "open".into(),
            observed_assignees: vec![],
            observed_labels: vec!["ready".into()],
            observed_project_fields: vec![],
            marker: self.config.sources[0].ready_marker.clone(),
            mapping: self.config.mappings[0].clone(),
            source: self.config.sources[0].clone(),
        };
        let mut store = StateStore::open(self.root(), self.config.capacity).unwrap();
        task_records::create_task(
            &mut store,
            id,
            &candidate,
            "original-revision",
            &self.config,
        )
        .unwrap();
    }

    pub fn completed(&self, id: &str, number: u64) {
        self.task(id, number);
        let attempt = format!("attempt-{id}");
        let mut store = StateStore::open(self.root(), self.config.capacity).unwrap();
        scheduling::reserve(&mut store, id, &attempt).unwrap();
        let plan = json!({"task_id":id, "attempt_id":attempt, "session_id":format!("session-{id}"),
            "worktree":self.dir.path().join(id), "expected_worktree":{
                "path":self.dir.path().join(id), "device":1, "inode":2, "branch":format!("luthor/{id}"),
                "base":"main", "head":"original-head", "repository":"org/code", "git_directory":"/git", "remote":"origin"},
            "executable":"/worker-must-not-run", "args":["original instructions"], "config_revision":"original-revision",
            "session_environment":{"home":self.dir.path()}}).to_string();
        journal::record_intent(
            &mut store,
            &format!("launch-{id}"),
            id,
            Some(&attempt),
            "launch",
            &plan,
        )
        .unwrap();
        let attempts = self.root().join("attempts");
        fs::create_dir_all(&attempts).unwrap();
        let stdout = attempts.join(format!("{attempt}.stdout"));
        let stderr = attempts.join(format!("{attempt}.stderr"));
        fs::write(&stdout, "historical output").unwrap();
        fs::write(&stderr, "historical diagnostic").unwrap();
        let receipt = json!({"attempt_id":attempt, "child_pid":123,
            "boot_identity":"fixture-boot", "child_start_identity":"fixture-start",
            "exit_code":0, "signal":null, "stdout_path":stdout, "stderr_path":stderr,
            "stdout_bytes":17, "stderr_bytes":21, "stop_signals":[]})
        .to_string();
        fs::write(attempts.join(format!("{attempt}.exit.json")), &receipt).unwrap();
        fs::write(attempts.join(format!("{attempt}.plan.json")), plan).unwrap();
        journal::record_evidence(&mut store, id, Some(&attempt), "attempt_exit", &receipt).unwrap();
        journal::record_evidence(
            &mut store,
            id,
            Some(&attempt),
            "verified_open_pr",
            &json!({
                "id":number, "url":format!("https://github.com/org/code/pull/{number}"),
                "repository":"org/code", "head_repository":"org/code", "draft":true,
                "checks":["failure"], "observed_at":123, "attempt_id":attempt
            })
            .to_string(),
        )
        .unwrap();
        let db = self.db();
        db.execute("UPDATE attempts SET lifecycle='completed',outcome='exit_code=Some(0);signal=None' WHERE id=?1", [&attempt]).unwrap();
        db.execute(
            "UPDATE reservations SET status='released' WHERE attempt_id=?1",
            [&attempt],
        )
        .unwrap();
        db.execute("UPDATE tasks SET state='pr_complete' WHERE id=?1", [id])
            .unwrap();
        drop(WorktreeOwner::acquire(self.root(), id).unwrap());
    }

    pub fn change(
        &self,
        old: usize,
        new: usize,
        reserved: usize,
    ) -> Result<CapacityChange, StateError> {
        capacity::change(
            self.root(),
            CapacityChangeRequest {
                expected_capacity: old,
                expected_reservations: reserved,
                new_capacity: new,
                actor: "operator",
                reason: "resize the existing queue",
            },
        )
    }

    pub fn args(&self, old: usize, new: usize, reserved: usize) -> Vec<String> {
        let mut config = self.config.clone();
        config.capacity = new;
        let path = self.dir.path().join("desired-config.json");
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        vec![
            "resize-capacity".into(),
            "--config".into(),
            path.to_str().unwrap().into(),
            "--expected-capacity".into(),
            old.to_string(),
            "--expected-reservations".into(),
            reserved.to_string(),
            "--actor".into(),
            "operator".into(),
            "--reason".into(),
            "resize the existing queue".into(),
            "--execute".into(),
        ]
    }

    pub fn history(&self) -> Vec<String> {
        let db = self.db();
        ["SELECT json_group_array(json_array(id,tracker_repo_id,issue_node_id,repository,issue_number,state,config_revision,created_at)) FROM tasks ORDER BY rowid",
         "SELECT json_group_array(json_array(id,task_id,lifecycle,outcome,created_at)) FROM attempts ORDER BY rowid",
         "SELECT json_group_array(json_array(sequence,id,task_id,attempt_id,kind,detail,created_at)) FROM intents ORDER BY sequence",
         "SELECT json_group_array(json_array(attempt_id,task_id,status,created_at)) FROM reservations ORDER BY rowid",
         "SELECT json_group_array(json_array(sequence,task_id,attempt_id,kind,payload,created_at)) FROM evidence ORDER BY sequence",
         "SELECT json_group_array(json_array(attempt_id,task_id,launch_sequence,owner_payload)) FROM launch_preparations ORDER BY rowid",
         "SELECT json_group_array(json_array(task_id,purpose,identity,observed_at)) FROM pr_polls ORDER BY rowid",
         "SELECT json_group_array(json_array(name,seq)) FROM sqlite_sequence WHERE name!='capacity_changes' ORDER BY name"]
            .iter().map(|sql| db.query_row(sql, [], |row| row.get(0)).unwrap()).collect()
    }

    pub fn audit_count(&self) -> i64 {
        self.db()
            .query_row("SELECT COUNT(*) FROM capacity_changes", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    pub fn view(&self, args: &[&str]) -> Value {
        serde_json::from_str(
            &luthor::cli::execute(
                self.root(),
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    pub fn launch(&self, id: &str, attempt: &str) {
        let mut store = StateStore::open(self.root(), self.config.capacity).unwrap();
        scheduling::reserve(&mut store, id, attempt).unwrap();
        journal::record_intent(
            &mut store,
            &format!("launch-{id}"),
            id,
            Some(attempt),
            "launch",
            "original-plan",
        )
        .unwrap();
        drop(WorktreeOwner::acquire(self.root(), id).unwrap());
    }

    pub fn evidence(&self, task: &str, attempt: Option<&str>, kind: &str, payload: &str) {
        self.db()
            .execute(
                "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,?2,?3,?4)",
                params![task, attempt, kind, payload],
            )
            .unwrap();
    }
}

pub fn cli(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(args)
        .output()
        .unwrap()
}

pub fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
