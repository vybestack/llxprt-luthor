#![cfg(unix)]

use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn fixture(dir: &Path, login: &str) -> (String, String) {
    let gh = dir.join("gh");
    let calls = dir.join("calls");
    let project = json!({"data":{"node":{"items":{
        "nodes":[{"id":"ITEM","content":{"__typename":"Issue","id":"ISSUE_NODE","number":7,
            "repository":{"id":"REPO_NODE","nameWithOwner":"org/tracker"}},
            "fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],
        "pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
    .to_string();
    let issue = json!({"node_id":"ISSUE_NODE","number":7,"repository_url":"https://api.github.com/repos/org/tracker",
        "html_url":"https://github.com/org/tracker/issues/7","state":"open","assignees":[],
        "labels":[{"name":"ready"}],"milestone":null}).to_string();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$2\" in\n  graphql) printf '%s\\n' '{}' ;;\n  repos/org/tracker) printf '%s\\n' '{{\"node_id\":\"REPO_NODE\"}}' ;;\n  repos/org/tracker/issues/7?per_page=100) printf '%s\\n' '{}' ;;\n  user) printf '%s\\n' '{}' ;;\n  *) exit 99 ;;\nesac\n",
        calls.display(),
        project,
        issue,
        login
    );
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let config = dir.join("config.json");
    fs::write(
        &config,
        json!({
            "state_root":dir.join("state"),"worktree_root":dir.join("worktrees"),"capacity":1,
            "assignment_login":"acoliver",
            "sources":[{"project_id":"PROJECT","repositories":["org/tracker"],
                "ready_marker":{"kind":"label","name":"ready"},"milestone":null}],
            "mappings":[{"tracker_repository":"org/tracker","code_repository":"org/code",
                "checkout":dir.join("checkout"),"base_branch":"main","push_remote":"origin",
                "allowed_pr_head_repository":"org/code","allowed_pr_author":"acoliver"}],
            "initial":{"executable":"/bin/true","args":[]},
            "resume":{"executable":"/bin/true","args":[]}
        })
        .to_string(),
    )
    .unwrap();
    (
        config.to_string_lossy().into_owned(),
        format!("{}:/usr/bin:/bin", dir.display()),
    )
}

fn run(path: &str, search_path: &str, extras: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args([
            "daemon",
            "--config",
            path,
            "--config-revision",
            "test-revision",
            "--once",
            "--repository",
            "org/tracker",
        ])
        .args(extras)
        .env("PATH", search_path)
        .output()
        .unwrap()
}

#[test]
fn preview_missing_target_fails_without_state_or_writes() {
    let dir = tempfile::tempdir().unwrap();
    let (config, path) = fixture(dir.path(), "acoliver");
    let result = run(&config, &path, &["--issues", "8"]);
    assert!(!result.status.success());
    assert!(!dir.path().join("state").exists());
    let calls = fs::read_to_string(dir.path().join("calls")).unwrap();
    assert!(!calls.contains("issues/7?"));
    assert!(!calls.contains("user"));
    assert!(!calls.contains("PATCH") && !calls.contains("POST"));
}

#[test]
fn preview_selected_target_prints_summary_without_state_or_writes() {
    let dir = tempfile::tempdir().unwrap();
    let (config, path) = fixture(dir.path(), "acoliver");
    let result = run(&config, &path, &["--issues", "7"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(summary["candidates"][0]["issue_number"], 7);
    assert_eq!(summary["mode"], "preview");
    assert!(!dir.path().join("state").exists());
    let calls = fs::read_to_string(dir.path().join("calls")).unwrap();
    assert!(!calls.contains("user"));
    assert!(!calls.contains("PATCH") && !calls.contains("POST"));
}

#[test]
fn identity_mismatch_blocks_execution_before_assignment_or_state() {
    let dir = tempfile::tempdir().unwrap();
    let (config, path) = fixture(dir.path(), "someone-else");
    let result = run(&config, &path, &["--issues", "7", "--execute"]);
    assert!(!result.status.success());
    assert!(!dir.path().join("state").exists());
    assert!(!dir.path().join("worktrees").exists());
    let calls = fs::read_to_string(dir.path().join("calls")).unwrap();
    assert!(calls.contains("api user --jq .login"));
    assert!(!calls.contains("PATCH") && !calls.contains("POST"));
}
