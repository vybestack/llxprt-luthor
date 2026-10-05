use luthor::state::{journal, scheduling, task_records};
use luthor::{
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    eligibility::Candidate,
    state::StateStore,
    supervisor::{execute_with_binary, prepare_initial, request_stop},
    worktree::ensure_worktree,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};
use tempfile::{Builder, TempDir};

pub(super) struct Harness {
    pub(super) _dir: TempDir,
    pub(super) config: std::path::PathBuf,
    pub(super) state: std::path::PathBuf,
    pub(super) log: std::path::PathBuf,
    pub(super) path: String,
}

impl Harness {
    pub(super) fn new() -> Self {
        let dir = Builder::new().prefix("luthor-cli-").tempdir().unwrap();
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
        let state = dir
            .path()
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .join("state");
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
    pub(super) fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_luthor"))
            .args(args)
            .env("PATH", &self.path)
            .output()
            .unwrap()
    }
    pub(super) fn seed_held_task(&self) {
        self.seed_task();
        let mut store = StateStore::open(&self.state, 2).unwrap();
        task_records::hold_task(&mut store, "task", "fixture paused").unwrap();
    }
    pub(super) fn seed_task(&self) {
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
        task_records::create_task(&mut store, "task", &candidate, "rev", &config).unwrap();
    }
    pub(super) fn seed_running_task(&self) {
        let worker = self._dir.path().join("cooperative-worker");
        fs::write(
            &worker,
            format!(
                "#!/bin/sh\nprintf 'stdout-check\\n'\nprintf 'stderr-check\\n' >&2\nprintf 'started\\n' >> '{}'\nexec /bin/sleep 120\n",
                self._dir.path().join("a-starts.log").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o755)).unwrap();
        let mut config = Config::from_json(&fs::read_to_string(&self.config).unwrap()).unwrap();
        config.initial.executable = worker;
        config.initial.args = vec![
            "--session".into(),
            "{task.id}".into(),
            "--cwd".into(),
            "{worktree}".into(),
            "-p".into(),
            "Work on {task.issue_url}".into(),
        ];
        fs::write(&self.config, serde_json::to_vec(&config).unwrap()).unwrap();
        self.seed_task();
        let mut store = StateStore::open(&self.state, 2).unwrap();
        let candidate = task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap()
            .candidate;
        task_records::record_claim_intent(&mut store, "task", "agent", "org/tracker", 7).unwrap();
        journal::record_evidence(&mut store, "task", None, "claim_verified", "agent").unwrap();
        task_records::set_task_phase(&mut store, "task", "claimed").unwrap();
        self.initialize_checkout(&candidate.mapping.checkout);
        ensure_worktree(
            &mut store,
            "task",
            &config.worktree_root,
            &candidate.mapping,
        )
        .unwrap();
        self.launch_worker(&mut store);
    }
    fn initialize_checkout(&self, checkout: &Path) {
        fs::create_dir(checkout).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.org"],
            vec!["remote", "add", "origin", "git@github.com:org/code.git"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(checkout)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fs::write(checkout.join("README"), "fixture").unwrap();
        for args in [vec!["add", "README"], vec!["commit", "-m", "initial"]] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(checkout)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
    fn launch_worker(&self, store: &mut StateStore) {
        let plan = prepare_initial(store, "task", "running-attempt").unwrap();
        execute_with_binary(store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap_or_else(
            |error| {
                panic!(
                    "{error}: {}",
                    fs::read_to_string(self.state.join("attempts/running-attempt.supervisor.log"))
                        .unwrap_or_default()
                )
            },
        );
        assert_eq!(scheduling::reservation_count(store).unwrap(), 1);
        let marker = self._dir.path().join("a-starts.log");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(marker.exists(), "worker A did not start");
    }
    pub(super) fn stop_running_task(&self) {
        let mut store = StateStore::open(&self.state, 2).unwrap();
        request_stop(&mut store, "task", "running-attempt").unwrap();
        let receipt = self.state.join("attempts/running-attempt.receipt.json");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !receipt.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(receipt.exists(), "cooperative child did not exit");
    }
    pub(super) fn dispatch_gh(&self) -> (std::path::PathBuf, std::path::PathBuf) {
        let assignments = self._dir.path().join("assignments.log");
        let worker = self._dir.path().join("worker.marker");
        fs::write(&assignments, "").unwrap();
        let gh = self._dir.path().join("gh");
        fs::write(&gh, format!(r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  *initial-worker*|*resume-worker*) touch '{}' ;;
  *"api -X POST "*) printf '%s\n' "$*" >> '{}' ;;
  *graphql*) printf '%s\n' '{{"data":{{"node":{{"items":{{"nodes":[{{"id":"ITEM","content":{{"__typename":"Issue","id":"ISSUE","number":7,"repository":{{"id":"REPO","nameWithOwner":"org/tracker"}}}},"fieldValues":{{"nodes":[],"pageInfo":{{"hasNextPage":false}}}}}},{{"id":"ITEM-8","content":{{"__typename":"Issue","id":"ISSUE-8","number":8,"repository":{{"id":"REPO","nameWithOwner":"org/tracker"}}}},"fieldValues":{{"nodes":[],"pageInfo":{{"hasNextPage":false}}}}}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}' ;;
  *repos/org/tracker/issues/8*) printf '%s\n' '{{"node_id":"ISSUE-8","number":8,"repository_url":"https://api.github.com/repos/org/tracker","html_url":"https://github.com/org/tracker/issues/8","state":"open","assignees":[],"labels":[{{"name":"ready"}}],"milestone":null}}' ;;
  *repos/org/tracker/issues/7*) printf '%s\n' '{{"node_id":"ISSUE","number":7,"html_url":"https://github.com/org/tracker/issues/7","repository_url":"https://api.github.com/repos/org/tracker","state":"open","assignees":[{{"login":"agent"}}],"labels":[{{"name":"ready"}}],"milestone":null}}' ;;
  *repos/org/code/pulls*) printf '%s\n' '[]' ;;
  *repos/org/tracker*) printf '%s\n' '{{"node_id":"REPO"}}' ;;
  *"api user --jq .login"*) printf '%s\n' 'acoliver' ;;
  *) exit 91 ;;
esac
"#, self.log.display(), worker.display(), assignments.display())).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        (assignments, worker)
    }
}
pub(super) fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
