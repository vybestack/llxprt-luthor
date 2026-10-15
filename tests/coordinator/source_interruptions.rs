//! Real SIGKILL boundaries leave only durable source-operation history.
use super::{FakeGithub, FakeLauncher, FakePr, FakeWriter, FixedIds, Fixture, git};
use luthor::{
    config::Config,
    coordinator::{
        ScheduleDependencies, abandon_task, schedule_candidates_after_startup,
        startup_reconcile_all,
    },
    eligibility::Candidate,
    github::project::{Issue, Page, ProjectItem, ProjectReadError, ProjectReader},
    state::{StateStore, WorktreeIntent, journal, task_records, worktree_records},
    supervisor, worktree,
};
use std::{fs, os::unix::process::ExitStatusExt, process::Command};

const CHILD_CONFIG: &str = "LUTHOR_ISSUE67_CHILD_CONFIG";
const CHILD_STAGE: &str = "LUTHOR_ISSUE67_CHILD_STAGE";

pub(crate) fn interrupted_source_child() {
    let Some(path) = std::env::var_os(CHILD_CONFIG) else {
        return;
    };
    let (config, candidate): (Config, Candidate) =
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let stage: usize = std::env::var(CHILD_STAGE).unwrap().parse().unwrap();
    let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
    task_records::create_task(&mut store, "interrupted", &candidate, "revision", &config).unwrap();
    checkpoint(stage, 0);
    task_records::record_claim_intent(
        &mut store,
        "interrupted",
        "bot",
        &candidate.repository,
        candidate.issue_number,
    )
    .unwrap();
    checkpoint(stage, 1);
    journal::record_evidence(&mut store, "interrupted", None, "claim_verified", "bot").unwrap();
    checkpoint(stage, 2);
    task_records::set_task_phase(&mut store, "interrupted", "claimed").unwrap();
    let base = git_output(&candidate.mapping.checkout, &["rev-parse", "HEAD"]);
    let intent = WorktreeIntent {
        path: config.worktree_root.join("interrupted"),
        branch: "luthor/interrupted".into(),
        base: candidate.mapping.base_branch.clone(),
        base_commit: Some(base),
        repository: candidate.mapping.code_repository.clone(),
    };
    worktree_records::begin_worktree(&mut store, "interrupted", &intent).unwrap();
    checkpoint(stage, 3);
    fs::create_dir_all(&config.worktree_root).unwrap();
    git(
        &candidate.mapping.checkout,
        &[
            "worktree",
            "add",
            "-b",
            &intent.branch,
            intent.path.to_str().unwrap(),
            intent.base_commit.as_deref().unwrap(),
        ],
    );
    checkpoint(stage, 4);
    worktree::ensure_worktree(
        &mut store,
        "interrupted",
        &config.worktree_root,
        &candidate.mapping,
    )
    .unwrap();
    checkpoint(stage, 5);
    supervisor::prepare_initial(&mut store, "interrupted", "attempt-interrupted").unwrap();
    checkpoint(stage, 6);
    panic!("unknown checkpoint");
}

fn checkpoint(selected: usize, current: usize) {
    if selected == current {
        assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGKILL) }, 0);
        unreachable!("SIGKILL returned");
    }
}

fn git_output(path: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().into()
}

fn kill_at(f: &mut Fixture, stage: usize) {
    let config_path = f.dir.path().join("child.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&(&f.config, &f.candidate)).unwrap(),
    )
    .unwrap();
    // Drop the coordinator lock before the independent killed execute cycle.
    let replacement = StateStore::open(f.dir.path().join("unused"), 2).unwrap();
    drop(std::mem::replace(&mut f.store, replacement));
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "issue67_interrupted_source_child", "--nocapture"])
        .env(CHILD_CONFIG, config_path)
        .env(CHILD_STAGE, stage.to_string())
        .output()
        .unwrap();
    assert_eq!(out.status.signal(), Some(9), "{stage}: {out:?}");
    f.store = StateStore::open(&f.config.state_root, f.config.capacity).unwrap();
}

struct TwoIssues {
    first: FakeGithub,
    second: FakeGithub,
}
impl ProjectReader for TwoIssues {
    fn dependency(
        &mut self,
        _: &luthor::config::IssueReference,
    ) -> Result<luthor::dependencies::DependencyState, ProjectReadError> {
        panic!("unexpected dependency read")
    }
    fn page(
        &mut self,
        id: &str,
        cursor: Option<&str>,
    ) -> Result<Page<ProjectItem>, ProjectReadError> {
        let mut page = self.first.page(id, cursor)?;
        page.items.extend(self.second.page(id, cursor)?.items);
        Ok(page)
    }
    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        if item.issue_number == self.first.candidate.issue_number {
            self.first.issue(item)
        } else {
            self.second.issue(item)
        }
    }
}

fn next_candidate(f: &Fixture) -> Candidate {
    let mut candidate = f.candidate.clone();
    candidate.issue_node_id = "issue-2".into();
    candidate.item_id = "item-2".into();
    candidate.issue_number = 2;
    candidate.issue_url = "https://github.com/org/tracker/issues/2".into();
    candidate
}

pub(crate) fn every_killed_source_boundary_recovers_or_holds_only_its_task() {
    for stage in 0..=6 {
        let mut f = Fixture::new(2);
        kill_at(&mut f, stage);
        let other = next_candidate(&f);
        let mut projects = TwoIssues {
            first: FakeGithub::new(&f.candidate),
            second: FakeGithub::new(&other),
        };
        projects.first.assigned = stage >= 2;
        let mut prs = FakePr::default();
        let startup = startup_reconcile_all(&mut f.store, &mut projects, &mut prs).unwrap();
        assert!(!startup.scheduling_blocked(), "stage {stage}: {startup:?}");
        let mut assignments = FakeWriter::default();
        let mut launcher = FakeLauncher::default();
        let report = schedule_candidates_after_startup(
            &mut f.store,
            vec![other],
            ScheduleDependencies {
                config: &f.config,
                config_revision: "revision",
                projects: &mut projects,
                prs: &mut prs,
                assignments: &mut assignments,
                launcher: &mut launcher,
                ids: &mut FixedIds::default(),
            },
            startup,
        )
        .unwrap();
        assert_eq!(
            assignments.calls, 1,
            "only the other issue may be assigned at stage {stage}"
        );
        assert!(
            report
                .launched
                .iter()
                .any(|plan| plan.task_id != "interrupted")
        );
        if (2..=5).contains(&stage) {
            assert!(
                report
                    .launched
                    .iter()
                    .any(|plan| plan.task_id == "interrupted")
            );
            assert_single_proofs(&f);
        } else {
            assert!(
                !report
                    .launched
                    .iter()
                    .any(|plan| plan.task_id == "interrupted")
            );
            assert_eq!(
                task_records::task_phase(&f.store, "interrupted")
                    .unwrap()
                    .as_deref(),
                Some("held")
            );
            abandon_task(&mut f.store, "interrupted", "operator", "interrupted cycle").unwrap();
            assert_eq!(
                task_records::task_phase(&f.store, "interrupted")
                    .unwrap()
                    .as_deref(),
                Some("abandoned")
            );
            assert!(
                task_records::latest_attempt(&f.store, "interrupted")
                    .unwrap()
                    .is_none()
                    || stage == 6
            );
        }
    }
}

fn assert_single_proofs(f: &Fixture) {
    let conn = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    for kind in ["claim_verified", "worktree_created"] {
        let count: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM evidence WHERE task_id='interrupted' AND kind=?1",
                [kind],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "{kind}");
    }
}

pub(crate) fn observable_claim_without_completion_is_confirmed_once() {
    let mut f = Fixture::new(1);
    kill_at(&mut f, 1);
    let mut projects = FakeGithub::new(&f.candidate);
    projects.assigned = true;
    for _ in 0..2 {
        let report =
            luthor::coordinator::reconcile_source(&mut f.store, "interrupted", &mut projects)
                .unwrap();
        assert_eq!(report.status, "verified");
        assert!(!report.reasons.contains(&"claim_intent_unverified"));
        assert_single_proofs(&f);
    }
    assert!(
        task_records::latest_attempt(&f.store, "interrupted")
            .unwrap()
            .is_none()
    );
}

pub(crate) fn partial_wrong_or_dirty_worktrees_hold_without_modification() {
    for change in [
        "partial",
        "branch",
        "commit",
        "tracked",
        "untracked",
        "symlink",
        "foreign",
        "absent_branch",
    ] {
        let mut f = Fixture::new(2);
        kill_at(
            &mut f,
            if change == "partial" || change == "foreign" {
                3
            } else {
                4
            },
        );
        let path = f.config.worktree_root.join("interrupted");
        corrupt_worktree(&f, &path, change);
        let mut projects = FakeGithub::new(&f.candidate);
        projects.assigned = true;
        let report =
            luthor::coordinator::reconcile_source(&mut f.store, "interrupted", &mut projects)
                .unwrap();
        assert_eq!(report.status, "held", "{change}: {report:?}");
        assert!(
            report.reasons.contains(&"worktree_unverified"),
            "{change}: {report:?}"
        );
        let record = worktree_records::worktree_record(&f.store, "interrupted")
            .unwrap()
            .unwrap();
        assert!(record.identity.is_none(), "{change}");
        assert!(
            task_records::latest_attempt(&f.store, "interrupted")
                .unwrap()
                .is_none()
        );
        assert!(luthor::state::scheduling::ensure_dispatch_capacity(&f.store).is_ok());
        abandon_task(&mut f.store, "interrupted", "operator", change).unwrap();
        if change == "tracked" {
            assert_eq!(
                fs::read_to_string(path.join("README")).unwrap(),
                "operator work"
            );
        }
        if change == "untracked" {
            assert_eq!(
                fs::read_to_string(path.join("local")).unwrap(),
                "operator work"
            );
        }
        if change == "partial" {
            assert_eq!(
                fs::read_to_string(path.join("partial")).unwrap(),
                "operator work"
            );
        }
    }
}

fn corrupt_worktree(f: &Fixture, path: &std::path::Path, change: &str) {
    match change {
        "partial" => {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("partial"), "operator work").unwrap();
        }
        "branch" => git(path, &["switch", "-c", "operator"]),
        "commit" => git(path, &["commit", "--allow-empty", "-m", "operator commit"]),
        "tracked" => fs::write(path.join("README"), "operator work").unwrap(),
        "untracked" => fs::write(path.join("local"), "operator work").unwrap(),
        "symlink" => {
            let moved = f.dir.path().join("moved");
            fs::rename(path, &moved).unwrap();
            std::os::unix::fs::symlink(moved, path).unwrap();
        }
        "foreign" => {
            fs::create_dir_all(path).unwrap();
            git(path, &["init", "-b", "luthor/interrupted"]);
            git(path, &["config", "user.name", "Fixture"]);
            git(path, &["config", "user.email", "fixture@example.org"]);
            git(path, &["commit", "--allow-empty", "-m", "foreign"]);
        }
        "absent_branch" => {
            fs::remove_dir_all(path).unwrap();
        }
        _ => unreachable!(),
    }
}

pub(crate) fn prelaunch_abandon_is_audited_and_allows_reselection() {
    for phase in ["preparing", "selected", "claimed"] {
        let mut f = Fixture::new(1);
        kill_at(&mut f, 0);
        task_records::set_task_phase(&mut f.store, "interrupted", phase).unwrap();
        abandon_task(&mut f.store, "interrupted", "alice", "wrong scope").unwrap();
        assert!(
            !task_records::existing_issue(
                &f.store,
                &f.candidate.tracker_repo_id,
                &f.candidate.issue_node_id
            )
            .unwrap()
        );
        assert!(
            !task_records::existing_target(
                &f.store,
                &f.candidate.repository,
                f.candidate.issue_number
            )
            .unwrap()
        );
        luthor::state::scheduling::ensure_dispatch_capacity(&f.store).unwrap();
        task_records::create_task(
            &mut f.store,
            "replacement",
            &f.candidate,
            "revision",
            &f.config,
        )
        .unwrap();
        assert!(
            task_records::create_task(
                &mut f.store,
                "duplicate",
                &f.candidate,
                "revision",
                &f.config
            )
            .is_err()
        );
        let conn = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
        let audit: String = conn
            .query_row(
                "SELECT payload FROM evidence WHERE task_id='interrupted' AND kind='task_retired'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let audit: serde_json::Value = serde_json::from_str(&audit).unwrap();
        assert_eq!(audit["actor"], "alice");
        assert_eq!(audit["reason"], "wrong scope");
        assert_eq!(audit["previous_phase"], phase);
        assert_eq!(audit["evidence"]["github_changes"], "none");
        assert!(audit["observed_at_unix_secs"].as_u64().unwrap() > 0);
        assert_eq!(task_records::task_count(&f.store).unwrap(), 2);
    }
}

pub(crate) fn unresolved_stop_remains_global_and_dispatched_attempt_cannot_be_abandoned() {
    let mut f = Fixture::new(2);
    kill_at(&mut f, 6);
    journal::record_stop_intent(&mut f.store, "interrupted", "attempt-interrupted").unwrap();
    let mut projects = FakeGithub::new(&f.candidate);
    projects.assigned = true;
    let startup =
        startup_reconcile_all(&mut f.store, &mut projects, &mut FakePr::default()).unwrap();
    assert!(startup.scheduling_blocked());
    assert!(abandon_task(&mut f.store, "interrupted", "operator", "unsafe").is_err());
    assert_eq!(
        luthor::state::scheduling::reservation_count(&f.store).unwrap(),
        1
    );
}
