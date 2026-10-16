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

fn point_response(cost: u64, remaining: u64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"data":{"rateLimit":{
        "cost":cost,"remaining":remaining,"resetAt":"2026-10-17T04:35:00Z"
    }}}))
    .unwrap()
}

#[test]
fn reported_points_not_request_counts_reserve_headroom_for_the_next_read() {
    let _guard = CycleGuard::start(&GithubPolling::default());
    command(&["api", "graphql"]);
    graphql_response(&point_response(75, 600)).unwrap();
    assert_eq!(usage().graphql, 1);
    assert_eq!(usage().graphql_points, 75);
    assert!(optional_allowed());
    command(&["api", "graphql"]);
    graphql_response(&point_response(1, 574)).unwrap();
    assert_eq!(usage().graphql, 2);
    assert_eq!(usage().graphql_points, 76);
    assert_eq!(usage().graphql_reset, Some(1792211700));
    assert!(
        !optional_allowed(),
        "574 exceeds reserve 500 but not reserve + peak cost 75"
    );
    assert!(
        deferral_reason()
            .unwrap()
            .contains("PR maintenance deferred")
    );
}

#[test]
fn graphql_errors_without_data_mark_exhaustion_and_preserve_the_header_reset_time() {
    let _guard = CycleGuard::start(&GithubPolling::default());
    headers(
        b"HTTP/2 403\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 2000\r\n\r\n{}",
        true,
    )
    .unwrap();
    graphql_response(br#"{"errors":[{"type":"RATE_LIMITED"}],"data":null}"#).unwrap();
    assert!(!optional_allowed());
    let exhausted = usage().exhausted(1999);
    assert_eq!(exhausted.len(), 1);
    assert_eq!(exhausted[0].resource, "graphql");
    assert_eq!(exhausted[0].reset_at_unix_secs, Some(2000));
    assert!(usage().exhausted(2000).is_empty());
    graphql_response(br#"{"errors":[{"extensions":{"code":"RATE_LIMITED"}}]}"#).unwrap();
    assert_eq!(usage().graphql_remaining, Some(0));
}

#[test]
fn malformed_reported_graphql_cost_or_reset_is_an_error_not_unlimited_budget() {
    let _guard = CycleGuard::start(&GithubPolling::default());
    for (field, value) in [
        ("cost", serde_json::json!("75")),
        ("remaining", serde_json::json!(-1)),
        ("resetAt", serde_json::json!("2026-02-30T00:00:00Z")),
        ("resetAt", serde_json::json!("not a date")),
    ] {
        let mut response: serde_json::Value =
            serde_json::from_slice(&point_response(1, 5000)).unwrap();
        response["data"]["rateLimit"][field] = value;
        assert!(graphql_response(&serde_json::to_vec(&response).unwrap()).is_err());
    }
    assert_eq!(usage().graphql_points, 0);
}

#[test]
fn point_accounting_includes_each_response_on_retry_and_resets_between_cycles() {
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    let marker = dir.path().join("retried");
    let body = String::from_utf8(point_response(9, 4991)).unwrap();
    fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
printf 'HTTP/2 503\r\nRetry-After: 0\r\n\r\n%s' '{body}'
if test ! -f '{marker}'; then touch '{marker}'; exit 1; fi
"#,
            marker = marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    {
        let _guard = CycleGuard::start(&GithubPolling::default());
        assert!(
            read::api(&gh, &["api", "graphql"])
                .unwrap()
                .status
                .success()
        );
        assert_eq!(usage().graphql, 2);
        assert_eq!(usage().graphql_points, 18);
    }
    let _guard = CycleGuard::start(&GithubPolling::default());
    assert_eq!(usage().graphql_points, 0);
}

mod discovery;
