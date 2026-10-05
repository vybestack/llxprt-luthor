mod amended_coordinator;
mod amendment;
mod authorization;
mod production;
mod reads;
mod storage;
use super::{FakeGithub, FakeLauncher, FakePr, FakeWriter, Fixture, git};
use luthor::coordinator::{
    ContinuationDependencies, ContinuationLocalInspector, ContinuationProcessInspector,
    ContinuationRefusal as Refusal, ContinuationResult, OsContinuationLocalInspector,
    ProcessInspectionError, continue_never_dispatched,
};
use luthor::{
    coordinator::SupervisorLauncher,
    state::StateStore,
    supervisor::{LaunchPlan, SupervisorError},
};
use luthor::{state::NeverDispatchedContext, supervisor::SessionEnvironment};
use std::fs;

struct Local {
    environment: SessionEnvironment,
    fail: Option<Refusal>,
    changed_on_recheck: bool,
    sql_on_recheck: Option<&'static str>,
    inspections: usize,
}
impl ContinuationLocalInspector for Local {
    fn session_environment(&mut self) -> Result<SessionEnvironment, Refusal> {
        if self.fail == Some(Refusal::EnvironmentUnavailable) {
            return Err(Refusal::EnvironmentUnavailable);
        }
        Ok(self.environment.clone())
    }
    fn inspect(&mut self, context: &NeverDispatchedContext) -> Result<(), Refusal> {
        self.inspections += 1;
        if let Some(reason) = self.fail {
            return Err(reason);
        }
        if self.changed_on_recheck && self.inspections == 2 {
            return Err(Refusal::WorktreeChanged);
        }
        if self.inspections == 2
            && let Some(sql) = self.sql_on_recheck
        {
            let db = rusqlite::Connection::open(
                context
                    .selection()
                    .effective_config
                    .state_root
                    .join("state.sqlite3"),
            )
            .unwrap();
            db.execute_batch(sql).unwrap();
        }
        OsContinuationLocalInspector.verify_worktree(context)?;
        OsContinuationLocalInspector.inspect_artifacts(context)
    }
}
#[derive(Default)]
struct Processes {
    failure: Option<ProcessInspectionError>,
    changed_on_recheck: bool,
    calls: usize,
}
impl ContinuationProcessInspector for Processes {
    fn inspect(&mut self, _: &NeverDispatchedContext) -> Result<(), ProcessInspectionError> {
        self.calls += 1;
        if self.changed_on_recheck && self.calls == 2 {
            return Err(ProcessInspectionError::Conflict);
        }
        self.failure.map_or(Ok(()), Err)
    }
}
#[derive(Default)]
struct Launcher {
    plans: Vec<LaunchPlan>,
    fail: bool,
}
impl SupervisorLauncher for Launcher {
    fn launch(&mut self, store: &mut StateStore, plan: &LaunchPlan) -> Result<(), SupervisorError> {
        let audits = store
            .evidence_payloads("task-a", "attempt-task-a", "never_dispatched_authorized")
            .unwrap();
        assert_eq!(audits.len(), 1, "audit must be durable before launch");
        assert_eq!(store.reservation_count().unwrap(), 1);
        assert_eq!(
            *plan,
            serde_json::from_str::<LaunchPlan>(
                &store.launch_intent("attempt-task-a").unwrap().unwrap()
            )
            .unwrap()
        );
        self.plans.push(plan.clone());
        if self.fail {
            Err(SupervisorError::ExecutionUnavailable)
        } else {
            Ok(())
        }
    }
}
struct Lane {
    f: Fixture,
    github: FakeGithub,
    local: Local,
    processes: Processes,
    launcher: Launcher,
    plan: LaunchPlan,
    read_scenario: &'static str,
}
impl Lane {
    fn new() -> Self {
        let mut f = Fixture::new(1);
        let c = f.candidate.clone();
        let mut github = FakeGithub::new(&c);
        let plan = f
            .run(
                "task-a",
                &c,
                &mut github,
                &mut FakeWriter::default(),
                &mut FakeLauncher::default(),
            )
            .unwrap();
        github.reads = 0;
        github.assigned = true;
        github.prs = FakePr::default();
        Self {
            local: Local {
                environment: plan.session_environment.clone(),
                fail: None,
                changed_on_recheck: false,
                sql_on_recheck: None,
                inspections: 0,
            },
            processes: Processes::default(),
            launcher: Launcher::default(),
            f,
            github,
            plan,
            read_scenario: "absent",
        }
    }
    fn run(&mut self) -> ContinuationResult {
        let mut prs = std::mem::take(&mut self.github.prs);
        let result = continue_never_dispatched(
            &mut self.f.store,
            ContinuationDependencies {
                task_id: "task-a",
                attempt_id: "attempt-task-a",
                actor: "bot",
                config: &self.f.config,
                config_revision: "revision",
                projects: &mut reads::Projects(&mut self.github, self.read_scenario),
                prs: &mut reads::Prs(&mut prs, self.read_scenario),
                local: &mut self.local,
                processes: &mut self.processes,
                launcher: &mut self.launcher,
            },
        )
        .unwrap();
        self.github.prs = prs;
        result
    }
    fn held(&mut self, reason: Refusal) {
        assert_eq!(self.run(), ContinuationResult::Held(reason));
        assert!(self.launcher.plans.is_empty());
        assert_eq!(self.f.store.reservation_count().unwrap(), 1);
        assert_eq!(
            self.f.store.latest_attempt("task-a").unwrap().as_deref(),
            Some("attempt-task-a")
        );
        assert_eq!(
            self.f.store.task_phase("task-a").unwrap().as_deref(),
            Some("held")
        );
        assert!(
            self.f
                .store
                .evidence_payloads("task-a", "attempt-task-a", "never_dispatched_authorized")
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn continuation_audits_then_launches_exact_saved_plan_once_with_same_reservation() {
    let mut lane = Lane::new();
    let saved = lane
        .f
        .store
        .launch_intent("attempt-task-a")
        .unwrap()
        .unwrap();
    assert_eq!(
        lane.run(),
        ContinuationResult::Dispatched(Box::new(lane.plan.clone()))
    );
    assert_eq!(lane.launcher.plans, [lane.plan.clone()]);
    assert_eq!(
        lane.f
            .store
            .launch_intent("attempt-task-a")
            .unwrap()
            .unwrap(),
        saved
    );
    assert_eq!(lane.local.inspections, 2);
    assert_eq!(lane.processes.calls, 2);
    assert_eq!(lane.run(), ContinuationResult::Held(Refusal::Ineligible));
    assert_eq!(lane.launcher.plans.len(), 1);
    assert_eq!(lane.f.store.reservation_count().unwrap(), 1);
    for kind in ["exit_pr_lookup", "child_registered", "supervisor_ready"] {
        assert!(
            lane.f
                .store
                .evidence_payloads("task-a", "attempt-task-a", kind)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn continuation_requires_full_config_revision_and_session_environment() {
    for drift in [
        "initial",
        "resume",
        "source",
        "mapping",
        "capacity",
        "state_root",
        "environment",
        "revision",
    ] {
        let mut lane = Lane::new();
        match drift {
            "initial" => lane.f.config.initial.args.push("changed".into()),
            "resume" => lane.f.config.resume.args.push("changed".into()),
            "source" => lane.f.config.sources[0].milestone = None,
            "mapping" => lane.f.config.mappings[0].push_remote = "other".into(),
            "capacity" => lane.f.config.capacity = 2,
            "state_root" => lane.f.config.state_root = lane.f.dir.path().join("other"),
            "environment" => lane.local.environment.xdg_state_home = Some("/different".into()),
            "revision" => {
                let db =
                    rusqlite::Connection::open(lane.f.store.root().join("state.sqlite3")).unwrap();
                db.execute_batch("UPDATE tasks SET config_revision='other'; UPDATE evidence SET payload=json_set(payload,'$.config_revision','other') WHERE kind='selection'; UPDATE intents SET detail=json_set(detail,'$.config_revision','other') WHERE kind='launch';").unwrap();
            }
            _ => unreachable!(),
        }
        lane.held(if drift == "environment" {
            Refusal::EnvironmentChanged
        } else {
            Refusal::ConfigChanged
        });
    }
}

#[test]
fn continuation_claim_drift_and_local_external_failures_keep_slot() {
    let mut lane = Lane::new();
    lane.github.change_on = Some(1);
    lane.held(Refusal::ClaimChanged);
    for reason in [
        Refusal::StorageUnavailable,
        Refusal::ArtifactConflict,
        Refusal::WorktreeChanged,
        Refusal::EnvironmentUnavailable,
        Refusal::ExecutableUnavailable,
    ] {
        let mut lane = Lane::new();
        lane.local.fail = Some(reason);
        lane.held(reason);
    }
    for (error, reason) in [
        (ProcessInspectionError::Conflict, Refusal::ProcessConflict),
        (
            ProcessInspectionError::Unavailable,
            Refusal::ProcessUnavailable,
        ),
    ] {
        let mut lane = Lane::new();
        lane.processes.failure = Some(error);
        lane.held(reason);
    }
}

#[test]
fn continuation_rechecks_mutable_local_proofs_before_audit() {
    let mut lane = Lane::new();
    lane.local.changed_on_recheck = true;
    lane.held(Refusal::WorktreeChanged);
    let mut lane = Lane::new();
    lane.processes.changed_on_recheck = true;
    lane.held(Refusal::ProcessConflict);
}

#[test]
fn continuation_changed_or_dirty_worktree_refuses_read_only() {
    for mutation in ["dirty", "untracked", "head", "remote", "branch"] {
        let mut lane = Lane::new();
        let path = &lane.plan.worktree;
        match mutation {
            "dirty" => fs::write(path.join("README"), "changed").unwrap(),
            "untracked" => fs::write(path.join("new-file"), "changed").unwrap(),
            "head" => git(path, &["commit", "--allow-empty", "-m", "changed"]),
            "remote" => git(
                path,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "git@github.com:org/other.git",
                ],
            ),
            "branch" => git(path, &["checkout", "-b", "other"]),
            _ => unreachable!(),
        }
        lane.held(Refusal::WorktreeChanged);
    }
}

#[test]
#[cfg(unix)]
fn continuation_entire_attempt_artifact_namespace_including_broken_symlinks_refuses() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for suffix in [
        "",
        ".plan.json",
        ".child.json",
        ".child.child.tmp",
        ".stop.sock",
        ".stdout.log",
        ".unknown",
        "-unexpected",
        ".broken",
    ] {
        let mut lane = Lane::new();
        let attempts = lane.f.store.root().join("attempts");
        let path = attempts.join(format!("attempt-task-a{suffix}"));
        if suffix == ".broken" {
            symlink("missing", &path).unwrap();
        } else {
            fs::write(&path, "conflict").unwrap();
        }
        lane.held(Refusal::ArtifactConflict);
        assert!(fs::symlink_metadata(path).is_ok());
    }
    let mut lane = Lane::new();
    fs::set_permissions(
        lane.f.store.root().join("attempts"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    lane.held(Refusal::StorageUnavailable);
}

#[test]
fn continuation_dispatch_marker_and_launch_failure_cannot_reauthorize() {
    let mut lane = Lane::new();
    lane.f
        .store
        .begin_supervision(
            "task-a",
            "attempt-task-a",
            &serde_json::to_string(&lane.plan).unwrap(),
        )
        .unwrap();
    lane.held(Refusal::Ineligible);
    let mut lane = Lane::new();
    lane.launcher.fail = true;
    assert_eq!(lane.run(), ContinuationResult::Held(Refusal::LaunchFailed));
    assert_eq!(lane.f.store.reservation_count().unwrap(), 1);
    assert_eq!(lane.run(), ContinuationResult::Held(Refusal::Ineligible));
    assert_eq!(lane.launcher.plans.len(), 1);
}
