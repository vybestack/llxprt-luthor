use luthor::{
    claim::{AssignmentError, AssignmentWriter},
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    coordinator::{
        AttemptReview, DispatchDependencies, DispatchError, IdCreator, ResumeDependencies,
        ScheduleDependencies, SupervisorLauncher, dispatch_one, resume_one, schedule_candidates,
        schedule_candidates_after_startup, startup_reconcile_all,
    },
    eligibility::Candidate,
    github::{
        project::{Issue, Page, ProjectItem, ProjectReadError, ProjectReader},
        pull_request::{ErrorCategory, LookupError, PullRequestReader},
    },
    state::{StateError, StateStore},
    supervisor::{LaunchPlan, SupervisorError},
};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn git(dir: &Path, args: &[&str]) {
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

struct Fixture {
    dir: tempfile::TempDir,
    config: Config,
    store: StateStore,
    candidate: Candidate,
}
impl Fixture {
    fn new(capacity: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
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
        let config = Config {
            state_root: dir.path().join("state"),
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
        };
        let candidate = Candidate {
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
            marker: source.ready_marker.clone(),
            mapping,
            source,
        };
        let store = StateStore::open(&config.state_root, capacity).unwrap();
        Self {
            dir,
            config,
            store,
            candidate,
        }
    }
    fn run(
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

    fn pause(&mut self) {
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
        self.store.record_stop_intent("task-a", attempt).unwrap();
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
        self.store
            .record_pause_pr_lookup(
                "task-a",
                attempt,
                &luthor::state::PausePrEvidence {
                    observed_at_unix_secs: 2,
                    repository: "org/code".into(),
                    status: luthor::state::PausePrStatus::Absent,
                },
            )
            .unwrap();
        assert_eq!(self.store.reservation_count().unwrap(), 0);
    }

    fn stopped_but_unproven(&mut self) {
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

    fn resume(
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

struct FakeGithub {
    candidate: Candidate,
    reads: usize,
    change_on: Option<usize>,
    assigned: bool,
    prs: FakePr,
}
impl FakeGithub {
    fn new(candidate: &Candidate) -> Self {
        Self {
            candidate: candidate.clone(),
            reads: 0,
            change_on: None,
            assigned: false,
            prs: FakePr::default(),
        }
    }
}
impl ProjectReader for FakeGithub {
    fn page(&mut self, _: &str, _: Option<&str>) -> Result<Page<ProjectItem>, ProjectReadError> {
        let c = &self.candidate;
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
        self.reads += 1;
        let c = &self.candidate;
        let assignees = if self.change_on == Some(self.reads) {
            vec!["other".into()]
        } else if self.reads > 1 || self.assigned {
            vec!["bot".into()]
        } else {
            vec![]
        };
        Ok(Issue {
            node_id: c.issue_node_id.clone(),
            repository: c.repository.clone(),
            tracker_repo_id: c.tracker_repo_id.clone(),
            number: c.issue_number,
            url: c.issue_url.clone(),
            state: "open".into(),
            assignees,
            labels: vec!["ready".into()],
            milestone: Some("v1".into()),
            milestone_id: Some("milestone-id".into()),
            observed_at_unix_secs: 1,
        })
    }
}
#[derive(Default)]
struct FakePr {
    lookups: usize,
    present_on: Option<usize>,
    fail_on: Option<usize>,
}
impl PullRequestReader for FakePr {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok("bot".into())
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<Value>, LookupError> {
        self.lookups += 1;
        if self.fail_on == Some(self.lookups) {
            return Err(LookupError {
                category: ErrorCategory::Transport,
                code: "offline",
                status: None,
            });
        }
        if self.present_on == Some(self.lookups) {
            return Ok(vec![
                json!({"number": 7, "body": "Tracker-Issue: https://github.com/org/tracker/issues/1"}),
            ]);
        }
        Ok(vec![])
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
    fn detail(&mut self, _: &str, _: u64) -> Result<Value, LookupError> {
        Ok(
            json!({"id": 77, "number": 7, "state": "open", "html_url": "https://github.com/org/code/pull/7",
            "body": "Tracker-Issue: https://github.com/org/tracker/issues/1", "created_at": "2026-01-01T00:00:00Z",
            "base": {"repo": {"id": 10, "full_name": "org/code"}, "ref": "main"},
            "head": {"repo": {"id": 10, "full_name": "org/code"}, "ref": "branch", "sha": "abc123"},
            "user": {"login": "bot"}, "draft": false }),
        )
    }
}
#[derive(Default)]
struct FakeWriter {
    calls: usize,
}
impl AssignmentWriter for FakeWriter {
    fn assign(&mut self, _: &str, _: u64, _: &str) -> Result<(), AssignmentError> {
        self.calls += 1;
        Ok(())
    }
}
#[derive(Default)]
struct FakeLauncher {
    plans: Vec<LaunchPlan>,
    fail: bool,
}
impl SupervisorLauncher for FakeLauncher {
    fn launch(&mut self, _: &mut StateStore, plan: &LaunchPlan) -> Result<(), SupervisorError> {
        self.plans.push(plan.clone());
        if self.fail {
            Err(SupervisorError::ExecutionUnavailable)
        } else {
            Ok(())
        }
    }
}

#[test]
fn worktree_preflight_failures_precede_assignment_and_attempts() {
    for failure in ["base", "origin", "branch"] {
        let mut f = Fixture::new(1);
        let checkout = f.candidate.mapping.checkout.clone();
        match failure {
            "base" => git(&checkout, &["branch", "-m", "missing-base"]),
            "origin" => git(
                &checkout,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "git@github.com:org/wrong.git",
                ],
            ),
            "branch" => git(&checkout, &["branch", "luthor/task-a"]),
            _ => unreachable!(),
        }
        let c = f.candidate.clone();
        let mut github = FakeGithub::new(&c);
        let mut writer = FakeWriter::default();
        let mut launcher = FakeLauncher::default();
        assert!(
            matches!(
                f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
                Err(DispatchError::Worktree(_))
            ),
            "{failure}"
        );
        assert_eq!(writer.calls, 0, "{failure}");
        assert!(!f.config.worktree_root.exists(), "{failure}");
    }
}

#[test]
fn verified_claim_and_worktree_precede_fake_launch() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let plan = f
        .run("task-a", &c, &mut github, &mut writer, &mut launcher)
        .unwrap();
    assert_eq!(writer.calls, 1);
    assert_eq!(github.reads, 3);
    assert_eq!(github.prs.lookups, 3);
    assert_eq!(launcher.plans.as_slice(), std::slice::from_ref(&plan));
    assert_eq!(
        plan.worktree,
        fs::canonicalize(f.config.worktree_root.join("task-a")).unwrap()
    );
    assert_eq!(f.store.reservation_count().unwrap(), 1);
    assert_eq!(
        f.store.task_phase("task-a").unwrap().as_deref(),
        Some("held")
    );
    assert!(f.store.launch_intent("attempt-task-a").unwrap().is_some());
    assert!(f.dir.path().join("checkout").exists());
}

#[test]
fn existing_pr_blocks_assignment_and_holds_selection() {
    let mut f = Fixture::new(2);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    github.prs.present_on = Some(1);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::Claim(_))
    ));
    assert_eq!(writer.calls, 0);
    assert!(launcher.plans.is_empty());
    assert_eq!(
        f.store.held_reason("task-a").unwrap().as_deref(),
        Some("claim failed")
    );
    assert!(!f.config.worktree_root.join("task-a").exists());
}

#[test]
fn changed_claim_blocks_before_worktree_or_launch() {
    let mut f = Fixture::new(2);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    github.change_on = Some(3);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::ChangedClaim)
    ));
    assert_eq!(writer.calls, 1);
    assert!(launcher.plans.is_empty());
    assert_eq!(f.store.reservation_count().unwrap(), 0);
    assert!(f.config.worktree_root.join("task-a").exists());
}

#[test]
fn new_pr_or_failed_lookup_blocks_before_reservation() {
    for fail in [false, true] {
        let mut f = Fixture::new(2);
        let c = f.candidate.clone();
        let mut github = FakeGithub::new(&c);
        if fail {
            github.prs.fail_on = Some(3);
        } else {
            github.prs.present_on = Some(3);
        }
        let mut writer = FakeWriter::default();
        let mut launcher = FakeLauncher::default();
        let result = f.run("task-a", &c, &mut github, &mut writer, &mut launcher);
        assert!(if fail {
            matches!(result, Err(DispatchError::PullRequest(_)))
        } else {
            matches!(result, Err(DispatchError::ExistingPr))
        });
        assert_eq!(writer.calls, 1);
        assert!(launcher.plans.is_empty());
        assert_eq!(f.store.reservation_count().unwrap(), 0);
        assert_eq!(
            f.store.task_phase("task-a").unwrap().as_deref(),
            Some("held")
        );
    }
}

#[test]
fn failed_launch_retains_reservation_and_second_schedule_cannot_launch() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher {
        fail: true,
        ..Default::default()
    };
    assert!(matches!(
        f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::Supervisor(_))
    ));
    assert_eq!(f.store.reservation_count().unwrap(), 1);
    assert_eq!(
        f.store.held_reason("task-a").unwrap().as_deref(),
        Some("launch preparation or dispatch failed")
    );
    let mut other = c.clone();
    other.issue_node_id = "issue-2".into();
    other.item_id = "item-2".into();
    other.issue_number = 2;
    other.issue_url = "https://github.com/org/tracker/issues/2".into();
    assert!(matches!(
        f.run("task-b", &other, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::State(StateError::Capacity { .. }))
    ));
    assert_eq!(writer.calls, 1);
    assert_eq!(launcher.plans.len(), 1);
    assert_eq!(f.store.task_count().unwrap(), 1);
}

#[test]
fn distinct_tasks_keep_selection_and_repository_issue_uniqueness() {
    let mut f = Fixture::new(3);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    f.run("task-a", &c, &mut github, &mut writer, &mut launcher)
        .unwrap();
    assert!(matches!(
        f.run("task-a-copy", &c, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::State(StateError::DuplicateTask(_, _)))
    ));
    let mut second = c.clone();
    second.item_id = "item-2".into();
    second.issue_node_id = "issue-2".into();
    second.issue_number = 2;
    second.issue_url = "https://github.com/org/tracker/issues/2".into();
    let mut github2 = FakeGithub::new(&second);
    f.run("task-b", &second, &mut github2, &mut writer, &mut launcher)
        .unwrap();
    assert_eq!(
        f.store
            .selection_evidence("task-a")
            .unwrap()
            .unwrap()
            .candidate,
        c
    );
    assert_eq!(
        f.store
            .selection_evidence("task-b")
            .unwrap()
            .unwrap()
            .candidate,
        second
    );
    assert_eq!(launcher.plans.len(), 2);
    assert_ne!(launcher.plans[0].session_id, launcher.plans[1].session_id);
    assert_eq!(f.store.reservation_count().unwrap(), 2);
}

#[test]
fn paused_resume_uses_stored_selection_and_launches_one_continuation() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut github = FakeGithub::new(&f.candidate);
    github.assigned = true;
    f.config.assignment_login = "other".into();
    f.config.resume.args.clear();
    let mut launcher = FakeLauncher::default();
    let plan = f.resume(&mut github, &mut launcher).unwrap();
    assert_eq!(github.reads, 1);
    assert_eq!(github.prs.lookups, 1);
    assert_eq!(launcher.plans.as_slice(), std::slice::from_ref(&plan));
    assert_eq!(plan.attempt_id, "attempt-next");
    assert_eq!(plan.session_id, "task-a");
    assert_eq!(plan.config_revision, "revision");
    let selection = f.store.selection_evidence("task-a").unwrap().unwrap();
    assert_eq!(selection.candidate.source, f.candidate.source);
    assert_eq!(selection.effective_config.assignment_login, "bot");
    assert!(plan.args.iter().any(|arg| arg.contains("continue")));
    assert_eq!(f.store.reservation_count().unwrap(), 1);
    assert_eq!(
        f.store.latest_attempt("task-a").unwrap().as_deref(),
        Some("attempt-next")
    );
    assert_eq!(
        f.store.task_phase("task-a").unwrap().as_deref(),
        Some("held")
    );
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(launcher.plans.len(), 1);
}

#[test]
fn resume_pr_present_or_failed_lookup_holds_without_new_attempt() {
    for fail in [false, true] {
        let mut f = Fixture::new(1);
        f.pause();
        let mut github = FakeGithub::new(&f.candidate);
        github.assigned = true;
        if fail {
            github.prs.fail_on = Some(1);
        } else {
            github.prs.present_on = Some(1);
        }
        let mut launcher = FakeLauncher::default();
        let result = f.resume(&mut github, &mut launcher);
        assert!(if fail {
            matches!(result, Err(DispatchError::PullRequest(_)))
        } else {
            matches!(result, Err(DispatchError::ExistingPr))
        });
        assert_eq!(
            f.store.held_reason("task-a").unwrap().as_deref(),
            Some(if fail {
                "resume PR read failed"
            } else {
                "resume PR present"
            })
        );
        assert_eq!(f.store.reservation_count().unwrap(), 0);
        assert_eq!(
            f.store.latest_attempt("task-a").unwrap().as_deref(),
            Some("attempt-task-a")
        );
        assert!(launcher.plans.is_empty());
    }
}

#[test]
fn resume_changed_claim_holds_without_new_attempt() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut github = FakeGithub::new(&f.candidate);
    github.change_on = Some(1);
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::ChangedClaim)
    ));
    assert_eq!(github.prs.lookups, 0);
    assert_eq!(
        f.store.held_reason("task-a").unwrap().as_deref(),
        Some("resume claim changed")
    );
    assert_eq!(f.store.reservation_count().unwrap(), 0);
    assert_eq!(
        f.store.latest_attempt("task-a").unwrap().as_deref(),
        Some("attempt-task-a")
    );
    assert!(launcher.plans.is_empty());
}

#[test]
fn resume_without_paused_reconciled_state_cannot_create_attempt() {
    let mut f = Fixture::new(1);
    let mut github = FakeGithub::new(&f.candidate);
    let mut launcher = FakeLauncher::default();
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(f.store.task_count().unwrap(), 0);
    f.pause();
    f.store.set_task_phase("task-a", "held").unwrap();
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(github.reads, 0);
    assert_eq!(github.prs.lookups, 0);
    assert_eq!(f.store.reservation_count().unwrap(), 0);
    assert_eq!(
        f.store.latest_attempt("task-a").unwrap().as_deref(),
        Some("attempt-task-a")
    );
    assert!(launcher.plans.is_empty());
}

#[test]
fn failed_resume_dispatch_retains_reservation_and_never_retries() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut github = FakeGithub::new(&f.candidate);
    github.assigned = true;
    let mut launcher = FakeLauncher {
        fail: true,
        ..Default::default()
    };
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::Supervisor(_))
    ));
    assert_eq!(launcher.plans.len(), 1);
    assert_eq!(
        f.store.held_reason("task-a").unwrap().as_deref(),
        Some("resume preparation or dispatch failed")
    );
    assert_eq!(f.store.reservation_count().unwrap(), 1);
    assert_eq!(
        f.store.latest_attempt("task-a").unwrap().as_deref(),
        Some("attempt-next")
    );
    assert!(matches!(
        f.resume(&mut github, &mut launcher),
        Err(DispatchError::State(StateError::LaunchBlocked))
    ));
    assert_eq!(launcher.plans.len(), 1);
}

#[derive(Default)]
struct FixedIds(usize);
impl IdCreator for FixedIds {
    fn create(&mut self) -> Result<String, std::io::Error> {
        self.0 += 1;
        Ok(self.0.to_string())
    }
}

#[test]
fn startup_reconcile_holds_missing_receipt_without_relaunching() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    f.run("task-a", &c, &mut github, &mut writer, &mut launcher)
        .unwrap();
    let launches = launcher.plans.len();
    let mut projects = FakeGithub::new(&f.candidate);
    let report = startup_reconcile_all(&mut f.store, &mut projects, &mut github.prs).unwrap();
    assert_eq!(report.attempts.len(), 1);
    assert!(matches!(report.attempts[0].review, AttemptReview::Held(_)));
    assert_eq!(f.store.reservation_count().unwrap(), 1);
    assert_eq!(launcher.plans.len(), launches);
}

#[test]
fn unproven_stopped_attempt_never_reads_pr_or_releases_task() {
    let mut f = Fixture::new(1);
    f.stopped_but_unproven();
    let mut projects = FakeGithub::new(&f.candidate);
    let mut prs = FakePr::default();
    let result = luthor::coordinator::reconcile_with_pr(
        &mut f.store,
        "task-a",
        "attempt-task-a",
        &mut projects,
        &mut prs,
    )
    .unwrap();
    assert!(matches!(
        result,
        luthor::supervisor::Reconciliation::Held { .. }
    ));
    assert_eq!(prs.lookups, 0);
    assert_eq!(
        f.store.task_phase("task-a").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(f.store.reservation_count().unwrap(), 0);
    assert!(matches!(
        f.store.ensure_dispatch_capacity(),
        Err(StateError::Capacity { .. })
    ));
}

#[test]
fn scheduler_dispatches_other_task_after_verified_pause_without_resuming_paused_task() {
    let mut f = Fixture::new(1);
    f.pause();
    let mut other = f.candidate.clone();
    other.issue_node_id = "issue-2".into();
    other.item_id = "item-2".into();
    other.issue_number = 2;
    other.issue_url = "https://github.com/org/tracker/issues/2".into();
    let mut github = FakeGithub::new(&other);
    let mut prs = FakePr::default();
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let report = schedule_candidates(
        &mut f.store,
        vec![other],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut github,
            prs: &mut prs,
            assignments: &mut writer,
            launcher: &mut launcher,
            ids: &mut FixedIds::default(),
        },
    )
    .unwrap();
    assert!(!report.capacity_full);
    assert_eq!(report.launched.len(), 1);
    assert_eq!(
        f.store.task_phase("task-a").unwrap().as_deref(),
        Some("paused")
    );
    assert_eq!(f.store.reservation_count().unwrap(), 1);
    assert!(matches!(
        f.store.ensure_dispatch_capacity(),
        Err(luthor::state::StateError::Capacity { .. })
    ));
}

#[test]
fn scheduler_uses_precomputed_startup_without_reconciling_again() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    f.run("task-a", &c, &mut github, &mut writer, &mut launcher)
        .unwrap();
    let latest = f.store.latest_attempt("task-a").unwrap();
    let mut projects = FakeGithub::new(&f.candidate);
    let mut prs = FakePr::default();
    let mut assignments = FakeWriter::default();
    let mut scheduler_launcher = FakeLauncher::default();
    let report = schedule_candidates_after_startup(
        &mut f.store,
        vec![],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut projects,
            prs: &mut prs,
            assignments: &mut assignments,
            launcher: &mut scheduler_launcher,
            ids: &mut FixedIds::default(),
        },
        Default::default(),
    )
    .unwrap();
    assert_eq!(report.startup, Default::default());
    assert!(report.launched.is_empty());
    assert_eq!(projects.reads, 0);
    assert_eq!(prs.lookups, 0);
    assert_eq!(f.store.latest_attempt("task-a").unwrap(), latest);
    assert_eq!(f.store.reservation_count().unwrap(), 1);
}

#[test]
fn scheduler_with_precomputed_blocked_startup_does_not_select_candidates() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let mut projects = FakeGithub::new(&c);
    let mut prs = FakePr::default();
    let mut assignments = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let startup = luthor::coordinator::StartupReport {
        source_holds: vec![luthor::coordinator::SourceHold {
            task_id: "task-source".into(),
            kind: "claim".into(),
        }],
        ..Default::default()
    };
    let report = schedule_candidates_after_startup(
        &mut f.store,
        vec![c],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut projects,
            prs: &mut prs,
            assignments: &mut assignments,
            launcher: &mut launcher,
            ids: &mut FixedIds::default(),
        },
        startup.clone(),
    )
    .unwrap();
    assert_eq!(report.startup, startup);
    assert!(report.launched.is_empty());
    assert_eq!(projects.reads, 0);
    assert_eq!(prs.lookups, 0);
    assert_eq!(assignments.calls, 0);
}
