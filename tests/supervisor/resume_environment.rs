use super::*;
use luthor::state::{launches, scheduling, task_records};

#[cfg(unix)]
pub(crate) fn resume_environment_mismatch_child() {
    let Ok(root) = std::env::var("LUTHOR_RESUME_STATE_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let task = std::env::var("LUTHOR_RESUME_TASK").unwrap();
    let attempt = std::env::var("LUTHOR_RESUME_ATTEMPT").unwrap();
    let mut store = StateStore::open(&root, 1).unwrap();
    assert!(matches!(
        prepare_resume(&mut store, &task, &attempt),
        Err(SupervisorError::Conflict)
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert_eq!(
        task_records::latest_attempt(&store, &task)
            .unwrap()
            .as_deref(),
        Some("attempt-real")
    );
    assert!(launches::launch_intent(&store, &attempt).unwrap().is_none());
}

#[cfg(unix)]
pub(crate) fn resume_rejects_different_root_environment_before_reservation_in_child_process() {
    let (_dir, config, store, initial) = paused_fixture();
    let root = config.state_root.clone();
    let home_b = tempfile::tempdir().unwrap();
    assert_ne!(
        initial.session_environment.home,
        fs::canonicalize(home_b.path()).unwrap()
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    drop(store);

    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("resume_environment_mismatch_child")
        .arg("--ignored")
        .env_clear()
        .env("HOME", home_b.path())
        .env("XDG_CONFIG_HOME", home_b.path().join("config"))
        .env("LUTHOR_RESUME_STATE_ROOT", &root)
        .env("LUTHOR_RESUME_TASK", "task")
        .env("LUTHOR_RESUME_ATTEMPT", "attempt-next")
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(unix)]
pub(crate) fn branch_switch_before_resume_does_not_reserve_or_launch() {
    let (_dir, config, mut store, initial) = paused_fixture();
    git(&initial.worktree, &["switch", "-c", "foreign"]);
    assert!(prepare_resume(&mut store, "task", "attempt-next").is_err());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    assert_eq!(
        task_records::latest_attempt(&store, "task")
            .unwrap()
            .as_deref(),
        Some("attempt-real")
    );
    assert!(
        launches::launch_intent(&store, "attempt-next")
            .unwrap()
            .is_none()
    );
    assert!(
        !config
            .state_root
            .join("attempts/attempt-next.child.json")
            .exists()
    );
}
