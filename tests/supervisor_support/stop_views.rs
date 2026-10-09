use super::*;
use luthor::state::{journal, launches, scheduling, task_records};
mod continuation;

#[cfg(unix)]
pub(crate) fn assert_natural_stop_views_and_restart(
    config: &Config,
    plan: &luthor::supervisor::LaunchPlan,
    receipt: &[u8],
    prs: &mut ExitPr,
) {
    let config_path = config.state_root.join("config.json");
    fs::write(&config_path, serde_json::to_vec(config).unwrap()).unwrap();
    let resume = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(["resume", "task", "--config"])
        .arg(&config_path)
        .arg("--execute")
        .output()
        .unwrap();
    assert!(!resume.status.success());
    assert!(String::from_utf8_lossy(&resume.stderr).contains("resume held: task is not resumable"));

    let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_natural_stop_cached_views(config, plan);
    let launch: luthor::supervisor::LaunchPlan = serde_json::from_str(
        &launches::launch_intent(&store, "attempt-real")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(&launch, plan);
    assert!(!task_records::has_attempt(&store, "task", "attempt-next").unwrap());
    let kinds = journal::evidence_kinds(&store, "task").unwrap();
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "attempt_exit").count(),
        1
    );
    assert!(!kinds.contains(&"independent_stop_signal".into()));
    assert!(!kinds.contains(&"independent_stop_decision".into()));
    assert_eq!(fs::read(receipt_path(config)).unwrap(), receipt);
    let mut projects = OtherProject(
        task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap()
            .candidate,
        1,
    );
    assert!(matches!(
        luthor::coordinator::reconcile_with_pr(
            &mut store,
            "task",
            "attempt-real",
            &mut projects,
            prs
        )
        .unwrap(),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(prs.reads, 1);
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("attention")
    );
}

#[cfg(unix)]
pub(crate) fn assert_crashed_supervisor_reservation(config: &Config, mut store: StateStore) {
    let kinds = journal::evidence_kinds(&store, "task").unwrap();
    assert!(kinds.contains(&"independent_stop_decision".into()));
    assert!(kinds.contains(&"independent_stop_signal".into()));
    assert!(kinds.contains(&"independent_group_absent".into()));
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "live worker identity or reservation unverified"
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    drop(store);
    let store = StateStore::open(&config.state_root, 1).unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"independent_group_absent".into())
    );
}

#[cfg(unix)]
pub(crate) fn assert_natural_stop_cached_views(
    config: &Config,
    plan: &luthor::supervisor::LaunchPlan,
) {
    let view = |args: &[&str]| -> serde_json::Value {
        let output = luthor::cli::execute(
            &config.state_root,
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap();
        serde_json::from_str(&output).unwrap()
    };
    let status = view(&["status"]);
    let shown = view(&["show", "task"]);
    assert_eq!(status["tasks"].as_array().unwrap().len(), 1);
    let task = &status["tasks"][0];
    assert_eq!(task["phase"], "attention");
    assert_eq!(shown["phase"], task["phase"]);
    assert_eq!(task["latest_attempt_id"], "attempt-real");
    assert_eq!(shown["latest_attempt_id"], task["latest_attempt_id"]);
    assert_eq!(task["latest_attempt_lifecycle"], "completed");
    assert_eq!(
        shown["attempts"][0]["lifecycle"],
        task["latest_attempt_lifecycle"]
    );
    assert_eq!(
        task["latest_attempt_outcome"],
        "exit_code=Some(7);signal=None"
    );
    assert_eq!(
        shown["latest_attempt_outcome"],
        task["latest_attempt_outcome"]
    );
    assert_eq!(task["reserved_slot"], false);
    assert_eq!(shown["reserved_slot"], task["reserved_slot"]);
    assert_eq!(status["capacity"]["reserved"], 0);
    assert_eq!(shown["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(shown["attempts"][0]["reservation"], "released");
    assert_eq!(shown["session"], plan.session_id);
    assert_eq!(
        shown["worktree"],
        serde_json::to_value(&plan.expected_worktree).unwrap()
    );
    let claims: Vec<_> = shown["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "claim_verified")
        .collect();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0]["detail"], config.assignment_login);
}

pub(crate) mod completion;
mod identity_races;
pub(crate) use completion::{assert_claim_hold, completion_fixture};
