use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
use tempfile::tempdir;

fn run(fake_response: &str) -> std::process::Output {
    let dir = tempdir().unwrap();
    run_in(dir.path(), fake_response, 1)
}

fn run_in(dir: &std::path::Path, fake_response: &str, capacity: usize) -> std::process::Output {
    let bin = dir.join("gh");
    let response = fake_response.replace('\'', "'\\''");
    let script = format!(
        "#!/bin/sh\ncase \"$2\" in graphql) printf '%s\\n' '{response}' ;; repos/org/tracker) printf '%s\\n' '{{\"node_id\":\"REPO_NODE\"}}' ;; repos/org/tracker/issues/7?per_page=100) printf '%s\\n' '{{\"node_id\":\"ISSUE_NODE\",\"repository_url\":\"https://api.github.com/repos/org/tracker\",\"html_url\":\"https://github.com/org/tracker/issues/7\",\"number\":7,\"state\":\"open\",\"assignees\":[],\"labels\":[{{\"name\":\"ready\"}}],\"milestone\":null}}' ;; *) printf '%s\\n' '{{}}' ;; esac\n"
    );
    fs::write(&bin, script).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let config = dir.join("config.json");
    fs::write(
        &config,
        format!(
            r#"{{"state_root":"{}","worktree_root":"{}","capacity":{},"sources":[{{"project_id":"PROJECT","repositories":["org/tracker"],"ready_marker":{{"kind":"label","name":"ready"}},"milestone":null}}],"mappings":[{{"tracker_repository":"org/tracker","code_repository":"org/code","checkout":"/tmp/code","base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/head","allowed_pr_author":"agent"}}],"initial":{{"executable":"agent","args":[]}},"resume":{{"executable":"agent","args":[]}}}}"#,
            dir.join("state").display(),
            dir.display(),
            capacity
        ),
    )
    .unwrap();
    Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(["discover", "--config", config.to_str().unwrap()])
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .output()
        .unwrap()
}

fn project(content: &str) -> String {
    let content: Value = serde_json::from_str(content).unwrap();
    serde_json::json!({
        "data": { "node": { "items": {
            "nodes": [{
                "id": "ITEM",
                "content": content,
                "fieldValues": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } }
            }],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        }}}
    }).to_string()
}

#[test]
fn discover_prints_complete_eligible_candidate_as_json_line() {
    let output = run(&project(
        r#"{"__typename":"Issue","id":"ISSUE_NODE","number":7,"repository":{"id":"REPO_NODE","nameWithOwner":"org/tracker"}}"#,
    ));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["candidate"]["issue_number"], 7);
    assert_eq!(lines[0]["candidate"]["code_repository"], "org/code");
    assert_eq!(lines[0]["source"]["project_id"], "PROJECT");
    assert_eq!(lines[0]["evidence"]["state"], "open");
}

#[test]
fn discover_skips_pull_request_before_selecting_later_issue() {
    let response = serde_json::json!({
        "data": { "node": { "items": {
            "nodes": [
                {"id":"PR_ITEM","content":{"__typename":"PullRequest","id":"PR_NODE"},"fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}},
                {"id":"ISSUE_ITEM","content":{"__typename":"Issue","id":"ISSUE_NODE","number":7,"repository":{"id":"REPO_NODE","nameWithOwner":"org/tracker"}},"fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}
            ],
            "pageInfo": {"hasNextPage": false, "endCursor": null}
        }}}
    }).to_string();
    let output = run(&response);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["candidate"]["issue_number"], 7);
}

#[test]
fn discover_fails_closed_for_malformed_or_non_issue_project() {
    let malformed = run(r#"{"data":{"node":{"items":null}}}"#);
    assert!(!malformed.status.success());
    assert!(malformed.stdout.is_empty());

    let non_issue = run(&project(r#"{"__typename":"DraftIssue","title":"draft"}"#));
    assert!(!non_issue.status.success());
    assert!(non_issue.stdout.is_empty());
}

#[test]
fn discover_does_not_create_state_after_success_or_failure() {
    let dir = tempdir().unwrap();
    let state = dir.path().join("state");
    let success = run_in(
        dir.path(),
        &project(
            r#"{"__typename":"Issue","id":"ISSUE_NODE","number":7,"repository":{"id":"REPO_NODE","nameWithOwner":"org/tracker"}}"#,
        ),
        1,
    );
    assert!(success.status.success());
    assert!(!state.exists());

    let failure = run_in(dir.path(), r#"{"data":{"node":{"items":null}}}"#, 1);
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty());
    assert!(!state.exists());
}

#[test]
fn discover_ignores_live_state_lock_and_capacity_mismatch() {
    let dir = tempdir().unwrap();
    let state = dir.path().join("state");
    let _store = luthor::state::StateStore::open(&state, 1).unwrap();
    let output = run_in(
        dir.path(),
        &project(
            r#"{"__typename":"Issue","id":"ISSUE_NODE","number":7,"repository":{"id":"REPO_NODE","nameWithOwner":"org/tracker"}}"#,
        ),
        2,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 1);
}
