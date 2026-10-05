use super::*;
use luthor::state::{launches, scheduling, task_records, worktree_records};

#[cfg(unix)]
pub(crate) fn paused_attempt_prepares_distinct_continuation_after_reopen() {
    let (_dir, config, store, initial) = paused_fixture();
    drop(store);
    let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
    let plan = prepare_resume(&mut store, "task", "attempt-next").unwrap();
    assert_eq!(plan.task_id, initial.task_id);
    assert_eq!(plan.session_id, initial.session_id);
    assert_eq!(plan.worktree, initial.worktree);
    assert_eq!(plan.config_revision, initial.config_revision);
    assert_eq!(plan.attempt_id, "attempt-next");
    let resume_prompt = plan
        .args
        .windows(2)
        .find(|pair| pair[0] == "--prompt")
        .unwrap()[1]
        .as_str();
    assert!(
        resume_prompt
            .starts_with("Continue https://github.com/org/tracker/issues/7 for attempt-next")
    );
    assert!(resume_prompt.contains("Tracker-Issue: https://github.com/org/tracker/issues/7"));
    assert!(resume_prompt.contains("already claimed; do not reassign it"));
    assert_eq!(
        plan.args
            .iter()
            .filter(|arg| arg.as_str() == "-p" || arg.as_str() == "--prompt")
            .count(),
        1
    );
    assert_eq!(
        task_records::latest_attempt(&store, "task")
            .unwrap()
            .as_deref(),
        Some("attempt-next")
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert_eq!(
        serde_json::from_str::<luthor::supervisor::LaunchPlan>(
            &launches::launch_intent(&store, "attempt-real")
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        initial
    );
    assert_eq!(
        serde_json::from_str::<luthor::supervisor::LaunchPlan>(
            &launches::launch_intent(&store, "attempt-next")
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        plan
    );
    drop(store);
    let store = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(
        launches::launch_intent(&store, "attempt-next")
            .unwrap()
            .unwrap(),
        serde_json::to_string(&plan).unwrap()
    );
}

#[cfg(unix)]
pub(crate) fn minimal_resume_template_gets_interrupted_worktree_inspection_guidance() {
    let (_dir, config, mut store, initial) =
        paused_fixture_with_resume_prompt("Continue {attempt.id}");
    let plan = prepare_resume(&mut store, "task", "attempt-next").unwrap();
    let rendered = plan
        .args
        .windows(2)
        .find(|pair| pair[0] == "--prompt")
        .unwrap()[1]
        .as_str();
    assert!(rendered.starts_with("Continue attempt-next"));
    assert!(
        rendered
            .contains("inspect the files left in the worktree by the interrupted or canceled turn")
    );
    assert!(rendered.contains("Do not assume its transcript was restored"));
    assert!(rendered.contains("Tracker-Issue: https://github.com/org/tracker/issues/7"));
    assert!(rendered.contains("already claimed; do not reassign it"));
    assert_eq!(plan.attempt_id, "attempt-next");
    assert_eq!(plan.session_id, initial.session_id);
    assert_eq!(plan.session_id, "task");
    assert_eq!(
        task_records::latest_attempt(&store, "task")
            .unwrap()
            .as_deref(),
        Some("attempt-next")
    );
    assert!(config.state_root.exists());
}

#[cfg(unix)]
pub(crate) fn resume_rejects_same_final_prompt_but_allows_distinct_rendered_attempt() {
    let (_dir, _config, mut store, _) =
        paused_fixture_with_resume_prompt("Continue {task.issue_url}");
    let first = prepare_resume(&mut store, "task", "attempt-next").unwrap();
    let first_prompt = first
        .args
        .windows(2)
        .find(|pair| pair[0] == "--prompt")
        .unwrap()[1]
        .clone();
    let latest = first.clone();
    assert!(matches!(
        ensure_distinct_resume_prompt(&first, &latest, &first.args),
        Err(SupervisorError::Conflict)
    ));

    let (_dir, _config, mut distinct_store, _) =
        paused_fixture_with_resume_prompt("Continue {task.issue_url} for {attempt.id}");
    let distinct = prepare_resume(&mut distinct_store, "task", "attempt-next").unwrap();
    let distinct_prompt = distinct
        .args
        .windows(2)
        .find(|pair| pair[0] == "--prompt")
        .unwrap()[1]
        .clone();
    assert_ne!(first_prompt, distinct_prompt);
    assert!(ensure_distinct_resume_prompt(&first, &first, &distinct.args).is_ok());
}

#[cfg(unix)]
pub(crate) fn resume_requires_paused_verified_exit_and_unused_attempt_id() {
    let (_dir, config, mut store, _) = paused_fixture();
    assert!(prepare_resume(&mut store, "task", "attempt-real").is_err());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    task_records::set_task_phase(&mut store, "task", "held").unwrap();
    assert!(prepare_resume(&mut store, "task", "fresh").is_err());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
    task_records::set_task_phase(&mut store, "task", "paused").unwrap();
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    db.execute(
        "DELETE FROM evidence WHERE kind='attempt_exit' AND task_id='task'",
        [],
    )
    .unwrap();
    assert!(prepare_resume(&mut store, "task", "fresh").is_err());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
}

#[cfg(unix)]
pub(crate) fn resume_rejects_same_prompt_and_tampered_session_or_inode_before_reservation() {
    for alteration in ["same-prompt", "session", "inode"] {
        let (_dir, config, mut store, initial) = paused_fixture();
        let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
        match alteration {
            "same-prompt" => {
                let mut selection = task_records::selection_evidence(&store, "task")
                    .unwrap()
                    .unwrap();
                selection.effective_config.resume.args =
                    selection.effective_config.initial.args.clone();
                db.execute(
                    "UPDATE evidence SET payload=?1 WHERE task_id='task' AND kind='selection'",
                    [serde_json::to_string(&selection).unwrap()],
                )
                .unwrap();
            }
            "session" => {
                let mut prior = initial;
                prior.session_id = "other-task".into();
                db.execute(
                    "UPDATE intents SET detail=?1 WHERE task_id='task' AND kind='launch'",
                    [serde_json::to_string(&prior).unwrap()],
                )
                .unwrap();
            }
            "inode" => {
                let mut identity = worktree_records::worktree_record(&store, "task")
                    .unwrap()
                    .unwrap()
                    .identity
                    .unwrap();
                identity.inode += 1;
                db.execute("UPDATE evidence SET payload=?1 WHERE task_id='task' AND kind='worktree_created'", [serde_json::to_string(&identity).unwrap()]).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            prepare_resume(&mut store, "task", "fresh").is_err(),
            "{alteration}"
        );
        assert_eq!(
            scheduling::reservation_count(&store).unwrap(),
            0,
            "{alteration}"
        );
        assert!(launches::launch_intent(&store, "fresh").unwrap().is_none());
    }
}

pub(crate) fn initial_rejects_conflicting_session_and_cwd_arguments_before_reservation() {
    let alterations: &[&[&str]] = &[
        &["--session", "other-task"],
        &["--cwd", "/tmp/other"],
        &["--session=other-task"],
        &["--cwd=/tmp/other"],
    ];
    for (index, alteration) in alterations.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let (mut config, candidate) = configured(dir.path());
        config
            .initial
            .args
            .extend(alteration.iter().map(|arg| (*arg).into()));
        let mut store = StateStore::open(&config.state_root, 1).unwrap();
        claimed(&mut store, &config, &candidate, dir.path());

        let attempt = format!("attempt-invalid-{index}");
        assert!(prepare_initial(&mut store, "task", &attempt).is_err());
        assert_eq!(
            scheduling::reservation_count(&store).unwrap(),
            0,
            "{alteration:?}"
        );
        assert!(launches::launch_intent(&store, &attempt).unwrap().is_none());
    }
}
