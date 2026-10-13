use super::*;
use crate::{
    config::{CheckRule, RequiredChecks},
    github::pull_request::RequiredChecksSource,
};

#[test]
fn configured_list_selects_exact_names_not_github_required_flags() {
    let checks = RequiredChecks::Names(vec!["chosen".into()]);
    let health = observe_with_checks("org/code", 7, &checks, |_| {
        Ok(page(
            vec![
                check("other", true, "FAILURE", 1),
                check("chosen", false, "FAILURE", 1),
            ],
            false,
        ))
    })
    .unwrap();
    assert_eq!(
        health.required_checks_source,
        RequiredChecksSource::ConfiguredList
    );
    assert_eq!(health.failed_required_checks, ["chosen"]);
    assert_eq!(health.failed_jobs.len(), 1);
    assert_eq!(health.failed_jobs[0].name, "chosen");
}

#[test]
fn all_includes_optional_checks_and_statuses_but_excludes_skipped() {
    let checks = RequiredChecks::Rule(CheckRule::All {});
    let mut skipped = check("skipped", false, "SKIPPED", 1);
    // Skipped jobs must not trigger an Actions detail read.
    skipped["detailsUrl"] = Value::Null;
    let health = observe_with_checks("org/code", 7, &checks, |_| {
        Ok(page(vec![skipped.clone(), check("chosen", false, "FAILURE", 1),
            json!({"__typename":"StatusContext","context":"release build + source bundle","state":"FAILURE","isRequired":false})], false))
    }).unwrap();
    assert_eq!(
        health.required_checks_source,
        RequiredChecksSource::ConfiguredAll
    );
    assert_eq!(
        health.failed_required_checks,
        ["release build + source bundle", "chosen"]
    );
    assert_eq!(health.required_jobs.len(), 1);
    assert_eq!(health.required_jobs[0].name, "chosen");
}

#[test]
fn named_gates_missing_or_pending_are_not_implicitly_green() {
    let checks = RequiredChecks::Names(vec!["chosen".into(), "missing".into()]);
    let mut pending = check("chosen", false, "SUCCESS", 1);
    pending["status"] = json!("QUEUED");
    pending["conclusion"] = Value::Null;
    let health = observe_with_checks("org/code", 7, &checks, |_| {
        Ok(page(vec![pending.clone()], false))
    })
    .unwrap();
    assert!(health.required_checks_pending);
    assert!(!health.needs_repair());
    let health = observe_with_checks("org/code", 7, &checks, |_| {
        Ok(page(vec![check("chosen", false, "SUCCESS", 1)], false))
    })
    .unwrap();
    assert!(health.required_checks_pending);
    let health = observe_with_checks("org/code", 7, &checks, |_| Ok(page(vec![check("chosen", false, "SUCCESS", 1), json!({"__typename":"StatusContext","context":"missing","state":"SUCCESS","isRequired":false})], false))).unwrap();
    assert!(!health.required_checks_pending);
}

#[test]
fn null_rollup_reports_source_and_keeps_named_gates_pending() {
    let mut response = page(vec![], false);
    *response
        .pointer_mut("/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup")
        .unwrap() = Value::Null;
    for (checks, source, pending) in [
        (RequiredChecks::default(), RequiredChecksSource::None, false),
        (
            RequiredChecks::Names(vec!["chosen".into()]),
            RequiredChecksSource::ConfiguredList,
            true,
        ),
        (
            RequiredChecks::Rule(CheckRule::All {}),
            RequiredChecksSource::ConfiguredAll,
            false,
        ),
    ] {
        let health = observe_with_checks("org/code", 7, &checks, |_| Ok(response.clone())).unwrap();
        assert_eq!(health.required_checks_source, source);
        assert_eq!(health.required_checks_pending, pending);
        assert!(!health.needs_repair());
    }
}

#[test]
fn configured_checks_keep_pagination_latest_attempt_and_job_head_verification() {
    for checks in [
        RequiredChecks::Names(vec!["chosen".into()]),
        RequiredChecks::Rule(CheckRule::All {}),
    ] {
        let mut calls = 0;
        let health = observe_with_checks("org/code", 7, &checks, |_| {
            calls += 1;
            Ok(page(
                vec![check(
                    "chosen",
                    false,
                    if calls == 1 { "FAILURE" } else { "SUCCESS" },
                    calls,
                )],
                calls == 1,
            ))
        })
        .unwrap();
        assert!(!health.needs_repair());
        assert!(!health.required_checks_pending);
        assert_eq!(health.required_jobs[0].run_attempt, 2);
        let result = health_reader::observe("org/code", 7, &checks, |args| {
            if args[1].contains("/actions/jobs/") {
                return Ok(
                    json!({"id":34,"run_id":12,"head_sha":"b".repeat(40),"name":"chosen","status":"completed","run_attempt":2}),
                );
            }
            Ok(page(vec![check("chosen", false, "FAILURE", 2)], false))
        });
        assert!(result.is_err());
    }
}

#[test]
fn default_unprotected_checks_report_none_and_protected_checks_report_github() {
    let health = observe("org/code", 7, |_| {
        Ok(page(vec![check("optional", false, "FAILURE", 1)], false))
    })
    .unwrap();
    assert_eq!(health.required_checks_source, RequiredChecksSource::None);
    assert!(!health.needs_repair());
    assert!(health.required_jobs.is_empty());
    let health = observe("org/code", 7, |_| {
        Ok(page(vec![check("required", true, "FAILURE", 1)], false))
    })
    .unwrap();
    assert_eq!(health.required_checks_source, RequiredChecksSource::Github);
    assert!(health.needs_repair());
}
