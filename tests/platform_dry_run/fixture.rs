use super::*;
use luthor::state::{journal, task_records};
pub(super) struct NativeFixture {
    pub(super) dir: TempDir,
    pub(super) binary: PathBuf,
    pub(super) config_root: PathBuf,
}
impl NativeFixture {
    pub(super) fn new() -> Self {
        let binary = std::env::var_os("LUTHOR_RS_BINARY").expect("LUTHOR_RS_BINARY is required");
        let binary = PathBuf::from(binary);
        assert!(
            binary.is_absolute() && binary.is_file(),
            "LUTHOR_RS_BINARY must be an absolute file"
        );
        let dir = tempdir().unwrap();
        let config_root = dir.path().join("config-root");
        fs::create_dir_all(&config_root).unwrap();
        Self {
            dir,
            binary,
            config_root,
        }
    }
}
fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
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

fn mapping(dir: &Path) -> Mapping {
    let checkout = dir.join("checkout");
    fs::create_dir(&checkout).unwrap();
    git(&checkout, &["init", "-b", "main"]);
    git(&checkout, &["config", "user.name", "Fixture"]);
    git(&checkout, &["config", "user.email", "fixture@example.org"]);
    git(
        &checkout,
        &["remote", "add", "origin", "git@github.com:org/code.git"],
    );
    fs::write(checkout.join("README"), "fixture").unwrap();
    git(&checkout, &["add", "README"]);
    git(&checkout, &["commit", "-m", "fixture"]);
    Mapping {
        tracker_repository: "org/tracker".into(),
        code_repository: "org/code".into(),
        checkout,
        base_branch: "main".into(),
        push_remote: "origin".into(),
        allowed_pr_head_repository: "org/code".into(),
        allowed_pr_author: "operator".into(),
    }
}
fn source() -> Source {
    Source {
        project_id: "project".into(),
        repositories: vec!["org/tracker".into()],
        ready_marker: Marker::Label {
            name: "ready".into(),
        },
        milestone: None,
    }
}
fn candidate(source: Source, mapping: Mapping) -> Candidate {
    Candidate {
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
        source,
        mapping,
    }
}
pub(super) fn fixture_config(
    dir: &Path,
    binary: &Path,
    profile: &Path,
    stopping: bool,
) -> (Config, Candidate) {
    let mapping = mapping(dir);
    let source = source();
    let initial = native_command(dir, binary, profile, "Respond with a short greeting");
    let resume = if stopping {
        native_command(
            dir,
            binary,
            profile,
            "Distinct second turn after stop for {attempt.id}",
        )
    } else {
        CommandTemplate {
            executable: "/usr/bin/true".into(),
            args: vec![],
        }
    };
    let config = Config {
        state_root: dir.join("state"),
        worktree_root: dir.join("worktrees"),
        capacity: 1,
        assignment_login: "operator".into(),
        sources: vec![source.clone()],
        mappings: vec![mapping.clone()],
        initial,
        resume,
    };
    (config, candidate(source, mapping))
}
fn native_command(dir: &Path, binary: &Path, profile: &Path, prompt: &str) -> CommandTemplate {
    CommandTemplate {
        executable: "/usr/bin/env".into(),
        args: vec![
            format!("LLXPRT_CONFIG_HOME={}", dir.join("config-root").display()),
            binary.display().to_string(),
            "--profile-load".into(),
            profile.display().to_string(),
            "--session".into(),
            "{task.id}".into(),
            "--cwd".into(),
            "{worktree}".into(),
            "-p".into(),
            prompt.into(),
        ],
    }
}
pub(super) fn claimed_store(config: &Config, candidate: &Candidate) -> StateStore {
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    task_records::create_task(&mut store, "task", candidate, "rev", config).unwrap();
    task_records::record_claim_intent(&mut store, "task", "operator", "org/tracker", 7).unwrap();
    journal::record_evidence(&mut store, "task", None, "claim_verified", "operator").unwrap();
    task_records::set_task_phase(&mut store, "task", "claimed").unwrap();
    ensure_worktree(
        &mut store,
        "task",
        &config.worktree_root,
        &candidate.mapping,
    )
    .unwrap();
    store
}
