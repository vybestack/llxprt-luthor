use luthor::github::{
    project::{GhProjectReader, ProjectItem, ProjectReader},
    pull_request::{GhPullRequestReader, PullRequestReader},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

fn gh_script(script: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gh");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, path)
}

fn project_page() -> Value {
    json!({"data":{"node":{"items":{"nodes":[], "pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
}

fn issue_response() -> Value {
    json!({"node_id":"ISSUE", "number":7,"repository_url":"https://api.github.com/repos/org/repo",
        "html_url":"https://github.com/org/repo/issues/7", "state":"open", "assignees":[], "labels":[], "milestone":null})
}

fn issue_item() -> ProjectItem {
    ProjectItem {
        item_id: "ITEM".into(),
        issue_node_id: "ISSUE".into(),
        repository: "org/repo".into(),
        tracker_repo_id: "REPO".into(),
        issue_number: 7,
        summary: Some(luthor::github::project::ProjectIssueSummary {
            state: "open".into(),
            assignees: vec![],
            labels: vec!["ready".into()],
            milestone: None,
        }),
        fields: vec![],
        unsupported_fields: vec![],
    }
}
use luthor::claim::{AssignmentWriter, GhAssignmentWriter};

fn flaky_script(failures: usize, success: &str, status: u16) -> (tempfile::TempDir, PathBuf) {
    gh_script(&format!(
        "cd -- \"$(dirname -- \"$0\")\" || exit 99\nn=0; if test -f count; then n=$(cat count); fi\nn=$((n+1)); echo \"$n\" > count\nif test \"$n\" -le {failures}; then printf 'HTTP/2.0 {status} Error\\r\\nRetry-After: 0\\r\\n\\r\\n{{}}'; printf 'HTTP {status}' >&2; exit 1; fi\nprintf '%s' '{success}'"
    ))
}

#[test]
fn project_and_direct_issue_reads_retry_500_then_200() {
    let (dir, path) = flaky_script(1, &project_page().to_string(), 500);
    let page = GhProjectReader::new(path).page("P", None).unwrap();
    assert!(page.items.is_empty());
    assert_eq!(fs::read_to_string(dir.path().join("count")).unwrap(), "2\n");
    let (dir, path) = flaky_script(1, &issue_response().to_string(), 500);
    // The repository identity reply is independent; fail only the direct read.
    let script = fs::read_to_string(&path).unwrap().replace("n=0;", "if test \"$2\" = repos/org/repo; then printf '%s' '{\"node_id\":\"REPO\"}'; exit 0; fi\nn=0;");
    fs::write(&path, script).unwrap();
    assert_eq!(
        GhProjectReader::new(path)
            .issue(&issue_item())
            .unwrap()
            .number,
        7
    );
    assert_eq!(fs::read_to_string(dir.path().join("count")).unwrap(), "2\n");
}

#[test]
fn pr_reads_retry_429_and_resets_but_exhausted_500_retains_status() {
    let (dir, path) = flaky_script(1, "[]", 429);
    assert!(
        GhPullRequestReader::new(path)
            .page("org/repo", 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read_to_string(dir.path().join("count")).unwrap(), "2\n");
    let (dir, path) = flaky_script(1, "{}", 500);
    let script = fs::read_to_string(&path).unwrap().replace(
        "printf 'HTTP/2.0 500 Error\\r\\nRetry-After: 0\\r\\n\\r\\n{}'; printf 'HTTP 500' >&2",
        "printf 'connection reset by peer' >&2",
    );
    fs::write(&path, script).unwrap();
    assert_eq!(
        GhPullRequestReader::new(path)
            .detail("org/repo", 7)
            .unwrap(),
        json!({})
    );
    assert_eq!(fs::read_to_string(dir.path().join("count")).unwrap(), "2\n");
    let (dir, path) = flaky_script(99, "[]", 500);
    let error = GhPullRequestReader::new(path)
        .page("org/repo", 1)
        .unwrap_err();
    assert_eq!(error.status, Some(500));
    assert_eq!(fs::read_to_string(dir.path().join("count")).unwrap(), "3\n");
}

#[test]
fn ambiguous_assignment_500_is_never_retried() {
    let (dir, path) = flaky_script(1, "{}", 500);
    let error = GhAssignmentWriter { executable: path }
        .assign("org/repo", 7, "acoliver")
        .unwrap_err();
    assert!(error.ambiguous);
    assert_eq!(fs::read_to_string(dir.path().join("count")).unwrap(), "1\n");
}
