use super::{Case, amendment};
use luthor::state::{launches, scheduling, task_records};
use luthor::{
    coordinator::{AmendedSupervisorLauncher, ContinuationResult},
    state::{NeverDispatchedContext, StateStore},
    supervisor::{self, LaunchPlan, Reconciliation, SupervisorError},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

struct Gated;
impl AmendedSupervisorLauncher for Gated {
    fn launch_amended(
        &mut self,
        store: &mut StateStore,
        context: &NeverDispatchedContext,
        config: &luthor::config::Config,
        revision: &str,
        owner: &luthor::WorktreeOwner,
    ) -> Result<(), SupervisorError> {
        supervisor::execute_amended_with_binary(
            store,
            context,
            config,
            revision,
            Path::new(env!("CARGO_BIN_EXE_luthor")),
            owner,
        )
    }
}

fn executable_case() -> Case {
    let mut case = Case::new();
    let lane = &mut case.lane;
    let executable = lane.f.dir.path().join("llxprt-code-rs");
    fs::write(&executable, "#!/bin/sh\nprintf '%s\\000' \"$@\"\nexit 17\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    lane.plan.executable = executable.clone();
    lane.f.config.initial.executable = executable.clone();
    let mut selection = task_records::selection_evidence(&lane.f.store, "task-a")
        .unwrap()
        .unwrap();
    selection.effective_config.initial.executable = executable;
    let db = amendment::database(lane);
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
    case
}

fn await_receipt(case: &Case) {
    let root = case.lane.f.store.root();
    let receipt = root.join("attempts/attempt-task-a.receipt.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !receipt.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        receipt.exists(),
        "{:?}",
        fs::read_to_string(root.join("attempts/attempt-task-a.supervisor.log"))
    );
}

#[test]
fn amended_coordinator_real_gated_worker_gets_only_audited_effective_argv() {
    let mut case = executable_case();
    let original = launches::launch_intent(&case.lane.f.store, "attempt-task-a")
        .unwrap()
        .unwrap();
    let mut effective = case.lane.plan.clone();
    effective.args.drain(2..4);
    assert_eq!(
        case.run_with(&mut Gated),
        ContinuationResult::Dispatched(Box::new(effective.clone()))
    );
    await_receipt(&case);
    let root = case.lane.f.store.root();
    let artifact: LaunchPlan =
        serde_json::from_slice(&fs::read(root.join("attempts/attempt-task-a.plan.json")).unwrap())
            .unwrap();
    assert_eq!(artifact, effective);
    let argv: Vec<u8> = effective
        .args
        .iter()
        .flat_map(|arg| arg.bytes().chain([0]))
        .collect();
    assert_eq!(
        fs::read(root.join("attempts/attempt-task-a.stdout.log")).unwrap(),
        argv
    );
    assert_eq!(
        supervisor::reconcile_attempt(&mut case.lane.f.store, "task-a", "attempt-task-a").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(17),
            signal: None
        }
    );
    assert_eq!(
        scheduling::reservation_count(&case.lane.f.store).unwrap(),
        0
    );
    assert_eq!(
        launches::launch_intent(&case.lane.f.store, "attempt-task-a")
            .unwrap()
            .unwrap(),
        original
    );
    assert_eq!(
        case.run_with(&mut Gated),
        ContinuationResult::Held(super::Refusal::Ineligible)
    );
    assert_eq!(
        super::count(&case.lane.f.store, "evidence", "initial_branch_removed"),
        1
    );
    assert_eq!(
        super::count(&case.lane.f.store, "intents", "supervisor_dispatch"),
        1
    );
}
