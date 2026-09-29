use luthor::{
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    eligibility::Candidate,
    state::{StateError, StateStore, WorktreeIdentity, WorktreeIntent},
    supervisor::{SupervisorError, execute, prepare_initial},
};
use std::path::Path;

fn configured(root: &Path) -> (Config, Candidate) {
    let mapping = Mapping {
        tracker_repository: "org/tracker".into(),
        code_repository: "org/code".into(),
        checkout: root.into(),
        base_branch: "main".into(),
        push_remote: "origin".into(),
        allowed_pr_head_repository: "org/code".into(),
        allowed_pr_author: "operator".into(),
    };
    let source = Source {
        project_id: "project".into(),
        repositories: vec!["org/tracker".into()],
        ready_marker: Marker::Label {
            name: "ready".into(),
        },
        milestone: None,
    };
    let initial = CommandTemplate {
        executable: root.join("worker-that-must-not-run"),
        args: vec![
            "--session".into(),
            "{task.id}".into(),
            "--cwd".into(),
            "{worktree}".into(),
            "-p".into(),
            "Work on {task.issue_url} for {attempt.id}".into(),
        ],
    };
    let config = Config {
        state_root: root.join("state"),
        worktree_root: root.join("worktrees"),
        capacity: 1,
        assignment_login: "operator".into(),
        sources: vec![source.clone()],
        mappings: vec![mapping.clone()],
        initial: initial.clone(),
        resume: initial,
    };
    let candidate = Candidate {
        project_id: "project".into(),
        item_id: "item".into(),
        repository: "org/tracker".into(),
        issue_node_id: "issue".into(),
        issue_number: 7,
        issue_url: "https://github.com/org/tracker/issues/7".into(),
        tracker_repo_id: "repo".into(),
        milestone_id: None,
        milestone_title: None,
        observed_at_unix_secs: 1,
        observed_state: "open".into(),
        observed_assignees: vec![],
        observed_labels: vec!["ready".into()],
        observed_project_fields: vec![],
        marker: source.ready_marker.clone(),
        mapping,
        source,
    };
    (config, candidate)
}

fn claimed(store: &mut StateStore, config: &Config, candidate: &Candidate, root: &Path) {
    store.create_task("task", candidate, "rev", config).unwrap();
    store
        .record_claim_intent("task", "operator", "org/tracker", 7)
        .unwrap();
    store
        .record_evidence("task", None, "claim_verified", "sole assignee")
        .unwrap();
    store.set_task_phase("task", "claimed").unwrap();
    let path = root.join("worktrees");
    std::fs::create_dir_all(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let intent = WorktreeIntent {
        path: path.clone(),
        branch: "luthor/task".into(),
        base: "main".into(),
        repository: "org/code".into(),
    };
    store.begin_worktree("task", &intent).unwrap();
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(&path).unwrap();
    let identity = WorktreeIdentity {
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
        branch: intent.branch.clone(),
        base: intent.base.clone(),
        head: "abc".into(),
        repository: intent.repository.clone(),
        git_directory: root.into(),
        remote: "origin".into(),
    };
    store.finish_worktree("task", &identity).unwrap();
}

#[test]
fn intent_and_slot_survive_restart_but_never_execute_or_release() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    let plan = prepare_initial(&mut store, "task", "attempt-1").unwrap();
    assert_eq!(plan.session_id, "task");
    assert!(plan.args.iter().any(|arg| arg.contains("attempt-1")));
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(matches!(
        execute(&plan),
        Err(SupervisorError::ExecutionUnavailable)
    ));
    assert!(!plan.executable.exists());
    drop(store);

    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    let persisted = store.launch_intent("attempt-1").unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<luthor::supervisor::LaunchPlan>(&persisted).unwrap(),
        plan
    );
    assert_eq!(store.reservation_count().unwrap(), 1);
    assert!(matches!(
        store.release_reservation("attempt-1"),
        Err(StateError::LaunchBlocked)
    ));
    assert!(prepare_initial(&mut store, "task", "attempt-2").is_err());
    let mut other = candidate.clone();
    other.issue_node_id = "other-issue".into();
    other.issue_number = 8;
    other.issue_url = "https://github.com/org/tracker/issues/8".into();
    store
        .create_task("other-task", &other, "rev", &config)
        .unwrap();
    assert!(matches!(
        store.reserve("other-task", "other-attempt"),
        Err(StateError::Capacity { .. })
    ));
    assert_eq!(store.reservation_count().unwrap(), 1);
}

#[test]
fn unverified_worktree_or_missing_session_cannot_reserve_or_launch() {
    let dir = tempfile::tempdir().unwrap();
    let (mut config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    store
        .create_task("task", &candidate, "rev", &config)
        .unwrap();
    assert!(prepare_initial(&mut store, "task", "attempt-1").is_err());
    assert_eq!(store.reservation_count().unwrap(), 0);
    drop(store);

    config.initial.args = vec![
        "--cwd".into(),
        "{worktree}".into(),
        "-p".into(),
        "prompt".into(),
    ];
    let mut store = StateStore::open(dir.path().join("other-state"), 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    assert!(matches!(
        prepare_initial(&mut store, "task", "attempt-1"),
        Err(SupervisorError::Conflict)
    ));
    assert_eq!(store.reservation_count().unwrap(), 0);
}

#[test]
fn changed_worktree_identity_blocks_before_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let (config, candidate) = configured(dir.path());
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed(&mut store, &config, &candidate, dir.path());
    std::fs::rename(
        dir.path().join("worktrees"),
        dir.path().join("old-worktrees"),
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("worktrees")).unwrap();
    assert!(prepare_initial(&mut store, "task", "attempt-1").is_err());
    assert_eq!(store.reservation_count().unwrap(), 0);
}
