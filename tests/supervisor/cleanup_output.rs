use super::cleanup_fixture::*;
use super::*;
use luthor::config::CleanupPolicy;
use serde_json::{Value, json};
use std::os::unix::fs::symlink;

#[test]
fn cleanup_whole_worktree_removes_ignored_untracked_fixture_repositories_under_tmp() {
    let (_dir, mut config, mut store, mut prs) = fixture();
    config.cleanup = CleanupPolicy::Worktree;
    close(&mut prs, true);
    let nested = root(&config).join("tmp/f/fixture/checkout");
    fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-b", "fixture"]);
    fs::write(nested.join("fixture-output"), "disposable").unwrap();
    assert_eq!(clean(&mut store, &config, &mut prs).cleaned, ["task"]);
    assert!(!root(&config).exists());
    assert!(config.mappings[0].checkout.join("README").exists());
}

#[test]
fn cleanup_tmp_fixture_exception_does_not_allow_symlinks_or_other_ignored_repositories() {
    let (_dir, mut config, mut store, mut prs) = fixture();
    config.cleanup = CleanupPolicy::Worktree;
    close(&mut prs, true);
    let nested = root(&config).join("tmp/f/checkout");
    fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-b", "fixture"]);
    symlink(&config.mappings[0].checkout, nested.join("escape")).unwrap();
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup tree contains symlink, mount or special file",
    );
    fs::remove_file(nested.join("escape")).unwrap();
    fs::create_dir(root(&config).join("target/.git")).unwrap();
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup tree contains another repository",
    );
    assert!(nested.join(".git").exists());
}

#[test]
fn cleanup_tracked_submodule_under_ignored_tmp_is_refused_even_when_uninitialized() {
    let (_dir, mut config, mut store, mut prs) = fixture();
    config.cleanup = CleanupPolicy::Worktree;
    close(&mut prs, true);
    let sha = prs.detail["head"]["sha"].as_str().unwrap().to_owned();
    git(
        &root(&config),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{sha},tmp/submodule"),
        ],
    );
    git(&root(&config), &["commit", "-m", "tracked submodule"]);
    fs::create_dir(root(&config).join("tmp/submodule")).unwrap();
    let output = Command::new("git")
        .arg("-C")
        .arg(root(&config))
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());
    prs.detail["head"]["sha"] = json!(String::from_utf8(output.stdout).unwrap().trim());
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "cleanup worktree contains tracked submodule",
    );
    assert!(root(&config).join("target/debug/artifact").exists());
}

#[test]
fn cleanup_failure_is_superseded_in_status_and_show_after_success_without_erasing_history() {
    let (_dir, config, mut store, mut prs) = fixture();
    close(&mut prs, true);
    prs.fail = true;
    assert_skipped(
        &mut store,
        &config,
        &mut prs,
        "pull request lookup failed (Transport, offline)",
    );
    prs.fail = false;
    assert_eq!(clean(&mut store, &config, &mut prs).cleaned, ["task"]);
    let shown: Value = serde_json::from_str(
        &luthor::cli::execute(&config.state_root, &["show".into(), "task".into()]).unwrap(),
    )
    .unwrap();
    assert!(shown["cleanup_failure"].is_null());
    assert!(
        shown["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["kind"] == "cleanup_failed")
    );
    let status: Value = serde_json::from_str(
        &luthor::cli::execute(&config.state_root, &["status".into()]).unwrap(),
    )
    .unwrap();
    assert!(status["tasks"][0]["cleanup_failure"].is_null());
}
