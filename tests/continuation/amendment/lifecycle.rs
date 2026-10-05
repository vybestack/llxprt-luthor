use luthor::state::{journal, launches, scheduling, task_records};
mod future_templates;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod gates;
mod live_cli;
mod observation;
mod pr_completion;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod safety_launch;
mod safety_recovery;
use super::{Lane, amend, database, fixture};
use luthor::{
    state::verify_amended_worker_plan,
    supervisor::{self, LaunchPlan, Reconciliation, RecoveryInspection},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn dispatch_count(lane: &Lane) -> usize {
    database(lane)
        .query_row(
            "SELECT COUNT(*) FROM intents WHERE kind='supervisor_dispatch'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn executable_lane() -> Lane {
    executable_lane_from(fixture())
}

fn executable_lane_from(mut lane: Lane) -> Lane {
    let executable = lane.f.dir.path().join("llxprt-code-rs");
    fs::write(&executable, "#!/bin/sh\nprintf '%s\\000' \"$@\"\nexit 17\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    lane.plan.executable = executable.clone();
    lane.f.config.initial.executable = executable.clone();
    let db = database(&lane);
    let mut selection = task_records::selection_evidence(&lane.f.store, "task-a")
        .unwrap()
        .unwrap();
    selection.effective_config.initial.executable = executable;
    db.execute(
        "UPDATE evidence SET payload=?1 WHERE kind='selection'",
        [serde_json::to_string(&selection).unwrap()],
    )
    .unwrap();
    db.execute(
        "UPDATE intents SET detail=?1 WHERE kind='launch'",
        [serde_json::to_string_pretty(&lane.plan).unwrap()],
    )
    .unwrap();
    amend(&mut lane).unwrap();
    lane
}

fn launch(lane: &mut Lane, binary: &Path) -> Result<(), supervisor::SupervisorError> {
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    supervisor::execute_amended_with_binary(
        &mut lane.f.store,
        &context,
        &lane.f.config,
        "corrected-revision",
        binary,
    )
}

fn await_receipt(lane: &Lane) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let path = lane
        .f
        .store
        .root()
        .join("attempts/attempt-task-a.receipt.json");
    while !path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        path.exists(),
        "supervisor log: {:?}",
        fs::read_to_string(
            lane.f
                .store
                .root()
                .join("attempts/attempt-task-a.supervisor.log")
        )
    );
}

#[test]
fn amended_real_worker_uses_exact_effective_argv_and_reconciles_without_rewriting_history() {
    let mut lane = executable_lane();
    let original = launches::launch_intent(&lane.f.store, "attempt-task-a")
        .unwrap()
        .unwrap();
    launch(&mut lane, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    await_receipt(&lane);
    let path = lane
        .f
        .store
        .root()
        .join("attempts/attempt-task-a.plan.json");
    let plan: LaunchPlan = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let mut expected = lane.plan.clone();
    expected.args.drain(2..4);
    assert_eq!(plan, expected);
    let argv: Vec<u8> = expected
        .args
        .iter()
        .flat_map(|arg| arg.bytes().chain([0]))
        .collect();
    assert_eq!(
        fs::read(
            lane.f
                .store
                .root()
                .join("attempts/attempt-task-a.stdout.log")
        )
        .unwrap(),
        argv
    );
    assert_eq!(
        supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(17),
            signal: None
        }
    );
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 0);
    assert_eq!(
        launches::launch_intent(&lane.f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        original
    );
    assert!(verify_amended_worker_plan(&database(&lane), lane.f.store.root(), &plan).is_err());
    assert_eq!(
        supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(17),
            signal: None
        }
    );
    assert!(
        supervisor::execute_with_binary(
            &mut lane.f.store,
            &plan,
            Path::new(env!("CARGO_BIN_EXE_luthor"))
        )
        .is_err()
    );
    assert!(
        lane.f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .is_err()
    );
}

#[test]
fn amended_missing_supervisor_keeps_committed_dispatch_and_reservation_without_exit() {
    let mut lane = executable_lane();
    assert!(launch(&mut lane, Path::new("missing-supervisor")).is_err());
    assert_eq!(dispatch_count(&lane), 1);
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
    assert!(matches!(
        supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
        Reconciliation::Held { .. }
    ));
    assert!(matches!(
        supervisor::inspect_recovery_quiescence(&lane.f.store, "task-a", "attempt-task-a").unwrap(),
        RecoveryInspection::Held(_)
    ));
    assert!(
        journal::evidence_payloads(&lane.f.store, "task-a", "attempt-task-a", "attempt_exit")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn amended_launch_rechecks_config_revision_and_audit_before_artifacts_or_spawn() {
    for mutation in ["config", "revision", "audit"] {
        let mut lane = executable_lane();
        let context = lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap();
        if mutation == "config" {
            lane.f.config.resume.args.push("changed".into());
        }
        if mutation == "audit" {
            database(&lane)
                .execute(
                    "DELETE FROM evidence WHERE kind='initial_branch_removed'",
                    [],
                )
                .unwrap();
        }
        let revision = if mutation == "revision" {
            "wrong"
        } else {
            "corrected-revision"
        };
        assert!(
            supervisor::execute_amended_with_binary(
                &mut lane.f.store,
                &context,
                &lane.f.config,
                revision,
                Path::new(env!("CARGO_BIN_EXE_luthor"))
            )
            .is_err()
        );
        assert_eq!(dispatch_count(&lane), 0);
        assert!(
            !lane
                .f
                .store
                .root()
                .join("attempts/attempt-task-a.plan.json")
                .exists()
        );
        assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
    }
}
