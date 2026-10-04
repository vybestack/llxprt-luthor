use super::*;

pub(super) fn configured_mapping(root: &Path, same_repository: bool) -> (Config, Candidate) {
    let (mut config, mut candidate) = configured(root);
    if same_repository {
        candidate.repository = candidate.mapping.code_repository.clone();
        candidate.issue_url = format!(
            "https://github.com/{}/issues/{}",
            candidate.repository, candidate.issue_number
        );
        candidate.mapping.tracker_repository = candidate.repository.clone();
        candidate.source.repositories = vec![candidate.repository.clone()];
        config.mappings[0] = candidate.mapping.clone();
        config.sources[0] = candidate.source.clone();
    }
    (config, candidate)
}

pub(super) fn assert_references(args: &[String], same_repository: bool) {
    let prompt = args
        .windows(2)
        .find(|pair| matches!(pair[0].as_str(), "-p" | "--prompt"))
        .unwrap()[1]
        .as_str();
    let repository = if same_repository {
        "org/code"
    } else {
        "org/tracker"
    };
    let tracker = format!("Tracker-Issue: https://github.com/{repository}/issues/7");
    let fixes = if same_repository {
        "Fixes #7"
    } else {
        "Fixes org/tracker#7"
    };
    assert!(prompt.contains("separate complete lines"), "{prompt}");
    let lines: Vec<_> = prompt.lines().collect();
    assert!(
        lines
            .windows(2)
            .any(|pair| pair == [tracker.as_str(), fixes]),
        "{prompt}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("Fixes "))
            .count(),
        1
    );
    if !same_repository {
        assert!(!prompt.contains("Fixes #7"), "{prompt}");
        assert!(!prompt.contains("Fixes org/code#7"), "{prompt}");
    }
}

#[test]
fn initial_prompts_require_tracker_and_closing_lines_for_both_mappings() {
    for same_repository in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (config, candidate) = configured_mapping(dir.path(), same_repository);
        let mut store = StateStore::open(&config.state_root, 1).unwrap();
        claimed_for_mapping(&mut store, &config, &candidate);
        let plan = prepare_initial(&mut store, "task", "attempt-1").unwrap();
        assert_references(&plan.args, same_repository);
    }
}

#[cfg(unix)]
#[test]
fn stopped_resume_prompts_require_tracker_and_closing_lines_for_both_mappings() {
    for same_repository in [false, true] {
        let (_dir, _config, mut store, _initial) = paused_fixture_for_mapping(
            "Continue {task.issue_url} for {attempt.id}. Fixes #999",
            same_repository,
        );
        let plan = prepare_resume(&mut store, "task", "attempt-next").unwrap();
        assert_references(&plan.args, same_repository);
    }
}

pub(super) fn claimed_for_mapping(store: &mut StateStore, config: &Config, candidate: &Candidate) {
    store.create_task("task", candidate, "rev", config).unwrap();
    store
        .record_claim_intent(
            "task",
            &config.assignment_login,
            &candidate.repository,
            candidate.issue_number,
        )
        .unwrap();
    store
        .record_evidence("task", None, "claim_verified", &config.assignment_login)
        .unwrap();
    store.set_task_phase("task", "claimed").unwrap();
    let checkout = &candidate.mapping.checkout;
    fs::create_dir(checkout).unwrap();
    git(checkout, &["init", "-b", "main"]);
    git(checkout, &["config", "user.name", "Fixture"]);
    git(checkout, &["config", "user.email", "fixture@example.org"]);
    git(
        checkout,
        &["remote", "add", "origin", "git@github.com:org/code.git"],
    );
    fs::write(checkout.join("README"), "fixture").unwrap();
    git(checkout, &["add", "README"]);
    git(checkout, &["commit", "-m", "initial"]);
    ensure_worktree(store, "task", &config.worktree_root, &candidate.mapping).unwrap();
}

fn prepared_for_mapping(
    dir: &tempfile::TempDir,
    resume_prompt: &str,
    same_repository: bool,
) -> (
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
    std::path::PathBuf,
) {
    use std::os::unix::fs::PermissionsExt;
    let (mut config, candidate) = configured_mapping(dir.path(), same_repository);
    config.resume.args[5] = resume_prompt.into();
    let marker = dir.path().join("worker-started");
    let worker = dir.path().join("worker");
    fs::write(&worker, format!("#!/bin/sh\necho started > '{}'\nprintf 'worker stdout\\n'\nprintf 'worker stderr\\n' >&2\n", marker.display())).unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    config.resume.executable = worker.clone();
    config.initial.executable = worker;
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    claimed_for_mapping(&mut store, &config, &candidate);
    let plan = prepare_initial(&mut store, "task", "attempt-real").unwrap();
    (config, store, plan, marker)
}

fn paused_fixture_for_mapping(
    resume_prompt: &str,
    same_repository: bool,
) -> (
    tempfile::TempDir,
    Config,
    StateStore,
    luthor::supervisor::LaunchPlan,
) {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_for_mapping(&dir, resume_prompt, same_repository);
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let receipt = config.state_root.join("attempts/attempt-real.receipt.json");
    for _ in 0..200 {
        if receipt.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(receipt.exists());
    let initial =
        serde_json::from_str(&store.launch_intent("attempt-real").unwrap().unwrap()).unwrap();
    store.record_stop_intent("task", "attempt-real").unwrap();
    edit_receipt(&config, |receipt| {
        receipt.stop_signals = vec![libc::SIGTERM]
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last_reason = String::from("worker process group is still running");
    loop {
        match reconcile_attempt(&mut store, "task", "attempt-real").unwrap() {
            Reconciliation::Completed { .. } => break,
            Reconciliation::Held { reason } => {
                assert_eq!(store.reservation_count().unwrap(), 1);
                last_reason = reason;
            }
            Reconciliation::Running => {
                assert_eq!(store.reservation_count().unwrap(), 1);
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker reconciliation remained uncertain: {last_reason}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    store
        .record_pause_pr_lookup(
            "task",
            "attempt-real",
            &luthor::state::PausePrEvidence {
                observed_at_unix_secs: 2,
                repository: "org/code".into(),
                status: luthor::state::PausePrStatus::Absent,
            },
        )
        .unwrap();
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("paused"));
    (dir, config, store, initial)
}

#[test]
fn natural_exit_retry_prompts_require_tracker_and_closing_lines_for_both_mappings() {
    for same_repository in [false, true] {
        let (_dir, mut config, mut store) = retry_fixture_for_mapping(None, same_repository);
        let selected = store.selection_evidence("task").unwrap().unwrap();
        config.resume.args[5] = "Continue {task.issue_url} for {attempt.id}. Fixes #999".into();
        let mut projects = OtherProject(selected.candidate, 1);
        let plan = retry(
            &mut store,
            &config,
            &mut projects,
            &mut ExitPr::default(),
            &mut OtherLauncher::default(),
        )
        .unwrap();
        assert_references(&plan.args, same_repository);
    }
}
