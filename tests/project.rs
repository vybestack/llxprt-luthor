#[test]
fn skips_pull_requests_and_continues_pagination() {
    let mixed = r#"{"data":{"node":{"items":{"nodes":[{"id":"PVTI_PR","content":{"__typename":"PullRequest","id":"PR1"},"fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}},{"id":"PVTI_ISSUE","content":{"__typename":"Issue","id":"ISSUE1","number":7,"repository":{"id":"REPO1","nameWithOwner":"org/tracker"}},"fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":true,"endCursor":"NEXT"}}}}}"#;
    let final_page = r#"{"data":{"node":{"items":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#;
    let dir = tempdir().unwrap();
    let path = dir.path().join("gh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$*\" in *NEXT*) printf '%s' '{}' ;; *) printf '%s' '{}' ;; esac\n",
            final_page.replace('\'', "'\\''"),
            mixed.replace('\'', "'\\''")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut reader = GhProjectReader::new(path);
    let first = reader.page("PROJECT", None).unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].item_id, "PVTI_ISSUE");
    assert!(first.has_next_page);
    let last = reader.page("PROJECT", first.end_cursor.as_deref()).unwrap();
    assert!(last.items.is_empty());
    assert!(!last.has_next_page);
}

#[test]
fn direct_issue_requires_stable_id_for_present_milestone() {
    use luthor::github::project::{ProjectItem, ReadCategory};
    for milestone in [
        r#"{"title":"0.12.0"}"#,
        r#"{"title":"0.12.0","node_id":""}"#,
    ] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("gh");
        let issue = format!(
            r#"{{"node_id":"ISSUE1","repository_url":"https://api.github.com/repos/org/tracker","number":7,"html_url":"https://github.com/org/tracker/issues/7","state":"open","assignees":[],"labels":[],"milestone":{milestone}}}"#
        );
        let script = "#!/bin/sh\ncase \"$*\" in *issues/7*) printf '%s' 'ISSUE_JSON' ;; *) printf '%s' 'REPO_JSON' ;; esac\n"
            .replace("ISSUE_JSON", &issue)
            .replace("REPO_JSON", r#"{"node_id":"REPO1"}"#);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let mut reader = GhProjectReader::new(path);
        let item = ProjectItem {
            item_id: "PVTI1".into(),
            issue_node_id: "ISSUE1".into(),
            repository: "org/tracker".into(),
            tracker_repo_id: "REPO1".into(),
            issue_number: 7,
            fields: vec![],
            unsupported_fields: Vec::new(),
        };
        assert_eq!(
            reader.issue(&item).unwrap_err().category,
            ReadCategory::Malformed
        );
    }
}

#[test]
fn fake_gh_paginates_two_pages_and_preserves_requested_fields() {
    let first = r#"{"data":{"node":{"items":{"nodes":[{"id":"PVTI1","content":{"__typename":"Issue","id":"ISSUE1","number":7,"repository":{"id":"REPO1","nameWithOwner":"org/tracker"}},"fieldValues":{"nodes":[{"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"Ready","field":{"name":"Status"}},{"__typename":"ProjectV2ItemFieldDateValue","name":"Due","date":"2026-01-01"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":true,"endCursor":"CURSOR1"}}}}}"#;
    let second = r#"{"data":{"node":{"items":{"nodes":[{"id":"PVTI2","content":{"__typename":"Issue","id":"ISSUE2","number":8,"repository":{"id":"REPO1","nameWithOwner":"org/tracker"}},"fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#;
    let dir = tempdir().unwrap();
    let path = dir.path().join("gh");
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(&path, format!("#!/bin/sh\ncase \"$*\" in *CURSOR1*) printf '%s\\n' '{}' ;; *) printf '%s\\n' '{}' ;; esac\n", second, first)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut reader = GhProjectReader::new(path);
    let page1 = reader.page("PROJECT", None).unwrap();
    assert_eq!(page1.items.len(), 1);
    assert_eq!(
        page1.items[0].fields,
        vec![("Status".into(), "Ready".into())]
    );
    assert!(page1.has_next_page);
    let page2 = reader.page("PROJECT", page1.end_cursor.as_deref()).unwrap();
    assert_eq!(page2.items[0].issue_node_id, "ISSUE2");
    assert!(!page2.has_next_page);
}

use luthor::github::project::{GhProjectReader, ProjectReader};
use std::{fs, os::unix::fs::PermissionsExt};
use tempfile::tempdir;

fn reader(response: &str) -> (tempfile::TempDir, GhProjectReader) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("gh");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s' '{}'\n",
            response.replace('\'', "'\\''")
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, GhProjectReader::new(path))
}
fn response(field_values: &str) -> String {
    format!(
        r#"{{"data":{{"node":{{"items":{{"nodes":[{{"id":"PVTI1","content":{{"__typename":"Issue","id":"ISSUE1","number":7,"repository":{{"id":"REPO1","nameWithOwner":"org/tracker"}}}},"fieldValues":{field_values}}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}"#
    )
}
#[test]
fn parses_project_fields_and_direct_issue_identity() {
    let fv = r#"{"nodes":[{"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"Ready","field":{"name":"Status"}},{"__typename":"ProjectV2ItemFieldTextValue","text":"Ada","field":{"name":"Owner"}},{"__typename":"ProjectV2ItemFieldDateValue","name":"Due","date":"2026-01-01"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}"#;
    let (_dir, mut r) = reader(&response(fv));
    let page = r.page("P", None).unwrap();
    assert_eq!(page.items[0].issue_node_id, "ISSUE1");
    assert_eq!(page.items[0].repository, "org/tracker");
    assert_eq!(page.items[0].tracker_repo_id, "REPO1");
    assert_eq!(page.items[0].issue_number, 7);
    assert_eq!(
        page.items[0].fields,
        vec![
            ("Status".into(), "Ready".into()),
            ("Owner".into(), "Ada".into())
        ]
    );
}
#[test]
fn missing_or_null_tracker_repository_id_fails_closed() {
    let fields = r#"{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}"#;
    for repository in [
        r#"{"nameWithOwner":"org/tracker"}"#,
        r#"{"id":null,"nameWithOwner":"org/tracker"}"#,
        r#"{"id":"","nameWithOwner":"org/tracker"}"#,
    ] {
        let json = response(fields).replace(
            r#"{"id":"REPO1","nameWithOwner":"org/tracker"}"#,
            repository,
        );
        let (_dir, mut reader) = reader(&json);
        assert_eq!(
            reader.page("P", None).unwrap_err().category,
            luthor::github::project::ReadCategory::Malformed
        );
    }
}
#[test]
fn graphql_errors_and_incomplete_field_values_fail() {
    let (_dir, mut r) = reader(r#"{"errors":[{"message":"broken"}]}"#);
    assert_eq!(r.page("P", None).unwrap_err().code, "graphql-error");
    let (_dir, mut r) = reader(&response(
        r#"{"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":"C"}}"#,
    ));
    assert_eq!(
        r.page("P", None).unwrap_err().category,
        luthor::github::project::ReadCategory::Malformed
    );
}
#[test]
fn classifies_plain_stderr_and_graphql_errors_without_exposing_messages() {
    use luthor::github::project::{ReadCategory, ReadOperation};

    for (stderr, expected, status) in [
        (
            "HTTP 403 forbidden secret=response-secret",
            ReadCategory::Permission,
            Some(403),
        ),
        (
            "HTTP 429 rate limit secret=response-secret",
            ReadCategory::RateLimit,
            Some(429),
        ),
        (
            "HTTP 403 rate limit secret=response-secret",
            ReadCategory::RateLimit,
            Some(403),
        ),
    ] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("gh");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s' 'not json'\necho '{}' >&2\nexit 1\n",
                stderr
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let mut reader = GhProjectReader::new(path);
        let error = reader.page("PROJECT-ERR", None).unwrap_err();
        assert_eq!(error.category, expected);
        assert_eq!(error.status, status);
        assert_eq!(error.project_id.as_deref(), Some("PROJECT-ERR"));
        assert_eq!(error.operation, ReadOperation::ProjectPage);
        assert!(!error.to_string().contains("response-secret"));
    }

    for (body, expected) in [
        (
            r#"{"errors":[{"type":"RATE_LIMITED","message":"response-secret"}]}"#,
            ReadCategory::RateLimit,
        ),
        (
            r#"{"errors":[{"extensions":{"code":"FORBIDDEN"},"message":"response-secret"}]}"#,
            ReadCategory::Permission,
        ),
        (
            r#"{"errors":[{"extensions":{"code":"NOT_FOUND"}}]}"#,
            ReadCategory::NotFound,
        ),
        (
            r#"{"errors":[{"message":"rate limit response-secret"}]}"#,
            ReadCategory::RateLimit,
        ),
    ] {
        let (_dir, mut reader) = reader(body);
        let error = reader.page("PROJECT-GQL", None).unwrap_err();
        assert_eq!(error.category, expected);
        assert_eq!(error.project_id.as_deref(), Some("PROJECT-GQL"));
        assert_eq!(error.operation, ReadOperation::ProjectPage);
        assert_eq!(error.code, "graphql-error");
        assert!(!error.to_string().contains("response-secret"));
    }
}

#[test]
fn rejects_non_issue_project_items_with_item_identity() {
    for content in [
        r#"{"__typename":"DraftIssue","title":"draft"}"#,
        r#"{"__typename":"UnknownContent"}"#,
        "null",
        "{}",
    ] {
        let json = format!(
            r#"{{"data":{{"node":{{"items":{{"nodes":[{{"id":"PVTI2","content":{content},"fieldValues":{{"nodes":[],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}"#
        );
        let (_dir, mut r) = reader(&json);
        let error = r.page("P", None).unwrap_err();
        assert_eq!(
            error.category,
            luthor::github::project::ReadCategory::Malformed
        );
        assert_eq!(error.project_id.as_deref(), Some("P"));
        assert_eq!(error.item_id.as_deref(), Some("PVTI2"));
        assert_eq!(error.issue_id, None);
    }
}

#[test]
fn classifies_safe_api_failure_categories_and_malformed_output() {
    use luthor::github::project::{ReadCategory, ReadOperation};
    for (body, expected, status) in [
        (
            r#"{"status":"403","message":"permission denied token=secret"}"#,
            ReadCategory::Permission,
            Some(403),
        ),
        (
            r#"{"status":"429","message":"rate limited token=secret"}"#,
            ReadCategory::RateLimit,
            Some(429),
        ),
    ] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("gh");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s' '{}'\necho 'secret stderr' >&2\nexit 1\n",
                body.replace('\'', "'\\''")
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let mut r = GhProjectReader::new(path);
        let error = r.page("PROJECT-1", None).unwrap_err();
        assert_eq!(error.category, expected);
        assert_eq!(error.status, status);
        assert_eq!(error.operation, ReadOperation::ProjectPage);
        assert_eq!(error.project_id.as_deref(), Some("PROJECT-1"));
        assert!(!error.to_string().contains("secret"));
    }
    let (_dir, mut r) = reader("not json");
    let error = r.page("PROJECT-2", None).unwrap_err();
    assert_eq!(error.category, ReadCategory::Malformed);
    assert_eq!(error.project_id.as_deref(), Some("PROJECT-2"));
}

#[test]
fn direct_issue_caches_repository_and_validates_identity_and_urls() {
    use luthor::github::project::ProjectItem;

    let dir = tempdir().unwrap();
    let path = dir.path().join("gh");
    let calls = dir.path().join("repository-calls");
    let issue = r#"{"node_id":"ISSUE1","repository_url":"https://api.github.com/repos/org/tracker","number":7,"html_url":"https://github.com/org/tracker/issues/7","state":"open","assignees":[],"labels":[],"milestone":null}"#;
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in *'repos/org/tracker/issues/7'*) printf '%s' '{}' ;; *) echo call >> '{}' ; printf '%s' '{{\"node_id\":\"REPO1\"}}' ;; esac\n",
        issue.replace('\'', "'\\''"),
        calls.display()
    );
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let mut reader = GhProjectReader::new(path);
    let item = ProjectItem {
        item_id: "PVTI1".into(),
        issue_node_id: "ISSUE1".into(),
        repository: "org/tracker".into(),
        tracker_repo_id: "REPO1".into(),
        issue_number: 7,
        fields: vec![],
        unsupported_fields: Vec::new(),
    };

    let first = reader.issue(&item).unwrap();
    let second = reader.issue(&item).unwrap();
    assert_eq!(first.repository, "org/tracker");
    assert_eq!(first.tracker_repo_id, "REPO1");
    assert_eq!(first.url, "https://github.com/org/tracker/issues/7");
    assert_eq!(first.node_id, "ISSUE1");
    assert_eq!(first, second);
    assert_eq!(fs::read_to_string(calls).unwrap().lines().count(), 1);
}

#[test]
fn direct_issue_rejects_repository_id_and_renamed_repository_urls() {
    use luthor::github::project::{ProjectItem, ReadCategory};

    for (repository_json, issue_json, expected_category) in [
        (
            r#"{"node_id":"OTHER"}"#,
            r#"{"node_id":"ISSUE1","repository_url":"https://api.github.com/repos/org/tracker","number":7,"html_url":"https://github.com/org/tracker/issues/7","state":"open","assignees":[],"labels":[],"milestone":null}"#,
            ReadCategory::Malformed,
        ),
        (
            r#"{"node_id":"REPO1"}"#,
            r#"{"node_id":"ISSUE1","repository_url":"https://api.github.com/repos/org/renamed","number":7,"html_url":"https://github.com/org/tracker/issues/7","state":"open","assignees":[],"labels":[],"milestone":null}"#,
            ReadCategory::Malformed,
        ),
        (
            r#"{"node_id":"REPO1"}"#,
            r#"{"node_id":"ISSUE1","repository_url":"https://api.github.com/repos/org/tracker","number":7,"html_url":"https://github.com/org/renamed/issues/7","state":"open","assignees":[],"labels":[],"milestone":null}"#,
            ReadCategory::Malformed,
        ),
    ] {
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in *issues/7*) printf '%s' '{}' ;; *) printf '%s' '{}' ;; esac\n",
            issue_json.replace('\'', "'\\''"),
            repository_json.replace('\'', "'\\''")
        );
        let dir = tempdir().unwrap();
        let path = dir.path().join("gh");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let mut reader = GhProjectReader::new(path);
        let item = ProjectItem {
            item_id: "PVTI1".into(),
            issue_node_id: "ISSUE1".into(),
            repository: "org/tracker".into(),
            tracker_repo_id: "REPO1".into(),
            issue_number: 7,
            fields: vec![],
            unsupported_fields: Vec::new(),
        };
        assert_eq!(reader.issue(&item).unwrap_err().category, expected_category);
    }
}

#[test]
fn iteration_field_value_is_reported_as_unsupported_by_name() {
    let fields = r#"{"nodes":[{"__typename":"ProjectV2ItemFieldIterationValue","field":{"name":"Sprint"}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}"#;
    let (_dir, mut reader) = reader(&response(fields));
    let page = reader.page("P", None).unwrap();
    assert_eq!(page.items[0].unsupported_fields, vec!["Sprint"]);
}

#[test]
fn unknown_field_value_type_fails_with_malformed_error() {
    let fields = r#"{"nodes":[{"__typename":"FutureProjectFieldValue"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}"#;
    let (_dir, mut reader) = reader(&response(fields));
    let error = reader.page("P", None).unwrap_err();
    assert_eq!(
        error.category,
        luthor::github::project::ReadCategory::Malformed
    );
    assert_eq!(error.code, "unsupported-project-field-value-type");
}

#[test]
fn direct_issue_errors_keep_context_for_repository_and_issue_responses() {
    use luthor::github::project::{ProjectItem, ReadCategory, ReadOperation};

    let item = ProjectItem {
        item_id: "PVTI_CONTEXT".into(),
        issue_node_id: "ISSUE_CONTEXT".into(),
        repository: "org/tracker".into(),
        tracker_repo_id: "REPO1".into(),
        issue_number: 7,
        fields: vec![],
        unsupported_fields: vec![],
    };
    for (metadata, issue, expected_category, expected_status) in [
        (
            r#"{"status":"403","message":"permission denied token=secret","documentation_url":"https://secret.invalid"}"#,
            "",
            ReadCategory::Permission,
            Some(403),
        ),
        (r#"{"node_id":null}"#, "", ReadCategory::Malformed, None),
        (
            r#"{"node_id":"REPO1"}"#,
            r#"{"node_id":null,"token":"secret"}"#,
            ReadCategory::Malformed,
            None,
        ),
    ] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("gh");
        let repo_exit = if expected_status.is_some() {
            "exit 1"
        } else {
            ":"
        };
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in *issues/7*) printf '%s' '{}' ;; *) printf '%s' '{}'; {} ;; esac\n",
            issue.replace('\'', "'\\''"),
            metadata.replace('\'', "'\\''"),
            repo_exit
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let mut reader = GhProjectReader::new(path);
        let error = reader.issue(&item).unwrap_err();
        assert_eq!(error.operation, ReadOperation::DirectIssue);
        assert_eq!(error.item_id.as_deref(), Some("PVTI_CONTEXT"));
        assert_eq!(error.issue_id.as_deref(), Some("ISSUE_CONTEXT"));
        assert_eq!(error.category, expected_category);
        assert_eq!(error.status, expected_status);
        assert!(!error.to_string().contains("secret"));
    }
}
