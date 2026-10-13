use super::{health_reader, types::LookupError};
use serde_json::{Value, json};

// Model the separate immutable Actions job read as well as GraphQL pages.
fn observe(
    repository: &str,
    number: u64,
    mut pages: impl FnMut(&[&str]) -> Result<Value, LookupError>,
) -> Result<super::health::PrHealth, LookupError> {
    let mut job = Value::Null;
    health_reader::observe(repository, number, |args| {
        if args[1].contains("/actions/jobs/") {
            return Ok(job.clone());
        }
        let response = pages(args)?;
        if let Some(nodes) = response.pointer("/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup/contexts/nodes").and_then(Value::as_array) {
            for node in nodes {
                if node["isRequired"] == true && node["status"] == "COMPLETED" {
                    job = json!({"id":34,"run_id":12,"head_sha":"a".repeat(40),"name":node["name"],"status":"completed","run_attempt":node["checkSuite"]["workflowRun"]["runAttempt"]});
                }
            }
        }
        Ok(response)
    })
}

fn check(name: &str, required: bool, conclusion: &str, attempt: u64) -> Value {
    json!({"__typename":"CheckRun", "databaseId":attempt, "name":name, "isRequired":required, "status":"COMPLETED", "conclusion":conclusion,
        "detailsUrl":"https://github.com/org/code/actions/runs/12/job/34", "checkSuite":{"databaseId":9,"workflowRun":{"databaseId":12,"runAttempt":attempt}}})
}
fn page(nodes: Vec<Value>, more: bool) -> Value {
    json!({"data":{"repository":{"pullRequest":{"databaseId":42,"headRefOid":"a".repeat(40),"mergeable":"MERGEABLE",
        "commits":{"nodes":[{"commit":{"oid":"a".repeat(40),"statusCheckRollup":{"contexts":{"nodes":nodes,"pageInfo":{"hasNextPage":more,"endCursor":"next"}}}}}]}}}}})
}

#[test]
fn required_failure_paginated_health_keeps_job_identity() {
    let mut calls = 0;
    let health = observe("org/code", 7, |args| {
        calls += 1;
        if calls == 1 {
            Ok(page(vec![check("optional", false, "FAILURE", 1)], true))
        } else {
            assert!(args.contains(&"cursor=next"));
            Ok(page(vec![check("required", true, "FAILURE", 2)], false))
        }
    })
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(health.failed_required_checks, ["required"]);
    assert_eq!(health.failed_jobs[0].run_id, 12);
    assert_eq!(health.failed_jobs[0].run_attempt, 2);
    assert_eq!(health.failed_jobs[0].job_id, 34);
    assert!(health.needs_repair());
}

#[test]
fn unknown_mergeability_pending_or_optional_checks_never_trigger_repair() {
    let mut pending = check("required", true, "SUCCESS", 1);
    pending["status"] = json!("IN_PROGRESS");
    pending["conclusion"] = Value::Null;
    let mut response = page(vec![pending, check("optional", false, "FAILURE", 1)], false);
    response["data"]["repository"]["pullRequest"]["mergeable"] = json!("UNKNOWN");
    let health = observe("org/code", 7, |_| Ok(response.clone())).unwrap();
    assert!(!health.needs_repair());
    assert!(health.required_checks_pending);
    response["data"]["repository"]["pullRequest"]["mergeable"] = json!("CONFLICTING");
    assert!(
        observe("org/code", 7, |_| Ok(response.clone()))
            .unwrap()
            .conflicting
    );
}

#[test]
fn malformed_or_changed_health_fails_instead_of_authorizing_repair() {
    for scenario in [
        "missing_required",
        "unknown_status",
        "errors",
        "changed_head",
        "missing_cursor",
    ] {
        let mut calls = 0;
        let result = observe("org/code", 7, |_| {
            calls += 1;
            let mut response = page(
                vec![check("required", true, "FAILURE", 1)],
                scenario == "changed_head",
            );
            match scenario {
                "missing_required" => {
                    response.pointer_mut("/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup/contexts/nodes/0").unwrap().as_object_mut().unwrap().remove("isRequired");
                }
                "unknown_status" => {
                    response["data"]["repository"]["pullRequest"]["mergeable"] = json!("invalid")
                }
                "errors" => response["errors"] = json!([{"message":"denied"}]),
                "changed_head" if calls == 2 => {
                    response["data"]["repository"]["pullRequest"]["headRefOid"] =
                        json!("b".repeat(40))
                }
                "missing_cursor" => {
                    let info = response.pointer_mut("/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup/contexts/pageInfo").unwrap();
                    info["hasNextPage"] = json!(true);
                    info["endCursor"] = Value::Null;
                }
                _ => {}
            }
            Ok(response)
        });
        assert!(result.is_err(), "{scenario}");
    }
}

#[test]
fn rerun_new_check_replaces_old_failure_even_across_pages() {
    let mut calls = 0;
    let health = observe("org/code", 7, |_| {
        calls += 1;
        if calls == 1 {
            Ok(page(vec![check("required", true, "FAILURE", 1)], true))
        } else {
            Ok(page(vec![check("required", true, "SUCCESS", 2)], false))
        }
    })
    .unwrap();
    assert!(!health.needs_repair());
    assert!(health.failed_jobs.is_empty());
    assert_eq!(health.required_jobs[0].run_attempt, 2);
}

#[test]
fn old_job_does_not_become_repeat_failure_when_suite_attempt_advances() {
    let health = health_reader::observe("org/code", 7, |args| {
        if args[1].contains("/actions/jobs/") {
            return Ok(json!({"id":34,"run_id":12,"head_sha":"a".repeat(40),"name":"required","status":"completed","run_attempt":1}));
        }
        Ok(page(vec![check("required", true, "FAILURE", 2)], false))
    }).unwrap();
    assert!(health.required_checks_pending);
    assert!(!health.needs_repair());
    assert!(health.failed_jobs.is_empty());
}

#[test]
fn actions_job_read_must_match_graphql_identity() {
    for field in ["id", "run_id", "head_sha", "name", "status", "run_attempt"] {
        let result = health_reader::observe("org/code", 7, |args| {
            if args[1].contains("/actions/jobs/") {
                let mut job = json!({"id":34,"run_id":12,"head_sha":"a".repeat(40),"name":"required","status":"completed","run_attempt":2});
                job[field] = Value::Null;
                return Ok(job);
            }
            Ok(page(vec![check("required", true, "FAILURE", 2)], false))
        });
        assert!(result.is_err(), "{field}");
    }
}
