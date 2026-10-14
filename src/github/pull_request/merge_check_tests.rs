use super::{MergeChecks, health_reader};
use crate::config::{CheckRule, RequiredChecks};
use serde_json::{Value, json};

fn check(name: &str, conclusion: &str, required: bool, id: u64) -> Value {
    json!({"__typename":"CheckRun","databaseId":id,"name":name,"status":"COMPLETED","conclusion":conclusion,"isRequired":required,
        "checkSuite":{"databaseId":id,"workflowRun":null}})
}
fn response(nodes: Vec<Value>) -> Value {
    json!({"data":{"repository":{"pullRequest":{"databaseId":42,"headRefOid":"a".repeat(40),"mergeable":"MERGEABLE",
        "commits":{"nodes":[{"commit":{"oid":"a".repeat(40),"statusCheckRollup":{"contexts":{"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}]}}}}})
}
fn observe(nodes: Vec<Value>, rule: RequiredChecks) -> MergeChecks {
    health_reader::observe_checks("org/code", 7, &rule, true, |_| Ok(response(nodes.clone())))
        .unwrap()
}

#[test]
fn merge_all_accepts_success_and_skipped_but_names_neutral_and_failed_checks() {
    let observed = observe(
        vec![
            check("passing", "SUCCESS", false, 1),
            check("skip", "SKIPPED", false, 2),
            check("neutral", "NEUTRAL", false, 3),
            check("failed", "FAILURE", false, 4),
        ],
        RequiredChecks::Rule(CheckRule::All {}),
    );
    assert_eq!(
        observed.health.failed_required_checks,
        ["neutral", "failed"]
    );
    assert!(!observed.health.required_checks_pending);
    assert_eq!(
        observed
            .summary
            .iter()
            .map(|summary| summary.state.as_str())
            .collect::<Vec<_>>(),
        ["SUCCESS", "SKIPPED", "NEUTRAL", "FAILURE"]
    );
    assert!(observed.summary.iter().all(|summary| summary.required));
}

#[test]
fn merge_named_and_github_rules_record_optional_failures_without_requiring_them() {
    for rule in [
        RequiredChecks::Names(vec!["gate".into()]),
        RequiredChecks::Rule(CheckRule::Github {}),
    ] {
        let observed = observe(
            vec![
                check("gate", "SUCCESS", true, 1),
                check("optional", "FAILURE", false, 2),
            ],
            rule,
        );
        assert!(observed.health.failed_required_checks.is_empty());
        assert!(!observed.health.required_checks_pending);
        assert!(observed.summary[0].required);
        assert!(!observed.summary[1].required);
        assert_eq!(observed.summary[1].state, "FAILURE");
    }
}

#[test]
fn merge_missing_named_pending_status_and_pending_check_are_not_success() {
    let observed = observe(vec![], RequiredChecks::Names(vec!["missing".into()]));
    assert!(observed.health.required_checks_pending);
    let mut pending = check("CI", "SUCCESS", true, 1);
    pending["status"] = json!("IN_PROGRESS");
    pending["conclusion"] = Value::Null;
    let status = json!({"__typename":"StatusContext","context":"external","state":"PENDING","isRequired":true});
    let observed = observe(
        vec![pending, status],
        RequiredChecks::Rule(CheckRule::All {}),
    );
    assert!(observed.health.required_checks_pending);
    assert!(
        observed
            .summary
            .iter()
            .all(|summary| summary.state == "PENDING")
    );
}

#[test]
fn merge_failed_external_status_and_nonpassing_conclusions_are_named() {
    for conclusion in [
        "FAILURE",
        "CANCELLED",
        "TIMED_OUT",
        "STALE",
        "ACTION_REQUIRED",
        "STARTUP_FAILURE",
        "NEUTRAL",
    ] {
        let observed = observe(
            vec![check("CI", conclusion, true, 1)],
            RequiredChecks::Rule(CheckRule::All {}),
        );
        assert_eq!(observed.health.failed_required_checks, ["CI"]);
    }
    let observed = observe(
        vec![
            json!({"__typename":"StatusContext","context":"external","state":"ERROR","isRequired":true}),
        ],
        RequiredChecks::Rule(CheckRule::All {}),
    );
    assert_eq!(observed.health.failed_required_checks, ["external"]);
}

#[test]
fn merge_check_pagination_fails_closed_on_changed_head_or_malformed_results() {
    let rule = RequiredChecks::Rule(CheckRule::All {});
    let mut calls = 0;
    let result = health_reader::observe_checks("org/code", 7, &rule, true, |_| {
        calls += 1;
        let mut page = response(vec![]);
        if calls == 1 {
            let info = page.pointer_mut("/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup/contexts/pageInfo").unwrap();
            info["hasNextPage"] = json!(true);
            info["endCursor"] = json!("next");
        } else {
            page["data"]["repository"]["pullRequest"]["headRefOid"] = json!("b".repeat(40));
        }
        Ok(page)
    });
    assert!(result.is_err());
    let result = health_reader::observe_checks("org/code", 7, &rule, true, |_| {
        Ok(json!({"errors":[{"message":"denied"}]}))
    });
    assert!(result.is_err());
}
