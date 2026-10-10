use luthor::state::{journal, task_records, worktree_records};
use luthor::{
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    eligibility::Candidate,
    state::StateStore,
    worktree::{
        WorktreeError, WorktreeResult, ensure_worktree, ensure_worktree_with_hooks, verify_snapshot,
    },
};
use std::{fs, path::Path, process::Command};

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct Fixture {
    dir: tempfile::TempDir,
    config: Config,
    store: StateStore,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        git(&checkout, &["init", "-b", "main"]);
        git(&checkout, &["config", "user.name", "Fixture"]);
        git(&checkout, &["config", "user.email", "fixture@example.org"]);
        git(
            &checkout,
            &["remote", "add", "origin", "git@github.com:org/code.git"],
        );
        fs::write(checkout.join("README"), "test").unwrap();
        git(&checkout, &["add", "README"]);
        git(&checkout, &["commit", "-m", "initial"]);
        let config = Config {
            state_root: dir.path().join("state"),
            worktree_root: dir.path().join("private"),
            capacity: 1,
            assignment_login: "bot".into(),
            sources: vec![Source {
                project_id: "project".into(),
                repositories: vec!["org/tracker".into()],
                ready_marker: Marker::Label {
                    name: "ready".into(),
                },
                milestone: None,
            }],
            mappings: vec![Mapping {
                tracker_repository: "org/tracker".into(),
                code_repository: "org/code".into(),
                checkout,
                base_branch: "main".into(),
                push_remote: "origin".into(),
                allowed_pr_head_repository: "org/code".into(),
                allowed_pr_author: "bot".into(),
            }],
            initial: CommandTemplate {
                executable: "/bin/worker".into(),
                args: vec!["--cwd".into(), "{worktree}".into()],
            },
            resume: CommandTemplate {
                executable: "/bin/worker".into(),
                args: vec!["--cwd".into(), "{worktree}".into()],
            },
        };
        let store = StateStore::open(&config.state_root, 1).unwrap();
        Self { dir, config, store }
    }
    fn task(&mut self, id: &str, number: u64, claimed: bool) {
        let candidate = Candidate {
            project_id: "project".into(),
            item_id: format!("item-{number}"),
            issue_node_id: format!("node-{number}"),
            tracker_repo_id: "repo-id".into(),
            repository: "org/tracker".into(),
            issue_number: number,
            issue_url: format!("https://github.com/org/tracker/issues/{number}"),
            milestone_id: None,
            milestone_title: None,
            observed_at_unix_secs: 1,
            observed_state: "open".into(),
            observed_assignees: vec![],
            observed_labels: vec!["ready".into()],
            observed_project_fields: vec![],
            marker: Marker::Label {
                name: "ready".into(),
            },
            mapping: self.config.mappings[0].clone(),
            source: self.config.sources[0].clone(),
        };
        task_records::create_task(&mut self.store, id, &candidate, "rev", &self.config).unwrap();
        if claimed {
            task_records::record_claim_intent(&mut self.store, id, "bot", "org/tracker", number)
                .unwrap();
            journal::record_evidence(&mut self.store, id, None, "claim_verified", "bot").unwrap();
            task_records::set_task_phase(&mut self.store, id, "claimed").unwrap();
        }
    }
    fn create(&mut self, id: &str) -> Result<WorktreeResult, WorktreeError> {
        ensure_worktree(
            &mut self.store,
            id,
            &self.config.worktree_root,
            &self.config.mappings[0],
        )
    }
}

#[test]
fn creates_and_reopens_verified_worktree() {
    let mut f = Fixture::new();
    f.task("task-1", 1, true);
    let WorktreeResult::Created(identity) = f.create("task-1").unwrap() else {
        panic!("not created")
    };
    assert_eq!(identity.branch, "luthor/task-1");
    assert_eq!(
        identity.path,
        fs::canonicalize(f.config.worktree_root.join("task-1")).unwrap()
    );
    assert_eq!(
        git(&identity.path, &["branch", "--show-current"]),
        identity.branch
    );
    assert_eq!(identity.head, git(&identity.path, &["rev-parse", "HEAD"]));
    assert_eq!(
        worktree_records::worktree_record(&f.store, "task-1")
            .unwrap()
            .unwrap()
            .intent
            .branch,
        identity.branch
    );
    drop(f.store);
    let mut store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        ensure_worktree(
            &mut store,
            "task-1",
            &f.config.worktree_root,
            &f.config.mappings[0]
        )
        .unwrap(),
        WorktreeResult::Existing(identity)
    );
}

#[test]
fn preclaim_and_invalid_ids_refuse_without_side_effects() {
    let mut f = Fixture::new();
    f.task("task-1", 1, false);
    assert!(matches!(f.create("task-1"), Err(WorktreeError::NotClaimed)));
    assert!(matches!(
        f.create("../outside"),
        Err(WorktreeError::InvalidTaskId)
    ));
    assert!(
        worktree_records::worktree_record(&f.store, "task-1")
            .unwrap()
            .is_none()
    );
    assert!(!f.config.worktree_root.exists());
}

#[test]
fn duplicate_task_and_foreign_path_or_branch_are_not_adopted() {
    let mut f = Fixture::new();
    f.task("task-1", 1, true);
    fs::create_dir(&f.config.worktree_root).unwrap();
    fs::create_dir(f.config.worktree_root.join("task-1")).unwrap();
    assert!(matches!(
        f.create("task-1"),
        Err(WorktreeError::Conflict(_))
    ));
    assert!(
        worktree_records::worktree_record(&f.store, "task-1")
            .unwrap()
            .is_none()
    );
    fs::remove_dir(f.config.worktree_root.join("task-1")).unwrap();
    git(&f.config.mappings[0].checkout, &["branch", "luthor/task-1"]);
    assert!(matches!(
        f.create("task-1"),
        Err(WorktreeError::Conflict(_))
    ));
    assert!(
        worktree_records::worktree_record(&f.store, "task-1")
            .unwrap()
            .is_none()
    );
    f.task("task-2", 2, true);
    f.create("task-2").unwrap();
    assert!(matches!(
        f.create("task-2"),
        Ok(WorktreeResult::Existing(_))
    ));
    // A second invocation cannot issue another worktree-add.
    assert_eq!(
        git(
            &f.config.mappings[0].checkout,
            &["worktree", "list", "--porcelain"]
        )
        .matches("branch refs/heads/luthor/task-2")
        .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn symlink_root_and_replaced_worktree_hold() {
    use std::os::unix::fs::symlink;
    let mut f = Fixture::new();
    f.task("task-1", 1, true);
    fs::create_dir(f.dir.path().join("other")).unwrap();
    symlink(f.dir.path().join("other"), &f.config.worktree_root).unwrap();
    assert!(matches!(
        f.create("task-1"),
        Err(WorktreeError::Conflict(_))
    ));
    assert!(
        worktree_records::worktree_record(&f.store, "task-1")
            .unwrap()
            .is_none()
    );
    fs::remove_file(&f.config.worktree_root).unwrap();
    let WorktreeResult::Created(identity) = f.create("task-1").unwrap() else {
        panic!()
    };
    fs::rename(&identity.path, f.dir.path().join("moved")).unwrap();
    fs::create_dir(&identity.path).unwrap();
    assert!(matches!(
        f.create("task-1"),
        Err(WorktreeError::Conflict(_))
    ));
}

#[cfg(unix)]
#[test]
fn user_symlink_ancestor_is_rejected_before_creating_a_root() {
    use std::os::unix::fs::symlink;
    let mut f = Fixture::new();
    let real = f.dir.path().join("real");
    fs::create_dir(&real).unwrap();
    let alias = f.dir.path().join("alias");
    symlink(&real, &alias).unwrap();
    f.config.worktree_root = alias.join("private");
    f.task("task-1", 1, true);
    assert!(matches!(
        f.create("task-1"),
        Err(WorktreeError::Conflict(_))
    ));
    assert!(!real.join("private").exists());
    assert!(
        worktree_records::worktree_record(&f.store, "task-1")
            .unwrap()
            .is_none()
    );
}

#[test]
fn restart_accepts_descendant_commit_but_partial_intent_holds_without_retry() {
    let mut f = Fixture::new();
    f.task("task-1", 1, true);
    let WorktreeResult::Created(identity) = f.create("task-1").unwrap() else {
        panic!()
    };
    fs::write(identity.path.join("change"), "change").unwrap();
    git(&identity.path, &["add", "change"]);
    git(&identity.path, &["commit", "-m", "changed"]);
    verify_snapshot(&identity).unwrap();
    drop(f.store);
    let mut store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(
        ensure_worktree(
            &mut store,
            "task-1",
            &f.config.worktree_root,
            &f.config.mappings[0]
        )
        .unwrap(),
        WorktreeResult::Existing(identity)
    );
    f.store = store;
    f.task("task-2", 2, true);
    let root = fs::canonicalize(&f.config.worktree_root).unwrap();
    worktree_records::begin_worktree(
        &mut f.store,
        "task-2",
        &luthor::state::WorktreeIntent {
            path: root.join("task-2"),
            branch: "luthor/task-2".into(),
            base: "main".into(),
            repository: "org/code".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        f.create("task-2"),
        Err(WorktreeError::Conflict(_))
    ));
    assert!(!root.join("task-2").exists());
}

#[test]
fn rewound_or_unrelated_head_is_not_adopted() {
    let mut f = Fixture::new();
    let checkout = &f.config.mappings[0].checkout;
    fs::write(checkout.join("second"), "second").unwrap();
    git(checkout, &["add", "second"]);
    git(checkout, &["commit", "-m", "second"]);
    f.task("task-1", 1, true);
    let WorktreeResult::Created(identity) = f.create("task-1").unwrap() else {
        panic!()
    };
    let path = &identity.path;
    git(path, &["reset", "--hard", &format!("{}^", identity.head)]);
    assert!(verify_snapshot(&identity).is_err());
    assert!(matches!(
        f.create("task-1"),
        Err(WorktreeError::Conflict(_))
    ));

    git(path, &["switch", "--orphan", "unrelated"]);
    fs::write(path.join("other"), "other").unwrap();
    git(path, &["add", "other"]);
    git(path, &["commit", "-m", "unrelated"]);
    let unrelated = git(path, &["rev-parse", "HEAD"]);
    git(path, &["switch", "luthor/task-1"]);
    git(path, &["reset", "--hard", &unrelated]);
    assert!(verify_snapshot(&identity).is_err());
    assert!(matches!(
        f.create("task-1"),
        Err(WorktreeError::Conflict(_))
    ));
}

#[test]
fn interruption_after_root_creation_leaves_durable_intent_and_never_adopts_a_path() {
    let mut f = Fixture::new();
    f.task("task-1", 1, true);
    let root = f.config.worktree_root.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ensure_worktree_with_hooks(
            &mut f.store,
            "task-1",
            &root,
            &f.config.mappings[0],
            |_| {},
            || {
                assert!(root.is_dir());
                panic!("interrupted after mkdir");
            },
        )
    }));
    assert!(result.is_err());
    let intent = worktree_records::worktree_record(&f.store, "task-1")
        .unwrap()
        .unwrap()
        .intent;
    assert_eq!(
        intent.path,
        fs::canonicalize(f.dir.path())
            .unwrap()
            .join("private/task-1")
    );
    assert!(!intent.path.exists());
    drop(f.store);
    let mut store = StateStore::open(&f.config.state_root, 1).unwrap();
    fs::create_dir(&intent.path).unwrap();
    assert!(matches!(
        ensure_worktree(&mut store, "task-1", &root, &f.config.mappings[0]),
        Err(WorktreeError::Conflict(
            "unfinished worktree intent requires inspection"
        ))
    ));
    assert_eq!(
        task_records::task_phase(&store, "task-1")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert_eq!(
        worktree_records::worktree_record(&store, "task-1")
            .unwrap()
            .map(|r| r.intent),
        Some(intent)
    );
}

#[test]
fn rejected_begin_worktree_leaves_root_untouched() {
    let mut f = Fixture::new();
    f.task("task-1", 1, true);
    let root = f.config.worktree_root.clone();
    assert!(matches!(
        ensure_worktree_with_hooks(
            &mut f.store,
            "task-1",
            &root,
            &f.config.mappings[0],
            |store| task_records::set_task_phase(store, "task-1", "held").unwrap(),
            || panic!("root must not be created"),
        ),
        Err(WorktreeError::State(
            luthor::state::StateError::InvalidSelection
        ))
    ));
    assert!(!root.exists());
    assert!(
        worktree_records::worktree_record(&f.store, "task-1")
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn changed_root_after_intent_does_not_create_a_worktree() {
    use std::os::unix::fs::symlink;
    let mut f = Fixture::new();
    f.task("task-1", 1, true);
    let root = f.config.worktree_root.clone();
    let moved = f.dir.path().join("moved");
    assert!(matches!(
        ensure_worktree_with_hooks(
            &mut f.store,
            "task-1",
            &root,
            &f.config.mappings[0],
            |_| {},
            || {
                fs::rename(&root, &moved).unwrap();
                symlink(&moved, &root).unwrap();
            },
        ),
        Err(WorktreeError::Conflict("worktree root contains a symlink"))
    ));
    assert!(
        worktree_records::worktree_record(&f.store, "task-1")
            .unwrap()
            .is_some()
    );
    assert!(!moved.join("task-1").exists());
    assert_eq!(
        task_records::task_phase(&f.store, "task-1")
            .unwrap()
            .as_deref(),
        Some("held")
    );
}

#[cfg(unix)]
#[test]
fn existing_worktree_does_not_change_root_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = Fixture::new();
    f.task("task-1", 1, true);
    let WorktreeResult::Created(identity) = f.create("task-1").unwrap() else {
        panic!()
    };
    fs::set_permissions(&f.config.worktree_root, fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(
        f.create("task-1").unwrap(),
        WorktreeResult::Existing(identity)
    );
    assert_eq!(
        fs::metadata(&f.config.worktree_root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
}
