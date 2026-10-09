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
        state_root: if root.is_absolute() {
            root.strip_prefix(std::env::current_dir().unwrap())
                .unwrap()
                .join("state")
        } else {
            root.join("state")
        },
        worktree_root: root.join("worktrees"),
        capacity: 1,
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
    ensure_worktree(store, "task", &config.worktree_root, &candidate.mapping).unwrap();
}
