use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
use tempfile::tempdir;

fn run(fake_response: &str) -> std::process::Output {
    let dir = tempdir().unwrap();
    let bin = dir.path().join("gh");
    let response = fake_response.replace('\'', "'\\''");
    let script = format!(
        "#!/bin/sh\ncase \"$2\" in graphql) printf '%s\\n' '{response}' ;; repos/org/tracker) printf '%s\\n' '{{\"node_id\":\"REPO_NODE\"}}' ;; repos/org/tracker/issues/7?per_page=100) printf '%s\\n' '{{\"node_id\":\"ISSUE_NODE\",\"repository_url\":\"https://api.github.com/repos/org/tracker\",\"html_url\":\"https://github.com/org/tracker/issues/7\",\"number\":7,\"state\":\"open\",\"assignees\":[],\"labels\":[{{\"name\":\"ready\"}}],\"milestone\":null}}' ;; *) printf '%s\\n' '{{}}' ;; esac\n"
    );
    fs::write(&bin, script).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let config = dir.path().join("config.json");
    fs::write(
        &config,
        format!(
            r#"{{"state_root":"{}","worktree_root":"{}","capacity":1,"sources":[{{"project_id":"PROJECT","repositories":["org/tracker"],"ready_marker":{{"kind":"label","name":"ready"}},"milestone":null}}],"mappings":[{{"tracker_repository":"org/tracker","code_repository":"org/code","checkout":"/tmp/code","base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/head","allowed_pr_author":"agent"}}],"initial":{{"executable":"agent","args":[]}},"resume":{{"executable":"agent","args":[]}}}}"#,
            dir.path().join("state").display(),
            dir.path().display()
        ),
    )
    .unwrap();
    Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(["discover", "--config", config.to_str().unwrap()])
        .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
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
fn discover_fails_closed_for_malformed_or_non_issue_project() {
    let malformed = run(r#"{"data":{"node":{"items":null}}}"#);
    assert!(!malformed.status.success());
    assert!(malformed.stdout.is_empty());

    let non_issue = run(&project(r#"{"__typename":"DraftIssue","title":"draft"}"#));
    assert!(!non_issue.status.success());
    assert!(non_issue.stdout.is_empty());
}
