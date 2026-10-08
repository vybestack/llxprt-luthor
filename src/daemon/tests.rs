#![cfg(target_os = "macos")]

use super::{Error, Options, cycle_with_dependencies};
use crate::{
    claim::GhAssignmentWriter,
    config::Config,
    coordinator::{
        AttemptReview, DispatchDependencies, DispatchError, IdCreator, ScheduleReport,
        SupervisorLauncher, dispatch_one,
    },
    eligibility,
    github::{project::GhProjectReader, pull_request::GhPullRequestReader},
    state::{StateStore, launches, scheduling, task_records},
    supervisor::{LaunchPlan, SupervisorError},
};
use rusqlite::types::Value;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Default)]
struct RecordingLauncher {
    plans: Vec<LaunchPlan>,
    fail: bool,
}

impl SupervisorLauncher for RecordingLauncher {
    fn launch(
        &mut self,
        _store: &mut StateStore,
        plan: &LaunchPlan,
        _ownership: &crate::ownership::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        self.plans.push(plan.clone());
        if self.fail {
            Err(SupervisorError::ExecutionUnavailable)
        } else {
            Ok(())
        }
    }
}

struct FixedIds;

impl IdCreator for FixedIds {
    fn create(&mut self) -> Result<String, std::io::Error> {
        Ok("fixture-id".into())
    }
}

fn create_checkout(path: &Path) {
    fs::create_dir(path).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.org"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["config", "core.hooksPath", "/dev/null"],
        vec!["remote", "add", "origin", "git@github.com:org/code.git"],
        vec!["commit", "--allow-empty", "-m", "base"],
    ] {
        let result = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

fn project_item(number: u64) -> serde_json::Value {
    json!({
        "id":format!("ITEM{number}"),
        "content":{"__typename":"Issue","id":format!("ISSUE{number}"),"number":number,
            "repository":{"id":"REPO331","nameWithOwner":"org/tracker"}},
        "fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}
    })
}

fn issue_json(number: u64, assigned: bool) -> String {
    let assignees = if assigned {
        vec![json!({"login":"fixture-agent"})]
    } else {
        vec![]
    };
    json!({
        "node_id":format!("ISSUE{number}"),"number":number,
        "repository_url":"https://api.github.com/repos/org/tracker",
        "html_url":format!("https://github.com/org/tracker/issues/{number}"),
        "state":"open","assignees":assignees,"labels":[{"name":"ready"}],"milestone":null
    })
    .to_string()
}

fn gh_script(calls: &Path, assigned: &Path, assigned12: &Path) -> String {
    let project = json!({"data":{"node":{"items":{
        "nodes":[project_item(331),project_item(12)],
        "pageInfo":{"hasNextPage":false,"endCursor":null}
    }}}})
    .to_string();
    format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$2" in
  graphql) printf '%s\n' '{}' ;;
  repos/org/tracker) printf '%s\n' '{{"node_id":"REPO331"}}' ;;
  repos/org/tracker/issues/331?per_page=100) if test -f '{}'; then printf '%s\n' '{}'; else printf '%s\n' '{}'; fi ;;
  repos/org/tracker/issues/12?per_page=100) if test -f '{}'; then printf '%s\n' '{}'; else printf '%s\n' '{}'; fi ;;
  "repos/org/code/pulls?state=open&per_page=100&page=1") printf '%s\n' '[]' ;;
  user) printf '%s\n' 'acoliver' ;;
  -X) test "$3" = POST || exit 99
    test "$5" = -f && test "$6" = 'assignees[]=fixture-agent' || exit 99
    case "$4" in
      repos/org/tracker/issues/331/assignees) touch '{}' ;;
      repos/org/tracker/issues/12/assignees) touch '{}' ;;
      *) exit 99 ;;
    esac; printf '%s\n' '{{}}' ;;
  *) exit 99 ;;
esac
"#,
        calls.display(),
        project,
        assigned.display(),
        issue_json(331, true),
        issue_json(331, false),
        assigned12.display(),
        issue_json(12, true),
        issue_json(12, false),
        assigned.display(),
        assigned12.display()
    )
}

fn create_config(root: &Path, checkout: &Path) -> Config {
    Config::from_json(&json!({
        "state_root": root.join("state"), "worktree_root": root.join("worktrees"), "capacity": 2,
        "assignment_login": "fixture-agent",
        "sources": [{"project_id":"PROJECT","repositories":["org/tracker"],"ready_marker":{"kind":"label","name":"ready"},"milestone":null}],
        "mappings": [{"tracker_repository":"org/tracker","code_repository":"org/code","checkout":checkout,"base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/code","allowed_pr_author":"acoliver"}],
        "initial": {"executable": root.join("nonexistent-worker"), "args": ["--session", "{task.id}", "--cwd", "{worktree}", "--prompt", "Continue issue"]},
        "resume": {"executable": root.join("nonexistent-worker"), "args": ["--session", "{task.id}", "--cwd", "{worktree}", "--prompt", "Continue issue"]}
    }).to_string()).unwrap()
}

fn validate_gh_fixture(gh: &Path) {
    let syntax = Command::new("sh").arg("-n").arg(gh).output().unwrap();
    assert!(syntax.status.success());
    let graphql = Command::new(gh)
        .args([
            "api",
            "graphql",
            "-f",
            "query=fixture",
            "-F",
            "projectId=PROJECT",
        ])
        .output()
        .unwrap();
    assert!(graphql.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&graphql.stdout).unwrap();
    let nodes = payload["data"]["node"]["items"]["nodes"]
        .as_array()
        .unwrap();
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0]["content"]["number"], 331);
    assert_eq!(nodes[1]["content"]["number"], 12);
}

struct Fixture {
    root: tempfile::TempDir,
    config: Config,
    gh: PathBuf,
    calls: PathBuf,
    assigned: PathBuf,
    assigned12: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("d28-")
            .tempdir_in("/private/var/tmp")
            .unwrap();
        let checkout = root.path().join("checkout");
        create_checkout(&checkout);
        let calls = root.path().join("calls");
        let assigned = root.path().join("assigned");
        let assigned12 = root.path().join("assigned12");
        let gh = root.path().join("gh");
        fs::write(&gh, gh_script(&calls, &assigned, &assigned12)).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
        validate_gh_fixture(&gh);
        let config = create_config(root.path(), &checkout);
        assert!(gh.is_absolute());
        assert_eq!(
            config.initial.executable,
            root.path().join("nonexistent-worker")
        );
        assert!(!config.initial.executable.exists());
        assert_eq!(config.resume.executable, config.initial.executable);
        Self {
            root,
            config,
            gh,
            calls,
            assigned,
            assigned12,
        }
    }

    fn options(&self) -> Options {
        let options = Options::parse(&[
            "--config".into(),
            self.root
                .path()
                .join("unused-config.json")
                .to_string_lossy()
                .into_owned(),
            "--config-revision".into(),
            "test-revision".into(),
            "--repository".into(),
            "org/tracker".into(),
            "--issues".into(),
            "331".into(),
            "--once".into(),
            "--execute".into(),
        ])
        .unwrap();
        assert!(options.once && options.execute);
        options
    }

    fn cycle(
        &self,
        config: &Config,
        launcher: &mut RecordingLauncher,
    ) -> Result<Option<ScheduleReport>, Error> {
        cycle_with_dependencies(
            config,
            &self.options(),
            self.gh.clone(),
            launcher,
            &mut FixedIds,
        )
    }
}

fn seed_held_issue12(f: &Fixture) -> String {
    let mut store = StateStore::open(&f.config.state_root, 2).unwrap();
    let mut projects = GhProjectReader::new(f.gh.clone());
    let mut prs = GhPullRequestReader::new(f.gh.clone());
    let mut assignments = GhAssignmentWriter {
        executable: f.gh.clone(),
    };
    let mut launcher = RecordingLauncher {
        fail: true,
        ..Default::default()
    };
    let selected = eligibility::select_target(
        &mut projects,
        &f.config.sources,
        &f.config.mappings,
        "org/tracker",
        12,
    )
    .unwrap();
    assert_eq!(selected.len(), 1);
    let result = dispatch_one(
        &mut store,
        &selected[0],
        DispatchDependencies {
            config: &f.config,
            config_revision: "test-revision",
            task_id: "task-12",
            attempt_id: "attempt-12",
            projects: &mut projects,
            prs: &mut prs,
            assignments: &mut assignments,
            launcher: &mut launcher,
        },
    );
    assert!(matches!(
        result,
        Err(DispatchError::Supervisor(
            SupervisorError::ExecutionUnavailable
        ))
    ));
    assert_eq!(launcher.plans.len(), 1);
    assert!(f.assigned12.exists());
    assert!(!f.assigned.exists());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    let original = launches::launch_intent(&store, "attempt-12")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<LaunchPlan>(&original).unwrap(),
        launcher.plans[0]
    );
    assert_eq!(
        task_records::task_phase(&store, "task-12")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert_eq!(
        task_records::latest_attempt(&store, "task-12")
            .unwrap()
            .as_deref(),
        Some("attempt-12")
    );
    for path in [&f.config.state_root, &f.config.state_root.join("attempts")] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    crate::supervisor::validate_stop_socket_path(&f.config.state_root, "attempt-fixture-id")
        .unwrap();
    drop(store);
    original
}

type History = Vec<Vec<Vec<Value>>>;

fn history(store: &StateStore, task: Option<&str>) -> History {
    ["tasks", "attempts", "intents", "reservations", "evidence"]
        .iter()
        .map(|table| {
            let column = if *table == "tasks" { "id" } else { "task_id" };
            let sql = match task {
                Some(_) => format!("SELECT * FROM {table} WHERE {column}=?1 ORDER BY rowid"),
                None => format!("SELECT * FROM {table} ORDER BY rowid"),
            };
            let mut statement = store.connection.prepare(&sql).unwrap();
            let columns = statement.column_count();
            let read_row = |row: &rusqlite::Row<'_>| {
                (0..columns)
                    .map(|index| row.get::<_, Value>(index))
                    .collect::<Result<Vec<_>, _>>()
            };
            match task {
                Some(task) => statement
                    .query_map([task], read_row)
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
                None => statement
                    .query_map([], read_row)
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
            }
        })
        .collect()
}

fn assert_saved_attempt(store: &StateStore, original: &str) {
    assert_eq!(
        launches::launch_intent(store, "attempt-12")
            .unwrap()
            .as_deref(),
        Some(original)
    );
    assert_eq!(
        task_records::latest_attempt(store, "task-12")
            .unwrap()
            .as_deref(),
        Some("attempt-12")
    );
    let attempt: (String, Option<String>, String) = store.connection.query_row(
        "SELECT a.lifecycle,a.outcome,r.status FROM attempts a JOIN reservations r ON r.attempt_id=a.id WHERE a.id='attempt-12'",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(attempt, ("launch_intended".into(), None, "reserved".into()));
    let forged: i64 = store.connection.query_row(
        "SELECT (SELECT COUNT(*) FROM evidence WHERE attempt_id='attempt-12' AND kind IN ('attempt_exit','telemetry_lost','worktree_owner_protocol')) + (SELECT COUNT(*) FROM intents WHERE attempt_id='attempt-12' AND kind='supervisor_dispatch')",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(forged, 0);
    for suffix in [
        "child.json",
        "receipt.json",
        "dispatch.json",
        "plan.json",
        "stop.sock",
    ] {
        assert!(
            !store
                .root()
                .join("attempts")
                .join(format!("attempt-12.{suffix}"))
                .exists()
        );
    }
}

fn assert_alias_refuses_cycle(f: &Fixture, original: &str, before: &History) {
    let alias = f.root.path().join("state-alias");
    symlink("state", &alias).unwrap();
    let direct_db = fs::metadata(f.config.state_root.join("state.sqlite3")).unwrap();
    let alias_db = fs::metadata(alias.join("state.sqlite3")).unwrap();
    assert_eq!(
        (direct_db.dev(), direct_db.ino()),
        (alias_db.dev(), alias_db.ino())
    );
    assert_eq!(fs::canonicalize(&alias).unwrap(), f.config.state_root);
    let mut config = f.config.clone();
    config.state_root = alias;
    crate::supervisor::validate_stop_socket_path(&config.state_root, "attempt-fixture-id").unwrap();
    fs::write(&f.calls, "").unwrap();
    let mut launcher = RecordingLauncher::default();
    let result = f.cycle(&config, &mut launcher);
    assert_eq!(
        result.unwrap_err().to_string(),
        "daemon scheduling failed; inspect local task state"
    );
    assert!(launcher.plans.is_empty());
    assert!(!f.assigned.exists());
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(!calls.contains("-X POST"));
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    assert_eq!(history(&store, None), *before);
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_saved_attempt(&store, original);
    assert_eq!(
        fs::read_dir(store.root().join("attempts")).unwrap().count(),
        0
    );
}

fn assert_direct_cycle_schedules_331(f: &Fixture, original: &str, before: &History) {
    fs::write(&f.calls, "").unwrap();
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&f.config, &mut launcher).unwrap().unwrap();
    assert_eq!(report.startup.attempts.len(), 1);
    let old = &report.startup.attempts[0];
    assert_eq!(old.task_id, "task-12");
    assert_eq!(old.attempt_id, "attempt-12");
    assert!(matches!(old.review, AttemptReview::Held(_)));
    assert!(report.startup.source_holds.is_empty());
    assert!(!report.capacity_full);
    assert_eq!(report.launched, launcher.plans);
    assert_eq!(launcher.plans.len(), 1);
    assert_eq!(launcher.plans[0].task_id, "task-fixture-id");
    assert_eq!(launcher.plans[0].executable, f.config.initial.executable);
    assert!(!launcher.plans[0].executable.exists());
    assert!(f.assigned.exists() && f.assigned12.exists());
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.contains("-X POST"))
            .count(),
        1
    );
    assert!(calls.contains("-X POST repos/org/tracker/issues/331/assignees"));
    assert!(!calls.contains("issues/12"));
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 2);
    assert_eq!(history(&store, Some("task-12")), *before);
    assert_saved_attempt(&store, original);
    let selection = task_records::selection_evidence(&store, "task-fixture-id")
        .unwrap()
        .unwrap();
    assert_eq!(selection.candidate.issue_number, 331);
    assert_eq!(
        launches::launch_intent(&store, "attempt-fixture-id")
            .unwrap()
            .unwrap(),
        serde_json::to_string(&launcher.plans[0]).unwrap()
    );
    assert_eq!(
        fs::read_dir(store.root().join("attempts")).unwrap().count(),
        0
    );
}

#[test]
fn execute_targeted_issue_launches_once_with_hermetic_gh_and_fake_launcher() {
    let f = Fixture::new();
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&f.config, &mut launcher).unwrap().unwrap();
    assert_eq!(report.launched.len(), 1);
    assert_eq!(launcher.plans.len(), 1);
    assert!(f.assigned.exists());
    assert!(!f.assigned12.exists());
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert!(calls.contains("issues/331"));
    assert!(!calls.contains("issues/12"));
    assert!(!calls.contains("issues/7"));
}

#[test]
fn alias_cycle_refuses_then_direct_cycle_holds_issue12_and_launches_331() {
    let f = Fixture::new();
    let original = seed_held_issue12(&f);
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    let all_before = history(&store, None);
    let old_before = history(&store, Some("task-12"));
    assert_saved_attempt(&store, &original);
    drop(store);
    assert_alias_refuses_cycle(&f, &original, &all_before);
    assert_direct_cycle_schedules_331(&f, &original, &old_before);
}
