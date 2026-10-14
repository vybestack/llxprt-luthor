use super::super::*;
use luthor::state::{journal, scheduling, task_records};
use luthor::{
    config::{RetryPolicy, TransientFailure},
    coordinator::{ScheduleError, ScheduleReport},
};
use std::os::unix::fs::PermissionsExt;

pub(super) struct Fixture {
    pub dir: tempfile::TempDir,
    pub config: Config,
    pub store: StateStore,
    pub projects: OtherProject,
    pub prs: ExitPr,
    pub launcher: Launcher,
    pub ids: OtherIds,
}

pub(super) struct Launcher {
    pub plans: Vec<luthor::supervisor::LaunchPlan>,
    pub real: bool,
    pub fail: bool,
}
impl SupervisorLauncher for Launcher {
    fn launch(
        &mut self,
        store: &mut StateStore,
        plan: &luthor::supervisor::LaunchPlan,
        owner: &luthor::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        self.plans.push(plan.clone());
        if self.fail {
            return Err(SupervisorError::ExecutionUnavailable);
        }
        if self.real {
            execute_with_binary(store, plan, Path::new(env!("CARGO_BIN_EXE_luthor")), owner)?;
        }
        Ok(())
    }
}

impl Fixture {
    pub fn new(code: &str, exit: i32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (config, candidate) = configured(dir.path());
        let worker = &config.initial.executable;
        let status = if exit == 0 { "ok" } else { "error" };
        let fields = if exit == 0 {
            "\"summary\":\"missing handoff; please ask a human\"".into()
        } else {
            format!(
                "\"error\":{{\"code\":\"{code}\",\"message\":\"body read failed 429 503 timeout\"}}"
            )
        };
        fs::write(worker, format!("#!/bin/sh\nsession=''\nwhile test \"$#\" -gt 0; do\n  if test \"$1\" = --session; then session=\"$2\"; shift; fi\n  shift\ndone\nprintf '{{{fields},\"session_id\":\"%s\",\"status\":\"{status}\"}}\\n' \"$session\"\nexit {exit}\n")).unwrap();
        fs::set_permissions(worker, fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
        claimed(&mut store, &config, &candidate, dir.path());
        let plan = prepare_initial(&mut store, "task", "attempt-real").unwrap();
        let owner = luthor::WorktreeOwner::acquire(store.root(), "task").unwrap();
        execute_with_binary(
            &mut store,
            &plan,
            Path::new(env!("CARGO_BIN_EXE_luthor")),
            &owner,
        )
        .unwrap();
        drop(owner);
        let mut f = Self {
            dir,
            config,
            store,
            projects: OtherProject(candidate, 1),
            prs: ExitPr::default(),
            launcher: Launcher {
                plans: vec![],
                real: false,
                fail: false,
            },
            ids: OtherIds::default(),
        };
        f.settle("attempt-real");
        f.config.retry_policy = RetryPolicy::Bounded {
            max_attempts: 3,
            backoff_secs: 60,
            classes: vec![
                TransientFailure::RateLimit,
                TransientFailure::TransientServer,
                TransientFailure::Connectivity,
            ],
        };
        f
    }

    pub fn settle(&mut self, attempt: &str) {
        crate::worker_exit::await_dispatched_worker_exit(&self.config, "task", attempt);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let result = luthor::coordinator::reconcile_with_pr(
                &mut self.store,
                "task",
                attempt,
                &mut self.projects,
                &mut self.prs,
            )
            .unwrap();
            if task_records::task_phase(&self.store, "task")
                .unwrap()
                .as_deref()
                == Some("attention")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "exit not accounted for: {result:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let db = self.db();
        let ready: String = db
            .query_row(
                "SELECT payload FROM evidence WHERE attempt_id=?1 AND kind='supervisor_ready'",
                [attempt],
                |row| row.get(0),
            )
            .unwrap();
        let ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
        wait_for_process_and_group_absence(ready["pid"].as_i64().unwrap() as i32);
        assert_eq!(scheduling::reservation_count(&self.store).unwrap(), 0);
    }

    pub fn schedule(&mut self) -> Result<ScheduleReport, ScheduleError> {
        schedule_candidates(
            &mut self.store,
            vec![],
            ScheduleDependencies {
                config: &self.config,
                config_revision: "auto-policy",
                projects: &mut self.projects,
                prs: &mut self.prs,
                assignments: &mut NoAssignments,
                launcher: &mut self.launcher,
                ids: &mut self.ids,
            },
        )
    }

    /// Model a terminal observation from an earlier poll, without sleeping or
    /// changing the configured backoff/production clock.
    pub fn elapsed(&mut self, attempt: &str, class: TransientFailure) {
        let observation = serde_json::json!({"outcome":{"kind":"transient","class":class},"observed_at_unix_secs":1}).to_string();
        let db = self.db();
        let changed = db
            .execute(
                "UPDATE evidence SET payload=?1 WHERE attempt_id=?2 AND kind='worker_outcome'",
                [&observation, attempt],
            )
            .unwrap();
        if changed == 0 {
            journal::record_evidence(
                &mut self.store,
                "task",
                Some(attempt),
                "worker_outcome",
                &observation,
            )
            .unwrap();
        }
    }

    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.config.state_root.join("state.sqlite3")).unwrap()
    }
}

struct NoAssignments;
impl AssignmentWriter for NoAssignments {
    fn assign(&mut self, _: &str, _: u64, _: &str) -> Result<(), AssignmentError> {
        panic!("automatic retry must not assign")
    }
}
