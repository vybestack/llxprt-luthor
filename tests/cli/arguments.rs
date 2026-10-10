use std::{fs, process::Command};

#[cfg(unix)]
pub(crate) fn recover_requires_execute_before_config_store_or_github_access() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    let calls = dir.path().join("gh-calls");
    fs::write(
        &gh,
        format!("#!/bin/sh\necho called >> '{}'\n", calls.display()),
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args([
            "recover",
            "task",
            "--attempt",
            "attempt",
            "--config",
            "/must/not/open",
            "--actor",
            "operator",
            "--reason",
            "audited reason",
        ])
        .env("PATH", dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--execute"));
    assert!(!calls.exists());
}

pub(crate) fn recover_accepts_documented_syntax_then_fails_before_state_or_github_access() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent_config = dir.path().join("missing-config.yaml");
    let output = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(["recover", "task", "--attempt", "attempt", "--config"])
        .arg(&nonexistent_config)
        .args([
            "--actor",
            "operator",
            "--reason",
            "lost receipt",
            "--execute",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("configuration unavailable"));
}

pub(crate) fn recover_rejects_extra_argument_before_config_access() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent_config = dir.path().join("missing-config.yaml");
    let output = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(["recover", "task", "--attempt", "attempt", "--config"])
        .arg(&nonexistent_config)
        .args([
            "--actor",
            "operator",
            "--reason",
            "lost receipt",
            "ignored",
            "--execute",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected TASK"));
}

pub(crate) fn pause_and_reconcile_reject_bad_arguments_and_unknown_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let state = dir
        .path()
        .strip_prefix(std::env::current_dir().unwrap())
        .unwrap()
        .join("state");
    fs::write(&config, format!(r#"{{"state_root":"{}","worktree_root":"{}","capacity":1,"assignment_login":"operator","sources":[{{"project_id":"project","repositories":["org/tracker"],"ready_marker":{{"kind":"label","name":"ready"}},"milestone":null}}],"mappings":[{{"tracker_repository":"org/tracker","code_repository":"org/code","checkout":"/code","base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/code","allowed_pr_author":"operator"}}],"initial":{{"executable":"/worker","args":[]}},"resume":{{"executable":"/worker","args":[]}}}}"#, state.display(), dir.path().display())).unwrap();
    let binary = env!("CARGO_BIN_EXE_luthor");
    let bad = Command::new(binary)
        .args(["pause", "--config", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!bad.status.success());

    for command in ["pause", "reconcile"] {
        let output = Command::new(binary)
            .args([
                command,
                "missing-task",
                "--config",
                config.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("task not found"));
    }
}

#[test]
fn invalid_dispatch_arguments_precede_configuration_and_writable_io() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("missing-config.json");
    for tail in [
        vec!["--issue", "0", "--config-revision", "rev", "--execute"],
        vec![
            "--issue",
            "1",
            "--config-revision",
            "rev",
            "--execute",
            "--execute",
        ],
        vec!["--issue", "1", "--config-revision", "rev", "--unknown"],
        vec![
            "--issue",
            "1",
            "--config-revision",
            "rev",
            "--config",
            "duplicate",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_luthor"))
            .args(["dispatch", "--config"])
            .arg(&config)
            .args(["--repository", "org/tracker"])
            .args(tail)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("No such file"), "{stderr}");
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }
}
