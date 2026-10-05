use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use luthor::github::{
    project::{GhProjectReader, ProjectItem, ProjectReader, ReadOperation},
    pull_request::{GhPullRequestReader, PullRequestReader},
};
use serde_json::{Value, json};

fn gh_script(script: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gh");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, path)
}

fn project_page() -> Value {
    json!({"data":{"node":{"items":{
        "nodes":[{"id":"ITEM", "content":{
            "__typename":"Issue", "id":"ISSUE", "number":7,
            "repository":{"id":"REPO", "nameWithOwner":"org/repo"}},
            "fieldValues":{"nodes":[], "pageInfo":{"hasNextPage":false}}}],
        "pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
}

#[test]
fn project_pagination_rejects_missing_malformed_and_empty_next_cursors() {
    for (page_info, code) in [
        (json!({"hasNextPage":false}), "invalid-page-info"),
        (
            json!({"hasNextPage":"false","endCursor":null}),
            "invalid-page-info",
        ),
        (
            json!({"hasNextPage":true,"endCursor":null}),
            "missing-page-cursor",
        ),
        (
            json!({"hasNextPage":true,"endCursor":""}),
            "missing-page-cursor",
        ),
        (
            json!({"hasNextPage":false,"endCursor":9}),
            "invalid-page-info",
        ),
    ] {
        let mut response = project_page();
        response["data"]["node"]["items"]["pageInfo"] = page_info;
        let (_dir, path) = gh_script(&format!("printf '%s' '{response}'"));
        let error = GhProjectReader::new(path).page("P", None).unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.operation, ReadOperation::ProjectPage);
        assert_eq!(error.project_id, None);
        assert_eq!(error.item_id, None);
    }
}

#[test]
fn project_item_errors_preserve_specific_context_and_skip_pr_fields() {
    for (content, code) in [
        (None, "missing-project-item-content"),
        (Some(Value::Null), "null-project-item-content"),
        (Some(json!({})), "missing-project-item-content-type"),
        (
            Some(json!({"__typename":"DraftIssue"})),
            "unsupported-project-item-content-type",
        ),
    ] {
        let mut response = project_page();
        let node = &mut response["data"]["node"]["items"]["nodes"][0];
        match content {
            Some(content) => node["content"] = content,
            None => {
                node.as_object_mut().unwrap().remove("content");
            }
        }
        let (_dir, path) = gh_script(&format!("printf '%s' '{response}'"));
        let error = GhProjectReader::new(path).page("P", None).unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.project_id.as_deref(), Some("P"));
        assert_eq!(error.item_id.as_deref(), Some("ITEM"));
    }
    let mut response = project_page();
    response["data"]["node"]["items"]["nodes"][0] =
        json!({"id":"PR_ITEM", "content":{"__typename":"PullRequest"}});
    let (_dir, path) = gh_script(&format!("printf '%s' '{response}'"));
    assert!(
        GhProjectReader::new(path)
            .page("P", None)
            .unwrap()
            .items
            .is_empty()
    );
}

fn issue_item() -> ProjectItem {
    ProjectItem {
        item_id: "ITEM".into(),
        issue_node_id: "ISSUE".into(),
        repository: "org/repo".into(),
        tracker_repo_id: "REPO".into(),
        issue_number: 7,
        fields: vec![],
        unsupported_fields: vec![],
    }
}

fn issue_response() -> Value {
    json!({"node_id":"ISSUE", "number":7,
        "repository_url":"https://api.github.com/repos/org/repo",
        "html_url":"https://github.com/org/repo/issues/7", "state":"open",
        "assignees":[], "labels":[], "milestone":null})
}

fn issue_reader(response: &Value) -> (tempfile::TempDir, GhProjectReader) {
    let (_dir, path) = gh_script(&format!(
        "case \"$*\" in *issues/7*) printf '%s' '{response}' ;; *) printf '%s' '{{\"node_id\":\"REPO\"}}' ;; esac"
    ));
    (_dir, GhProjectReader::new(path))
}

#[test]
fn issue_collection_caps_and_milestone_absence_remain_errors() {
    for (field, value, code) in [
        (
            "assignees",
            json!(vec![json!({"login":"worker"}); 100]),
            "issue-subcollection-at-cap",
        ),
        (
            "labels",
            json!(vec![json!({"name":"ready"}); 100]),
            "issue-subcollection-at-cap",
        ),
        ("assignees", Value::Null, "invalid-issue-assignees"),
        ("labels", Value::Null, "invalid-issue-labels"),
        (
            "milestone",
            json!({"title":"release"}),
            "invalid-issue-milestone",
        ),
    ] {
        let mut response = issue_response();
        response[field] = value;
        let (_dir, mut reader) = issue_reader(&response);
        let error = reader.issue(&issue_item()).unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.operation, ReadOperation::DirectIssue);
        assert_eq!(error.item_id.as_deref(), Some("ITEM"));
        assert_eq!(error.issue_id.as_deref(), Some("ISSUE"));
    }
    let mut response = issue_response();
    response.as_object_mut().unwrap().remove("milestone");
    let (_dir, mut reader) = issue_reader(&response);
    assert_eq!(
        reader.issue(&issue_item()).unwrap_err().code,
        "invalid-issue-milestone"
    );
}

fn pr_detail() -> Value {
    json!({"head":{"sha":"0123456789012345678901234567890123456789"}})
}

fn checks_detail(runs: &Value, statuses: &Value) -> Value {
    let detail = pr_detail();
    let script = format!(
        "case \"$*\" in *check-runs*) printf '%s' '{runs}' ;; *'/status'*) printf '%s' '{statuses}' ;; *) printf '%s' '{detail}' ;; esac"
    );
    let (_dir, path) = gh_script(&script);
    GhPullRequestReader::new(path)
        .detail("org/repo", 7)
        .unwrap()
}

#[test]
fn check_summary_validation_and_pending_conclusions_preserve_advisory_behavior() {
    let valid_run = json!({"name":"build", "status":"completed", "conclusion":null});
    let detail = checks_detail(
        &json!({"total_count":1, "check_runs":[valid_run.clone()]}),
        &json!({"statuses":[{"context":"legacy", "state":"failure"},
            {"context":"legacy", "state":"failure"}]}),
    );
    assert_eq!(
        detail["luthor_checks"],
        json!(["build:completed:pending", "legacy:failure"])
    );
    for (field, value) in [
        ("name", json!("")),
        ("name", json!("x".repeat(129))),
        ("name", json!("build\n")),
        ("status", json!("")),
        ("status", json!("Completed")),
        ("status", json!("x".repeat(33))),
        ("conclusion", json!("Failure")),
        ("conclusion", json!("x".repeat(33))),
    ] {
        let mut run = valid_run.clone();
        run[field] = value;
        let detail = checks_detail(
            &json!({"total_count":1,"check_runs":[run]}),
            &json!({"statuses":[]}),
        );
        assert!(detail.get("luthor_checks").is_none(), "{field}");
        assert_eq!(detail["head"], pr_detail()["head"]);
    }
}

#[test]
fn malformed_check_counts_and_statuses_are_unavailable_not_observed_empty() {
    for runs in [
        json!({"total_count":1,"check_runs":[]}),
        json!({"total_count":0,"check_runs":[{"name":"build","status":"completed"}]}),
        json!({"check_runs":[]}),
        json!({"total_count":0,"check_runs":null}),
    ] {
        assert!(
            checks_detail(&runs, &json!({"statuses":[]}))
                .get("luthor_checks")
                .is_none()
        );
    }
    let empty_runs = json!({"total_count":0,"check_runs":[]});
    for statuses in [
        json!({"statuses":[{"context":"", "state":"success"}]}),
        json!({"statuses":[{"context":"legacy", "state":"Success"}]}),
        json!({"statuses":null}),
        json!({}),
    ] {
        assert!(
            checks_detail(&empty_runs, &statuses)
                .get("luthor_checks")
                .is_none()
        );
    }
    assert_eq!(
        checks_detail(&empty_runs, &json!({"statuses":[]}))["luthor_checks"],
        json!([])
    );
}

#[test]
fn check_run_pagination_stops_at_ten_pages_and_preserves_duplicate_runs() {
    for (total, expected_count, expected_status_calls) in [(1000, Some(1000), 1), (1001, None, 0)] {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let runs = json!({"total_count":total, "check_runs":vec![
            json!({"name":"same", "status":"completed", "conclusion":"failure"});100]});
        let detail = pr_detail();
        let script = format!(
            "echo \"$2\" >> '{}'\ncase \"$2\" in *check-runs*) printf '%s' '{runs}' ;; *'/status'*) printf '%s' '{{\"statuses\":[]}}' ;; *) printf '%s' '{detail}' ;; esac",
            calls.display()
        );
        let (_script_dir, path) = gh_script(&script);
        let actual = GhPullRequestReader::new(path)
            .detail("org/repo", 7)
            .unwrap();
        assert_eq!(
            actual
                .get("luthor_checks")
                .map(|checks| checks.as_array().unwrap().len()),
            expected_count
        );
        if let Some(checks) = actual.get("luthor_checks") {
            assert!(
                checks
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|check| check == "same:completed:failure")
            );
        }
        let calls = fs::read_to_string(calls).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.contains("check-runs"))
                .count(),
            10
        );
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.ends_with("/status"))
                .count(),
            expected_status_calls
        );
        assert!(!calls.contains("page=11"));
    }
}

#[test]
fn check_and_status_page_caps_do_not_change_pr_detail() {
    let runs = json!({"total_count":101, "check_runs":vec![
        json!({"name":"check", "status":"completed"});101]});
    let actual = checks_detail(&runs, &json!({"statuses":[]}));
    assert_eq!(actual, pr_detail());
    let empty_runs = json!({"total_count":0,"check_runs":[]});
    let status = json!({"context":"legacy", "state":"failure"});
    let oversized = json!({"statuses":vec![status.clone();1001]});
    assert_eq!(checks_detail(&empty_runs, &oversized), pr_detail());
    let at_cap = json!({"statuses":vec![status;1000]});
    assert_eq!(
        checks_detail(&empty_runs, &at_cap)["luthor_checks"],
        json!(["legacy:failure"])
    );
}
