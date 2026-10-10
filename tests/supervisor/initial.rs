use super::*;
use luthor::WorktreeOwner;
use luthor::state::{launches, scheduling, task_records};

pub(crate) fn initial_prompt_distinguishes_issue_assignee_from_author() {
    let dir = tempfile::tempdir().unwrap();
    let (mut config, mut candidate) = configured(dir.path());
    config.assignment_login = "issue-agent".into();
    config.mappings[0].allowed_pr_author = "acoliver".into();
    candidate.mapping = config.mappings[0].clone();
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    let plan = prepare_initial(&mut store, "task", "attempt-1").unwrap();
    let prompt = plan.args.windows(2).find(|pair| pair[0] == "-p").unwrap()[1].as_str();
    assert!(
        prompt.contains("authorized PR author is acoliver"),
        "{prompt}"
    );
    assert!(
        prompt.contains("tracker issue is assigned to issue-agent"),
        "{prompt}"
    );
    assert!(
        !prompt.contains("authorized PR author is issue-agent"),
        "{prompt}"
    );
}

pub(crate) fn intent_and_slot_survive_restart_and_failed_dispatch_stays_held() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    let plan = prepare_initial(&mut store, "task", "attempt-1").unwrap();
    let ownership = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    assert_eq!(plan.session_id, "task");
    assert!(plan.args.iter().any(|arg| arg.contains("attempt-1")));
    let initial_prompt = plan.args.windows(2).find(|pair| pair[0] == "-p").unwrap()[1].as_str();
    for required in [
        "Work only in code repository org/code",
        "mapped base branch main",
        "PR head in repository org/code on branch luthor/task, pushed to remote git@github.com:org/code.git",
        "Tracker-Issue: https://github.com/org/tracker/issues/7",
        "authorized PR author is operator",
        "already claimed; do not reassign it",
        "Create only an open PR",
        "Report the PR URL and ID",
    ] {
        assert!(initial_prompt.contains(required), "missing {required}");
    }
    assert_eq!(
        plan.args
            .iter()
            .filter(|arg| arg.as_str() == "-p" || arg.as_str() == "--prompt")
            .count(),
        1
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        execute_with_binary(
            &mut store,
            &plan,
            &dir.path().join("absent-luthor"),
            &ownership
        )
        .is_err()
    );
    assert!(
        execute_with_binary(
            &mut store,
            &plan,
            &dir.path().join("absent-luthor"),
            &ownership
        )
        .is_err()
    );
    assert!(!plan.executable.exists());
    drop(store);

    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    let persisted = launches::launch_intent(&store, "attempt-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<luthor::supervisor::LaunchPlan>(&persisted).unwrap(),
        plan
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(matches!(
        scheduling::release_reservation(&mut store, "attempt-1"),
        Err(StateError::LaunchBlocked)
    ));
    assert!(prepare_initial(&mut store, "task", "attempt-2").is_err());
    let mut other = candidate.clone();
    other.issue_node_id = "other-issue".into();
    other.issue_number = 8;
    other.issue_url = "https://github.com/org/tracker/issues/8".into();
    task_records::create_task(&mut store, "other-task", &other, "rev", &config).unwrap();
    assert!(matches!(
        scheduling::reserve(&mut store, "other-task", "other-attempt"),
        Err(StateError::Capacity { .. })
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}

pub(crate) fn unverified_worktree_or_missing_session_cannot_reserve_or_launch() {
    let dir = tempfile::tempdir().unwrap();
    let (mut config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    task_records::create_task(&mut store, "task", &candidate, "rev", &config).unwrap();
    assert!(prepare_initial(&mut store, "task", "attempt-1").is_err());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    drop(store);
    config.initial.args = vec![
        "--cwd".into(),
        "{worktree}".into(),
        "-p".into(),
        "prompt".into(),
    ];
    let mut store = StateStore::open(
        dir.path()
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .join("other-state"),
        1,
    )
    .unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    assert!(matches!(
        prepare_initial(&mut store, "task", "attempt-1"),
        Err(SupervisorError::Conflict)
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
}

pub(crate) fn branch_switch_before_initial_does_not_reserve_or_launch() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    git(
        &config.worktree_root.join("task"),
        &["switch", "-c", "foreign"],
    );
    assert!(prepare_initial(&mut store, "task", "attempt-1").is_err());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert!(
        launches::launch_intent(&store, "attempt-1")
            .unwrap()
            .is_none()
    );
    assert!(
        !config
            .state_root
            .join("attempts/attempt-1.child.json")
            .exists()
    );
}

pub(crate) fn changed_worktree_identity_blocks_before_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    fs::rename(
        dir.path().join("worktrees/task"),
        dir.path().join("old-worktrees"),
    )
    .unwrap();
    fs::create_dir(dir.path().join("worktrees/task")).unwrap();
    assert!(prepare_initial(&mut store, "task", "attempt-1").is_err());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
}

#[cfg(unix)]
pub(crate) fn partial_ready_handshake_times_out_and_keeps_reserved_slot() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (_config, mut store, plan, _) = prepared_fake_worker(&dir);
    let ownership = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    let binary = dir.path().join("fake-supervisor");
    fs::write(&binary, "#!/bin/sh\nprintf 'R'\nsleep 8\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let started = Instant::now();
    assert!(matches!(
        execute_with_binary(&mut store, &plan, &binary, &ownership),
        Err(SupervisorError::ReadyTimeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(7));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}
