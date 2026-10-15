use super::*;
use luthor::state::{journal, task_records};

pub(crate) fn configured(root: &Path) -> (Config, Candidate) {
    let mapping = Mapping {
        tracker_repository: "org/tracker".into(),
        code_repository: "org/code".into(),
        checkout: root.join("checkout"),
        base_branch: "main".into(),
        push_remote: "origin".into(),
        allowed_pr_head_repository: "org/code".into(),
        allowed_pr_author: "operator".into(),
        required_checks: Default::default(),
        merge: luthor::config::MergePolicy::disabled(),
    };
    let source = Source {
        dependencies: Vec::new(),
        project_id: "project".into(),
        repositories: vec!["org/tracker".into()],
        ready_marker: Marker::Label {
            name: "ready".into(),
        },
        milestone: None,
    };
    let worker = fixture_worker(root);
    let initial = CommandTemplate {
        executable: worker,
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
        state_root: fixture_state_root(root),
        worktree_root: root.join("worktrees"),
        capacity: 1,
        retry_policy: Default::default(),
        github_polling: Default::default(),
        cleanup: luthor::config::CleanupPolicy::disabled(),
        min_free_space_bytes: 0,
        assignment_login: "operator".into(),
        sources: vec![source.clone()],
        mappings: vec![mapping.clone()],
        initial: initial.clone(),
        resume: CommandTemplate {
            executable: initial.executable.clone(),
            args: vec![
                "--session".into(),
                "{task.id}".into(),
                "--cwd".into(),
                "{worktree}".into(),
                "--prompt".into(),
                "Continue {task.issue_url} for {attempt.id}".into(),
            ],
        },
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

fn fixture_state_root(root: &Path) -> std::path::PathBuf {
    if root.is_absolute() {
        root.strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .join("state")
    } else {
        root.join("state")
    }
}

pub(crate) fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(crate) fn claimed(
    store: &mut StateStore,
    config: &Config,
    candidate: &Candidate,
    _root: &Path,
) {
    task_records::create_task(store, "task", candidate, "rev", config).unwrap();
    task_records::record_claim_intent(store, "task", &config.assignment_login, "org/tracker", 7)
        .unwrap();
    journal::record_evidence(
        store,
        "task",
        None,
        "claim_verified",
        &config.assignment_login,
    )
    .unwrap();
    task_records::set_task_phase(store, "task", "claimed").unwrap();
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
    crate::git_fixture::remote(checkout);
    ensure_worktree(store, "task", &config.worktree_root, &candidate.mapping).unwrap();
}

fn fixture_worker(root: &Path) -> std::path::PathBuf {
    let worker = root.join("worker-that-must-not-run");
    fs::write(&worker, "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    worker
}
