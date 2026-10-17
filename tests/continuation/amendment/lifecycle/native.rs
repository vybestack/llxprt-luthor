mod completion;
mod config;
mod gates;
mod refusals;
mod tampering;
use super::{Lane, amend, database, launch};
use luthor::{
    coordinator::{
        NativeContinuationDependencies, SupervisorLauncher, continue_amended, reconcile_with_pr,
    },
    state::{StateStore, launches, native_continuation::Mode, scheduling, task_records},
    supervisor::{self, LaunchPlan, Reconciliation, SupervisorError},
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn fixture(stopped: bool) -> Lane {
    let mut lane = super::super::future_templates::dual_fixture();
    let executable = lane.f.dir.path().join("llxprt-code-rs");
    fs::write(
        &executable,
        "#!/bin/sh\nexec /bin/sh \"$0.behavior\" \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let started = lane.f.dir.path().join("started");
    let running = format!(
        "#!/bin/sh\ntouch '{}'\nwhile [ -d '{}' ]; do /bin/sleep 0.1; done\n",
        started.display(),
        lane.f.dir.path().display()
    );
    fs::write(
        executable.with_extension("behavior"),
        if stopped {
            &running
        } else {
            "#!/bin/sh\nprintf '%s\\000' \"$@\"\nexit 17\n"
        },
    )
    .unwrap();
    lane.plan.executable = executable.clone();
    lane.plan.worker_build =
        Some(luthor::worker_build::WorkerBuild::capture(&executable, &lane.plan.worktree).unwrap());
    lane.f.config.initial.executable = executable.clone();
    lane.f.config.resume.executable = executable.clone();
    let mut selection = task_records::selection_evidence(&lane.f.store, "task-a")
        .unwrap()
        .unwrap();
    selection.effective_config.initial.executable = executable.clone();
    selection.effective_config.resume.executable = executable;
    database(&lane)
        .execute(
            "UPDATE evidence SET payload=?1 WHERE kind='selection'",
            [serde_json::to_string(&selection).unwrap()],
        )
        .unwrap();
    database(&lane)
        .execute(
            "UPDATE intents SET detail=?1 WHERE kind='launch'",
            [serde_json::to_string(&lane.plan).unwrap()],
        )
        .unwrap();
    amend(&mut lane).unwrap();
    let result = launch(&mut lane, Path::new(env!("CARGO_BIN_EXE_luthor")));
    if result.is_err() {
        eprintln!(
            "initial log: {:?}",
            fs::read_to_string(
                lane.f
                    .store
                    .root()
                    .join("attempts/attempt-task-a.supervisor.log")
            )
        );
    }
    result.unwrap();
    if stopped {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !started.exists() {
            assert!(std::time::Instant::now() < deadline, "worker did not start");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    if stopped {
        supervisor::request_stop(&mut lane.f.store, "task-a", "attempt-task-a").unwrap();
    }
    settle(&mut lane, "attempt-task-a");
    fs::write(
        lane.plan.executable.with_extension("behavior"),
        "#!/bin/sh\nprintf '%s\\000' \"$@\"\nexit 17\n",
    )
    .unwrap();
    lane
}

fn settle(lane: &mut Lane, attempt: &str) {
    crate::coordinator::waiting::receipt(lane.f.store.root(), attempt);
    crate::coordinator::waiting::owner_release(lane.f.store.root(), "task-a");
    assert!(matches!(
        reconcile_with_pr(
            &mut lane.f.store,
            "task-a",
            attempt,
            &mut lane.github,
            &mut crate::FakePr::default()
        )
        .unwrap(),
        Reconciliation::Completed { .. }
    ));
}
struct Launcher {
    count: usize,
    execute: bool,
}
impl SupervisorLauncher for Launcher {
    fn launch(
        &mut self,
        store: &mut StateStore,
        plan: &LaunchPlan,
        owner: &luthor::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        self.count += 1;
        if self.execute {
            let result = supervisor::execute_with_binary(
                store,
                plan,
                Path::new(env!("CARGO_BIN_EXE_luthor")),
                owner,
            );
            if result.is_err() {
                for suffix in ["supervisor.log", "supervisor-error.json"] {
                    eprintln!(
                        "{suffix}: {:?}",
                        fs::read_to_string(
                            store
                                .root()
                                .join(format!("attempts/{}.{}", plan.attempt_id, suffix))
                        )
                    );
                }
            }
            result
        } else {
            Err(SupervisorError::ExecutionUnavailable)
        }
    }
}
fn run(
    lane: &mut Lane,
    mode: Mode,
    previous: &str,
    attempt: &str,
    scenario: &'static str,
    launcher: &mut Launcher,
) -> Result<LaunchPlan, luthor::coordinator::DispatchError> {
    let mut prs = std::mem::take(&mut lane.github.prs);
    let result = continue_amended(
        &mut lane.f.store,
        NativeContinuationDependencies {
            task_id: "task-a",
            previous_attempt_id: previous,
            attempt_id: attempt,
            mode,
            config: &lane.f.config,
            config_revision: "corrected-revision",
            actor: "bot",
            projects: &mut super::super::super::reads::Projects(&mut lane.github, scenario),
            prs: &mut super::super::super::reads::Prs(&mut prs, scenario),
            launcher,
        },
    );
    lane.github.prs = prs;
    result
}

#[test]
fn native_continuation_stopped_resume_and_natural_retry_launch_once_with_own_audit() {
    for (stopped, mode) in [(true, Mode::StoppedResume), (false, Mode::NaturalExitRetry)] {
        let mut lane = fixture(stopped);
        let original = launches::launch_intent(&lane.f.store, "attempt-task-a")
            .unwrap()
            .unwrap();
        assert!(launches::resume_context(&lane.f.store, "task-a").is_err());
        assert!(
            luthor::state::retry_context_for_task(&lane.f.store, "task-a", "attempt-task-a")
                .is_err()
        );
        let mut launcher = Launcher {
            count: 0,
            execute: true,
        };
        let plan = run(
            &mut lane,
            mode,
            "attempt-task-a",
            "attempt-second",
            "ok",
            &mut launcher,
        )
        .unwrap();
        assert!(!plan.args.iter().any(|a| a.starts_with("--branch")));
        assert!(
            plan.args
                .iter()
                .any(|a| a.contains("inspect the files left in the worktree"))
        );
        assert_eq!(plan.session_id, "task-a");
        assert_eq!(plan.worker_build, lane.plan.worker_build);
        settle(&mut lane, "attempt-second");
        assert_eq!(
            launches::launch_intent(&lane.f.store, "attempt-task-a")
                .unwrap()
                .unwrap(),
            original
        );
        assert!(
            run(
                &mut lane,
                mode,
                "attempt-task-a",
                "attempt-third",
                "ok",
                &mut launcher
            )
            .is_err()
        );
        assert!(
            run(
                &mut lane,
                mode,
                "attempt-second",
                "attempt-second",
                "ok",
                &mut launcher
            )
            .is_err()
        );
        assert_eq!(launcher.count, 1);
        let next = run(
            &mut lane,
            Mode::NaturalExitRetry,
            "attempt-second",
            "attempt-third",
            "ok",
            &mut launcher,
        )
        .unwrap();
        assert_ne!(plan.args, next.args);
        settle(&mut lane, "attempt-third");
        assert_eq!(launcher.count, 2);
        assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
    }
}
