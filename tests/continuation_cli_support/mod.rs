use luthor::state::{journal, task_records};
mod amendment;
mod diagnostics;
use luthor::{
    config::Config,
    eligibility::Candidate,
    state::StateStore,
    supervisor::{self, LaunchPlan},
    worktree,
};
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub config: Config,
    pub config_path: PathBuf,
    pub task: String,
    pub attempt: String,
    pub saved: String,
    pub plan: LaunchPlan,
    pub calls: PathBuf,
    pub marker: PathBuf,
    path: String,
}
fn seed_saved_attempt(config: &Config, task: &str, attempt: &str) -> LaunchPlan {
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
        marker: config.sources[0].ready_marker.clone(),
        mapping: config.mappings[0].clone(),
        source: config.sources[0].clone(),
    };
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    task_records::create_task(&mut store, task, &candidate, "saved-revision", config).unwrap();
    task_records::record_claim_intent(&mut store, task, "acoliver", "org/tracker", 7).unwrap();
    journal::record_evidence(&mut store, task, None, "claim_verified", "acoliver").unwrap();
    task_records::set_task_phase(&mut store, task, "claimed").unwrap();
    worktree::ensure_worktree(&mut store, task, &config.worktree_root, &candidate.mapping).unwrap();
    let plan = supervisor::prepare_initial(&mut store, task, attempt).unwrap();
    let saved = serde_json::to_string_pretty(&plan).unwrap();
    let db = Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute("UPDATE intents SET detail=?1 WHERE kind='launch'", [&saved])
        .unwrap();
    supervisor::preflight_attempt_storage(&config.state_root).unwrap();
    fs::set_permissions(
        config.state_root.join("attempts"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    plan
}

impl Fixture {
    pub fn new() -> Self {
        Self::new_with(false)
    }

    pub fn new_with(native: bool) -> Self {
        let dir = tempfile::Builder::new().prefix("cu").tempdir().unwrap();
        let root = dir.path();
        let task = format!("task-{}", root.file_name().unwrap().to_str().unwrap());
        let attempt = format!("a-{}", root.file_name().unwrap().to_str().unwrap());
        let checkout = root.join("checkout");
        fs::create_dir(&checkout).unwrap();
        git(&checkout, &["init", "-b", "main"]);
        git(&checkout, &["config", "user.name", "Fixture"]);
        git(&checkout, &["config", "user.email", "fixture@example.org"]);
        git(
            &checkout,
            &["remote", "add", "origin", "git@github.com:org/code.git"],
        );
        fs::write(checkout.join("README"), "fixture").unwrap();
        git(&checkout, &["add", "README"]);
        git(&checkout, &["commit", "-m", "initial"]);
        let marker = root.join("worker-runs");
        let worker_name = if native {
            "llxprt-code-rs".into()
        } else {
            format!(
                "marker-worker-{}",
                root.file_name().unwrap().to_str().unwrap()
            )
        };
        let worker = root.join(worker_name);
        script(
            &worker,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" >> '{}'\nprintf 'healthy local worker\\n'\n",
                marker.display()
            ),
        );
        let command = |prompt| json!({"executable":worker,"args":["--session","{task.id}","--cwd","{worktree}","-p",prompt,"--max-tool-calls","1"]});
        let mut config_value = json!({
            "state_root":root.strip_prefix(std::env::current_dir().unwrap()).unwrap().join("s"),
            "worktree_root":root.join("w"), "capacity":1,"assignment_login":"acoliver",
            "sources":[{"project_id":"project","repositories":["org/tracker"],"ready_marker":{"kind":"label","name":"ready"},"milestone":null}],
            "mappings":[{"tracker_repository":"org/tracker","code_repository":"org/code","checkout":checkout,"base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/code","allowed_pr_author":"acoliver"}],
            "initial":command("Start {task.issue_url}"),"resume":command("Continue {task.issue_url} {attempt.id}")
        });
        if native {
            let args = config_value["initial"]["args"].as_array_mut().unwrap();
            args.splice(2..2, [json!("--branch"), json!("luthor/{task.id}")]);
        }
        let config = Config::from_json(&config_value.to_string()).unwrap();
        let config_path = root.join("config.json");
        fs::write(&config_path, config_value.to_string()).unwrap();
        let plan = seed_saved_attempt(&config, &task, &attempt);
        let saved = serde_json::to_string_pretty(&plan).unwrap();
        let calls = fake_github(root);
        let path = format!("{}:{}", root.display(), std::env::var("PATH").unwrap());
        Self {
            dir,
            config,
            config_path,
            task,
            attempt,
            saved,
            plan,
            calls,
            marker,
            path,
        }
    }

    pub fn args(&self) -> Vec<String> {
        [
            "--task",
            &self.task,
            "--attempt",
            &self.attempt,
            "--config",
            self.config_path.to_str().unwrap(),
            "--config-revision",
            "saved-revision",
            "--actor",
            "acoliver",
            "--reason",
            "legacy_preflight_recovery",
            "--execute",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    pub fn command(&self, args: &[String]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_luthor"));
        cmd.arg("continue-undispatched")
            .args(args)
            .env("PATH", &self.path)
            .env(
                "LUTHOR_PROCESS_DIAGNOSTIC_FILE",
                self.dir.path().join("process-probe"),
            );
        cmd
    }

    pub fn process_diagnostic(&self) -> String {
        fs::read_to_string(self.dir.path().join("process-probe"))
            .map(|record| record.chars().take(160).collect())
            .unwrap_or_else(|_| "process_probe record=missing".into())
    }

    pub fn run(&self, args: &[String]) -> Output {
        self.command(args).output().unwrap()
    }
    pub fn retry_os_uncertainty(&self, args: &[String], mut out: Output) -> (Output, String) {
        let protected = self.rows(&["tasks", "attempts", "reservations", "intents"]);
        let mut calls_start = 0;
        for retry in 0..=20 {
            let value = serde_json::from_slice::<Value>(&out.stdout).unwrap();
            if value["reason"] != "process_unavailable" {
                let calls = fs::read_to_string(&self.calls).unwrap();
                return (out, calls[calls_start..].to_owned());
            }
            // The real OS scan can lose a same-user process between ps and cwd.
            // Retry only a pre-authorization refusal, never a consumed launch.
            assert!(!out.status.success());
            assert_eq!(value["status"], "held");
            assert_eq!(value["task_id"], self.task);
            assert_eq!(value["attempt_id"], self.attempt);
            assert_eq!(
                protected,
                self.rows(&["tasks", "attempts", "reservations", "intents"])
            );
            assert_eq!(self.count("evidence", "never_dispatched_authorized"), 0);
            assert!(!self.marker.exists());
            assert_eq!(
                fs::read_dir(self.config.state_root.join("attempts"))
                    .unwrap()
                    .count(),
                0
            );
            assert!(
                retry < 20,
                "production process inspector remained unavailable in isolated fixture: {}",
                self.process_diagnostic()
            );
            calls_start = fs::read_to_string(&self.calls).unwrap().len();
            thread::sleep(Duration::from_millis(50));
            out = self.run(args);
        }
        panic!("production process inspector remained unavailable in isolated fixture");
    }

    pub fn correct_storage(&self) {
        fs::set_permissions(
            self.config.state_root.join("attempts"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }

    pub fn db(&self) -> Connection {
        Connection::open(self.config.state_root.join("state.sqlite3")).unwrap()
    }

    pub fn rows(&self, tables: &[&str]) -> Vec<Vec<Vec<SqlValue>>> {
        let db = self.db();
        tables
            .iter()
            .map(|table| {
                let mut stmt = db
                    .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                    .unwrap();
                let columns = stmt.column_count();
                stmt.query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap()
            })
            .collect()
    }

    pub fn count(&self, table: &str, kind: &str) -> i64 {
        self.db()
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE kind=?1"),
                [kind],
                |row| row.get(0),
            )
            .unwrap()
    }

    pub fn assert_held(&self, reason: &str) {
        let before = self.rows(&["tasks", "attempts", "reservations", "intents"]);
        let args = self.args();
        let (out, _) = self.retry_os_uncertainty(&args, self.run(&args));
        assert!(
            !out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["status"], "held");
        assert_eq!(value["reason"], reason);
        assert_eq!(value["task_id"], self.task);
        assert_eq!(value["attempt_id"], self.attempt);
        assert_eq!(
            before,
            self.rows(&["tasks", "attempts", "reservations", "intents"])
        );
        assert_eq!(self.count("evidence", "never_dispatched_authorized"), 0);
        assert!(!self.marker.exists());
        assert_eq!(
            fs::read_dir(self.config.state_root.join("attempts"))
                .unwrap()
                .count(),
            0
        );
    }
}

fn fake_github(root: &Path) -> PathBuf {
    let calls = root.join("gh-calls");
    fs::write(&calls, "").unwrap();
    let project = json!({"data":{"node":{"items":{"nodes":[{
        "id":"item","content":{"__typename":"Issue","id":"issue","number":7,"repository":{"id":"repo","nameWithOwner":"org/tracker"}},
        "fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false}}
    }],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}});
    fs::write(root.join("project.json"), project.to_string()).unwrap();
    fs::write(root.join("issue.json"), json!({"node_id":"issue","number":7,"repository_url":"https://api.github.com/repos/org/tracker",
        "html_url":"https://github.com/org/tracker/issues/7","state":"open","assignees":[{"login":"acoliver"}],"labels":[{"name":"ready"}],"milestone":null}).to_string()).unwrap();
    fs::write(root.join("prs.json"), "[]").unwrap();
    let gh = root.join("gh");
    script(
        &gh,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{root}/gh-calls'
if test -f '{root}/block'; then
  touch '{root}/observing'
  while ! test -f '{root}/release'; do sleep 0.02; done
fi
case "$2" in
 graphql) cat '{root}/project.json' ;;
 repos/org/tracker) printf '%s\n' '{{"node_id":"repo"}}' ;;
 repos/org/tracker/issues/7?per_page=100) cat '{root}/issue.json' ;;
 repos/org/code/pulls?*) cat '{root}/prs.json' ;;
 user) printf 'acoliver\n' ;;
 *) printf 'unexpected or write operation\n' >&2; exit 99 ;;
esac
"#,
            root = root.display()
        ),
    );
    calls
}

fn script(path: &Path, program: &str) {
    fs::write(path, program).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

pub fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(Instant::now() < deadline, "missing {}", path.display());
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
