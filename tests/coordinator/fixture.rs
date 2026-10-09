use super::fakes::{FakeGithub, FakeLauncher, FakeWriter};
use luthor::state::{exit_observation, journal, scheduling};
use luthor::{
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    coordinator::{
        DispatchDependencies, DispatchError, ResumeDependencies, dispatch_one, resume_one,
    },
    eligibility::Candidate,
    state::StateStore,
    supervisor::LaunchPlan,
};
use serde_json::json;
use std::{fs, path::Path, process::Command};

pub(crate) fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(crate) struct Fixture {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) config: Config,
    pub(crate) store: StateStore,
    pub(crate) candidate: Candidate,
}
impl Fixture {
    pub(crate) fn new(capacity: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let checkout = fixture_checkout(&dir);
        let config = fixture_config(&dir, checkout, capacity);
        let candidate = fixture_candidate(&config);
        let store = StateStore::open(&config.state_root, capacity).unwrap();
        Self {
            dir,
            config,
            store,
            candidate,
        }
    }

    pub(crate) fn run(
        &mut self,
        task: &str,
        candidate: &Candidate,
        github: &mut FakeGithub,
        writer: &mut FakeWriter,
        launcher: &mut FakeLauncher,
    ) -> Result<LaunchPlan, DispatchError> {
        let mut prs = std::mem::take(&mut github.prs);
        let attempt = format!("attempt-{task}");
        let result = dispatch_one(
            &mut self.store,
            candidate,
            DispatchDependencies {
                config: &self.config,
                config_revision: "revision",
                task_id: task,
                attempt_id: &attempt,
                projects: github,
                prs: &mut prs,
                assignments: writer,
                launcher,
            },
        );
        github.prs = prs;
        result
    }

    pub(crate) fn pause(&mut self) {
        let c = self.candidate.clone();
        let mut github = FakeGithub::new(&c);
        self.run(
            "task-a",
            &c,
            &mut github,
            &mut FakeWriter::default(),
            &mut FakeLauncher::default(),
        )
        .unwrap();
        let attempt = "attempt-task-a";
        let launch_detail = luthor::state::launches::launch_intent(&self.store, attempt)
            .unwrap()
            .unwrap();
        let owner =
            luthor::WorktreeOwner::acquire_existing(&self.config.state_root, "task-a").unwrap();
        let protocol = owner
            .protocol_evidence(&self.config.state_root, "task-a", attempt)
            .unwrap();
        luthor::state::launches::begin_supervision(
            &mut self.store,
            "task-a",
            attempt,
            &launch_detail,
            &protocol,
        )
        .unwrap();
        drop(owner);
        journal::record_stop_intent(&mut self.store, "task-a", attempt).unwrap();
        // Seed the already-reconciled exit. The supervisor integration tests cover
        // receipt and process verification; this fixture exercises the coordinator gate.
        let receipt = json!({
            "attempt_id": attempt, "child_pid": 123, "boot_identity": "boot",
            "child_start_identity": "start", "exit_code": null, "signal": 15,
            "stdout_path": "stdout", "stdout_bytes": 0,
            "stderr_path": "stderr", "stderr_bytes": 0, "stop_signals": [15]
        });
        let connection =
            rusqlite::Connection::open(self.config.state_root.join("state.sqlite3")).unwrap();
        connection
            .execute(
                "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES('task-a',?1,'attempt_exit',?2)",
                rusqlite::params![attempt, receipt.to_string()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE attempts SET lifecycle='completed',outcome='exit_code=None;signal=Some(15)' WHERE id=?1",
                [attempt],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE reservations SET status='released' WHERE attempt_id=?1",
                [attempt],
            )
            .unwrap();
        drop(connection);
        exit_observation::record_pause_pr_lookup(
            &mut self.store,
            "task-a",
            attempt,
            &luthor::state::PausePrEvidence {
                observed_at_unix_secs: 2,
                repository: "org/code".into(),
                status: luthor::state::PausePrStatus::Absent,
            },
        )
        .unwrap();
        assert_eq!(scheduling::reservation_count(&self.store).unwrap(), 0);
    }

    pub(crate) fn stopped_but_unproven(&mut self) {
        self.pause();
        let connection =
            rusqlite::Connection::open(self.config.state_root.join("state.sqlite3")).unwrap();
        connection
            .execute(
                "DELETE FROM evidence WHERE task_id='task-a' AND kind='pause_pr_lookup'",
                [],
            )
            .unwrap();
        connection
            .execute("UPDATE tasks SET state='held' WHERE id='task-a'", [])
            .unwrap();
    }

    pub(crate) fn resume(
        &mut self,
        github: &mut FakeGithub,
        launcher: &mut FakeLauncher,
    ) -> Result<LaunchPlan, DispatchError> {
        let mut prs = std::mem::take(&mut github.prs);
        let result = resume_one(
            &mut self.store,
            ResumeDependencies {
                task_id: "task-a",
                attempt_id: "attempt-next",
                projects: github,
                prs: &mut prs,
                launcher,
            },
        );
        github.prs = prs;
        result
    }
}

fn fixture_checkout(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let checkout = dir.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    git(&checkout, &["init", "-b", "main"]);
    git(&checkout, &["config", "user.name", "Fixture"]);
    git(&checkout, &["config", "user.email", "fixture@example.org"]);
    git(
        &checkout,
        &["remote", "add", "origin", "git@github.com:org/code.git"],
    );
    fs::write(checkout.join("README"), "test").unwrap();
    git(&checkout, &["add", "README"]);
    git(&checkout, &["commit", "-m", "initial"]);
    checkout
}

fn fixture_config(
    dir: &tempfile::TempDir,
    checkout: std::path::PathBuf,
    capacity: usize,
) -> Config {
    let source = Source {
        project_id: "project".into(),
        repositories: vec!["org/tracker".into()],
        ready_marker: Marker::Label {
            name: "ready".into(),
        },
        milestone: Some("v1".into()),
    };
    let mapping = Mapping {
        tracker_repository: "org/tracker".into(),
        code_repository: "org/code".into(),
        checkout,
        base_branch: "main".into(),
        push_remote: "origin".into(),
        allowed_pr_head_repository: "org/code".into(),
        allowed_pr_author: "bot".into(),
    };
    let command = CommandTemplate {
        executable: "/bin/worker".into(),
        args: vec![
            "--session".into(),
            "{task.id}".into(),
            "--cwd".into(),
            "{worktree}".into(),
            "-p".into(),
            "start {task.issue_url}".into(),
        ],
    };
    Config {
        state_root: dir
            .path()
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .join("state"),
        worktree_root: dir.path().join("private"),
        capacity,
        assignment_login: "bot".into(),
        sources: vec![source.clone()],
        mappings: vec![mapping.clone()],
        initial: command.clone(),
        resume: CommandTemplate {
            executable: command.executable.clone(),
            args: vec![
                "--session".into(),
                "{task.id}".into(),
                "--cwd".into(),
                "{worktree}".into(),
                "-p".into(),
                "continue {task.issue_url} {attempt.id}".into(),
            ],
        },
    }
}

fn fixture_candidate(config: &Config) -> Candidate {
    Candidate {
        project_id: "project".into(),
        item_id: "item-1".into(),
        repository: "org/tracker".into(),
        issue_node_id: "issue-1".into(),
        issue_number: 1,
        issue_url: "https://github.com/org/tracker/issues/1".into(),
        tracker_repo_id: "repo-id".into(),
        milestone_id: Some("milestone-id".into()),
        milestone_title: Some("v1".into()),
        observed_at_unix_secs: 1,
        observed_state: "open".into(),
        observed_assignees: vec![],
        observed_labels: vec!["ready".into()],
        observed_project_fields: vec![],
        marker: config.sources[0].ready_marker.clone(),
        mapping: config.mappings[0].clone(),
        source: config.sources[0].clone(),
    }
}
