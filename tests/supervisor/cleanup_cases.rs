use super::cleanup_fixture::*;
use super::*;
use luthor::state::{journal, task_records};
use luthor::{cleanup::clean_terminal_tasks, config::CleanupPolicy};
use serde_json::json;

#[test]
fn cleanup_merged_and_closed_pr_removes_only_listed_artifacts_once_across_restart() {
    for merged in [true, false] {
        let (_dir, config, mut store, mut prs) = fixture();
        let worktree = root(&config);
        close(&mut prs, merged);
        assert_eq!(clean(&mut store, &config, &mut prs).cleaned, ["task"]);
        assert!(!worktree.join("target").exists());
        assert!(!worktree.join("tmp/build").exists());
        assert!(worktree.join("README").exists());
        assert!(config.mappings[0].checkout.join("README").exists());
        assert!(
            config
                .state_root
                .join("attempts/attempt-real.receipt.json")
                .exists()
        );
        let shown: serde_json::Value = serde_json::from_str(
            &luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap(),
        )
        .unwrap();
        let removed: Vec<_> = shown["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["kind"] == "cleanup_removed")
            .collect();
        assert_eq!(removed.len(), 2);
        assert!(
            removed
                .iter()
                .any(|row| row["detail"]["path"] == json!(worktree.join("target")))
        );
        drop(store);
        let mut reopened = StateStore::open(&config.state_root, config.capacity).unwrap();
        let reads = prs.reads;
        assert!(clean(&mut reopened, &config, &mut prs).cleaned.is_empty());
        assert_eq!(prs.reads, reads);
    }
}

#[test]
fn cleanup_worktree_removes_git_registration_but_preserves_branch_state_and_checkout() {
    let (_dir, mut config, mut store, mut prs) = fixture();
    config.cleanup = CleanupPolicy::Worktree;
    close(&mut prs, true);
    assert_eq!(clean(&mut store, &config, &mut prs).cleaned, ["task"]);
    assert!(!root(&config).exists());
    let output = Command::new("git")
        .arg("-C")
        .arg(&config.mappings[0].checkout)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("luthor/task")
    );
    git(
        &config.mappings[0].checkout,
        &["show-ref", "--verify", "refs/heads/luthor/task"],
    );
    assert!(config.state_root.join("state.sqlite3").exists());
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("pr_complete")
    );
}

#[test]
fn cleanup_open_or_reopened_or_replacement_pr_is_retained() {
    let (_dir, config, mut store, mut prs) = fixture();
    assert_eq!(clean(&mut store, &config, &mut prs).retained, ["task"]);
    close(&mut prs, false);
    prs.replacement = true;
    assert_eq!(clean(&mut store, &config, &mut prs).retained, ["task"]);
    assert!(root(&config).join("target/debug/artifact").exists());
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"cleanup_complete".into())
    );
}

#[test]
fn cleanup_disabled_held_running_and_busy_owner_never_delete_or_read_pr() {
    let (_dir, mut config, mut store, mut prs) = fixture();
    close(&mut prs, true);
    let reads = prs.reads;
    let policy = config.cleanup.clone();
    config.cleanup = CleanupPolicy::Disabled;
    assert!(clean(&mut store, &config, &mut prs).cleaned.is_empty());
    config.cleanup = policy;
    for phase in ["held", "running"] {
        task_records::set_task_phase(&mut store, "task", phase).unwrap();
        assert!(clean(&mut store, &config, &mut prs).cleaned.is_empty());
    }
    task_records::set_task_phase(&mut store, "task", "pr_complete").unwrap();
    let owner = luthor::WorktreeOwner::acquire_existing(store.root(), "task").unwrap();
    assert_eq!(clean(&mut store, &config, &mut prs).retained, ["task"]);
    assert_eq!(prs.reads, reads);
    assert!(root(&config).join("target").exists());
    drop(owner);
}

#[test]
fn cleanup_api_failure_and_pr_identity_drift_fail_closed() {
    let (_dir, config, mut store, mut prs) = fixture();
    close(&mut prs, true);
    prs.fail = true;
    assert!(clean_terminal_tasks(&mut store, &config, &mut prs).is_err());
    prs.fail = false;
    prs.detail["id"] = json!(9999);
    assert!(clean_terminal_tasks(&mut store, &config, &mut prs).is_err());
    assert!(root(&config).join("target/debug/artifact").exists());
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"cleanup_complete".into())
    );
}
