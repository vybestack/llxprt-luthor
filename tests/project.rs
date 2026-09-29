#[test]
fn fake_gh_paginates_two_pages_and_preserves_requested_fields() {
    let first = r#"{"data":{"node":{"items":{"nodes":[{"id":"PVTI1","content":{"__typename":"Issue","id":"ISSUE1","number":7,"repository":{"nameWithOwner":"org/tracker"}},"fieldValues":{"nodes":[{"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"Ready","field":{"name":"Status"}},{"__typename":"ProjectV2ItemFieldDateValue","name":"Due","date":"2026-01-01"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":true,"endCursor":"CURSOR1"}}}}}"#;
    let second = r#"{"data":{"node":{"items":{"nodes":[{"id":"PVTI2","content":{"__typename":"Issue","id":"ISSUE2","number":8,"repository":{"nameWithOwner":"org/tracker"}},"fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#;
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
        r#"{{"data":{{"node":{{"items":{{"nodes":[{{"id":"PVTI1","content":{{"__typename":"Issue","id":"ISSUE1","number":7,"repository":{{"nameWithOwner":"org/tracker"}}}},"fieldValues":{field_values}}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}"#
    )
}
#[test]
fn parses_project_fields_and_direct_issue_identity() {
    let fv = r#"{"nodes":[{"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"Ready","field":{"name":"Status"}},{"__typename":"ProjectV2ItemFieldTextValue","text":"Ada","field":{"name":"Owner"}},{"__typename":"ProjectV2ItemFieldDateValue","name":"Due","date":"2026-01-01"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}"#;
    let (_dir, mut r) = reader(&response(fv));
    let page = r.page("P", None).unwrap();
    assert_eq!(page.items[0].issue_node_id, "ISSUE1");
    assert_eq!(page.items[0].repository, "org/tracker");
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
fn graphql_errors_and_incomplete_field_values_fail() {
    let (_dir, mut r) = reader(r#"{"errors":[{"message":"broken"}]}"#);
    assert_eq!(r.page("P", None).unwrap_err(), "graphql-error");
    let (_dir, mut r) = reader(&response(
        r#"{"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":"C"}}"#,
    ));
    assert_eq!(
        r.page("P", None).unwrap_err(),
        "incomplete-project-field-values"
    );
}
#[test]
fn rejects_non_issue_project_items_with_item_identity() {
    for content in [
        r#"{"__typename":"DraftIssue","title":"draft"}"#,
        r#"{"__typename":"PullRequest","number":4}"#,
        "null",
        "{}",
    ] {
        let json = format!(
            r#"{{"data":{{"node":{{"items":{{"nodes":[{{"id":"PVTI2","content":{content},"fieldValues":{{"nodes":[],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}"#
        );
        let (_dir, mut r) = reader(&json);
        let error = r.page("P", None).unwrap_err();
        assert!(error.contains("PVTI2"), "{error}");
    }
}
