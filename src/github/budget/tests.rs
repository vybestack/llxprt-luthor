#![cfg(unix)]
use super::*;
use crate::{
    claim::{AssignmentWriter, GhAssignmentWriter},
    github::read,
};
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn transport_counts_each_retry_rest_write_and_graphql_call_and_updates_quota_headers() {
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    let marker = dir.path().join("retried");
    fs::write(&gh, format!(r#"#!/bin/sh
case "$2" in
rest) if test ! -f '{marker}'; then
    touch '{marker}'
    printf 'HTTP/2 503\r\nX-RateLimit-Remaining: 505\r\nX-RateLimit-Reset: 1000\r\nRetry-After: 0\r\n\r\n{{}}'; exit 1
  fi
  printf 'HTTP/2 200\r\nX-RateLimit-Remaining: 504\r\nX-RateLimit-Reset: 1000\r\n\r\n{{}}' ;;
graphql) printf 'HTTP/2 200\r\nX-RateLimit-Resource: graphql\r\nX-RateLimit-Remaining: 499\r\nX-RateLimit-Reset: 2000\r\n\r\n{{}}' ;;
-X) printf '%s\n' '{{}}' ;;
*) exit 99 ;;
esac
"#, marker=marker.display())).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    let _guard = CycleGuard::start(&GithubPolling::default());
    assert!(read::api(&gh, &["api", "rest"]).unwrap().status.success());
    assert_eq!(usage().rest, 2);
    assert!(optional_allowed());
    let mut writer = GhAssignmentWriter {
        executable: gh.clone(),
    };
    writer.assign("fixture/repo", 1, "acoliver").unwrap();
    assert_eq!(usage().rest, 3);
    assert!(
        read::api(&gh, &["api", "graphql"])
            .unwrap()
            .status
            .success()
    );
    assert_eq!(usage().graphql, 1);
    assert_eq!(usage().core_remaining, Some(504));
    assert_eq!(usage().core_reset, Some(1000));
    assert_eq!(usage().graphql_remaining, Some(499));
    assert_eq!(usage().graphql_reset, Some(2000));
    assert!(!optional_allowed());
    // The reserve is a scheduling boundary, never a transport/write veto.
    writer.assign("fixture/repo", 1, "acoliver").unwrap();
    assert_eq!(usage().rest, 4);
}

#[test]
fn accounting_is_cycle_scoped_and_malformed_quota_is_not_treated_as_unlimited() {
    {
        let _guard = CycleGuard::start(&GithubPolling::default());
        assert!(
            set_quota(&serde_json::json!({"resources":{"core":{"remaining":"oops","reset":1}}}))
                .is_err()
        );
        assert!(
            headers(
                b"HTTP/2 200\r\nX-RateLimit-Remaining: oops\r\n\r\n{}",
                false
            )
            .is_err()
        );
        headers(b"HTTP/2 200\r\nX-RateLimit-Remaining: 500\r\n\r\n{}", false).unwrap();
        assert!(!optional_allowed());
        command(&["api"]);
        defer_task("finished");
        assert!(!task_allowed("finished"));
    }
    let _guard = CycleGuard::start(&GithubPolling::default());
    assert_eq!(usage().rest, 0);
    assert!(optional_allowed() && task_allowed("finished"));
}
