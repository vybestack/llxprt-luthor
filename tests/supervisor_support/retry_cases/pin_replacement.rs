use super::*;
use crate::pin_replacement_fixture as fixture;
use luthor::state::{journal, launches, pin_replacement, scheduling, task_records};

#[test]
fn repin_next_retry_dispatches_new_binary_and_multiple_pin_eras_remain_verifiable() {
    let (_dir, mut config, mut store) = retry_fixture();
    let selection = task_records::selection_evidence(&store, "task")
        .unwrap()
        .unwrap();
    let first = launches::launch_intent(&store, "attempt-real")
        .unwrap()
        .unwrap();
    let old_files = fs::read(config.state_root.join("attempts/attempt-real.plan.json")).unwrap();
    let build = fixture::replacement(&mut config, &mut store);
    let old =
        pin_replacement::replace(&mut store, &config, "task", fixture::request(&build)).unwrap();
    fs::remove_file(&old.previous_worker_build.path).unwrap();
    drop(store);
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    let mut projects = OtherProject(selection.candidate.clone(), 1);
    let mut prs = ExitPr::default();
    let mut launcher = OtherLauncher::default();
    let next = retry(&mut store, &config, &mut projects, &mut prs, &mut launcher).unwrap();
    assert_eq!(launcher.0, 1);
    assert_eq!(next.worker_build, Some(build.clone()));
    assert_eq!(next.executable, build.path);
    assert_eq!(next.session_id, "task");
    complete(&config, &mut store, &next, &mut projects, &mut prs);
    assert!(next.worktree.join("replacement-ran").exists());
    second_repin_and_retry(
        &mut config,
        &mut store,
        &build,
        &mut projects,
        &mut prs,
        &mut launcher,
    );
    assert_eq!(
        launches::launch_intent(&store, "attempt-real")
            .unwrap()
            .unwrap(),
        first
    );
    assert_eq!(
        fs::read(config.state_root.join("attempts/attempt-real.plan.json")).unwrap(),
        old_files
    );
    assert_eq!(
        task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap(),
        selection
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert_eq!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .filter(|kind| *kind == "worker_pin_replaced")
            .count(),
        2
    );
}

fn second_repin_and_retry(
    config: &mut Config,
    store: &mut StateStore,
    build: &luthor::worker_build::WorkerBuild,
    projects: &mut OtherProject,
    prs: &mut ExitPr,
    launcher: &mut OtherLauncher,
) {
    let path = build.path.with_extension("second");
    let second = fixture::build(config, &path, "printf '{\"session_state_versions\":[3]}'");
    let audit = pin_replacement::replace(store, config, "task", fixture::request(&second)).unwrap();
    assert_eq!(&audit.previous_worker_build, build);
    assert_eq!(audit.historical_attempts, ["attempt-real", "attempt-retry"]);
    fs::remove_file(&build.path).unwrap();
    let third = luthor::coordinator::retry_one(
        store,
        luthor::coordinator::RetryDependencies {
            task_id: "task",
            previous_attempt_id: "attempt-retry",
            attempt_id: "attempt-third",
            config,
            config_revision: "third",
            actor: "operator",
            reason: "second protocol fix",
            revalidate_terminal_exit: false,
            projects,
            prs,
            launcher,
        },
    )
    .unwrap();
    assert_eq!(third.worker_build, Some(second));
    complete(config, store, &third, projects, prs);
}

fn complete(
    config: &Config,
    store: &mut StateStore,
    plan: &luthor::supervisor::LaunchPlan,
    projects: &mut OtherProject,
    prs: &mut ExitPr,
) {
    let owner = luthor::WorktreeOwner::acquire_existing(store.root(), "task").unwrap();
    execute_with_binary(store, plan, Path::new(env!("CARGO_BIN_EXE_luthor")), &owner).unwrap();
    drop(owner);
    crate::worker_exit::await_dispatched_worker_exit(config, "task", &plan.attempt_id);
    let ready =
        journal::evidence_payloads(store, "task", &plan.attempt_id, "supervisor_ready").unwrap();
    let ready: serde_json::Value = serde_json::from_str(&ready[0]).unwrap();
    wait_for_process_and_group_absence(ready["pid"].as_i64().unwrap() as i32);
    assert!(matches!(
        luthor::coordinator::reconcile_with_pr(store, "task", &plan.attempt_id, projects, prs)
            .unwrap(),
        Reconciliation::Completed {
            exit_code: Some(2),
            signal: None
        }
    ));
}

#[test]
fn repin_audit_and_hash_checks_are_required_before_retry_dispatch() {
    for scenario in ["deleted-audit", "changed-hash", "unattested-audit"] {
        let (_dir, mut config, mut store) = retry_fixture();
        let build = fixture::replacement(&mut config, &mut store);
        pin_replacement::replace(&mut store, &config, "task", fixture::request(&build)).unwrap();
        let selection = task_records::selection_evidence(&store, "task")
            .unwrap()
            .unwrap();
        let next = retry(
            &mut store,
            &config,
            &mut OtherProject(selection.candidate, 1),
            &mut ExitPr::default(),
            &mut OtherLauncher::default(),
        )
        .unwrap();
        match scenario {
            "deleted-audit" => {
                fixture::db(&config)
                    .execute("DELETE FROM evidence WHERE kind='worker_pin_replaced'", [])
                    .unwrap();
            }
            "unattested-audit" => {
                fixture::db(&config).execute("UPDATE evidence SET payload=json_set(payload,'$.capability.session_state_versions',json('[4]')) WHERE kind='worker_pin_replaced'", []).unwrap();
            }
            "changed-hash" => fs::write(&build.path, "#!/bin/sh\nexit 99\n").unwrap(),
            _ => unreachable!(),
        }
        let owner = luthor::WorktreeOwner::acquire_existing(store.root(), "task").unwrap();
        assert!(
            execute_with_binary(
                &mut store,
                &next,
                Path::new(env!("CARGO_BIN_EXE_luthor")),
                &owner
            )
            .is_err(),
            "{scenario}"
        );
        assert!(!next.worktree.join("replacement-ran").exists());
        assert!(
            !config
                .state_root
                .join("attempts/attempt-retry.plan.json")
                .exists()
        );
        assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    }
}
