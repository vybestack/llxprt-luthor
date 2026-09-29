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
    let fv = r#"{"nodes":[{"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"Status","value":"Ready"},{"__typename":"ProjectV2ItemFieldTextValue","name":"Owner","text":"Ada"},{"__typename":"ProjectV2ItemFieldDateValue","name":"Due","date":"2026-01-01"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}"#;
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
fn skips_non_issue_nodes() {
    let json = r#"{"data":{"node":{"items":{"nodes":[{"id":"PVTI2","content":{"__typename":"DraftIssue","title":"draft"},"fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#;
    let (_dir, mut r) = reader(json);
    assert!(r.page("P", None).unwrap().items.is_empty());
}
