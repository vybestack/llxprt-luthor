use super::cleanup_fixture::*;
use super::*;
use luthor::config::CleanupPolicy;
use luthor::state::journal;
use std::os::unix::fs::symlink;

#[test]
fn cleanup_symlink_artifacts_and_tracked_paths_cannot_escape_worktree() {
    let (_dir, mut config, mut store, mut prs) = fixture();
    close(&mut prs, true);
    let worktree = root(&config);
    symlink(&config.mappings[0].checkout, worktree.join("target/escape")).unwrap();
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup tree contains symlink, mount or special file",
    );
    assert!(worktree.join("target/debug/artifact").exists());
    assert!(config.mappings[0].checkout.join("README").exists());
    fs::remove_file(worktree.join("target/escape")).unwrap();
    config.cleanup = CleanupPolicy::Artifacts {
        paths: vec!["README".into()],
    };
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup artifact must be ignored and untracked",
    );
    assert!(worktree.join("README").exists());
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"cleanup_complete".into())
    );
}

#[test]
fn cleanup_whole_worktree_refuses_unsaved_files_and_changed_tip() {
    let (_dir, mut config, mut store, mut prs) = fixture();
    config.cleanup = CleanupPolicy::Worktree;
    close(&mut prs, true);
    fs::write(root(&config).join("unsaved"), "operator work").unwrap();
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup worktree contains unsaved files",
    );
    fs::remove_file(root(&config).join("unsaved")).unwrap();
    fs::write(root(&config).join("README"), "operator commit").unwrap();
    git(&root(&config), &["add", "README"]);
    git(&root(&config), &["commit", "-m", "unpublished change"]);
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup worktree tip differs from verified PR tip",
    );
    assert!(root(&config).join("target").exists());
}

#[test]
fn cleanup_unfinished_intent_is_not_automatically_repeated() {
    let (_dir, config, mut store, mut prs) = fixture();
    close(&mut prs, true);
    journal::record_intent(
        &mut store,
        "cleanup-task",
        "task",
        None,
        "cleanup",
        "interrupted fixture deletion",
    )
    .unwrap();
    let reads = prs.reads;
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "unfinished cleanup intent requires operator inspection",
    );
    assert_eq!(prs.reads, reads);
    assert!(root(&config).join("target/debug/artifact").exists());
    assert!(
        journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"cleanup_failed".into())
    );
}

#[test]
fn cleanup_live_tracked_descendant_refuses_before_github_or_deletion() {
    let (_dir, config, mut store, mut prs) = fixture();
    close(&mut prs, true);
    let reads = prs.reads;
    let (boot_identity, start_identity) = test_process_identity(std::process::id());
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "tracked_descendant",
        &serde_json::json!({
            "pid":std::process::id(),"boot_identity":boot_identity,"start_identity":start_identity
        })
        .to_string(),
    )
    .unwrap();
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup process quiescence unproven",
    );
    assert_eq!(prs.reads, reads);
    assert!(root(&config).join("target").exists());
}

#[test]
fn cleanup_replaced_worktree_and_reserved_task_refuse() {
    let (_dir, config, mut store, mut prs) = fixture();
    close(&mut prs, true);
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE reservations SET status='reserved' WHERE task_id='task'",
            [],
        )
        .unwrap();
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup terminal task proof invalid",
    );
    connection
        .execute(
            "UPDATE reservations SET status='released' WHERE task_id='task'",
            [],
        )
        .unwrap();
    fs::rename(root(&config), config.worktree_root.join("moved")).unwrap();
    fs::create_dir(root(&config)).unwrap();
    fs::write(root(&config).join("foreign"), "foreign data").unwrap();
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "worktree repository, path or branch changed",
    );
    assert!(root(&config).join("foreign").exists());
    assert!(
        config
            .worktree_root
            .join("moved/target/debug/artifact")
            .exists()
    );
}

#[test]
fn cleanup_nested_repository_and_symlink_parent_fail_before_any_removal() {
    let (_dir, config, mut store, mut prs) = fixture();
    close(&mut prs, true);
    let worktree = root(&config);
    fs::create_dir(worktree.join("target/.git")).unwrap();
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup tree contains another repository",
    );
    fs::remove_dir(worktree.join("target/.git")).unwrap();
    fs::rename(worktree.join("tmp"), worktree.join("tmp-saved")).unwrap();
    symlink(&config.mappings[0].checkout, worktree.join("tmp")).unwrap();
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup path contains symlink",
    );
    assert!(worktree.join("target/debug/artifact").exists());
    assert!(config.mappings[0].checkout.join("README").exists());
}

#[test]
fn cleanup_missing_artifacts_complete_without_inventing_removal_events() {
    let (_dir, config, mut store, mut prs) = fixture();
    close(&mut prs, false);
    fs::remove_dir_all(root(&config).join("target")).unwrap();
    fs::remove_dir_all(root(&config).join("tmp/build")).unwrap();
    assert_eq!(clean(&mut store, &config, &mut prs).cleaned, ["task"]);
    let kinds = journal::evidence_kinds(&store, "task").unwrap();
    assert!(kinds.contains(&"cleanup_complete".into()));
    assert!(!kinds.contains(&"cleanup_removed".into()));
}
