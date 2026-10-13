//! Real gh health selection on an unprotected repository, through repair dispatch.
use super::*;
use luthor::{
    config::{CheckRule, RequiredChecks},
    github::pull_request::GhPullRequestReader,
};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

fn configure_gh(f: &mut Fixture) {
    let gh = f._dir.path().join("gh");
    fs::write(&gh, r#"#!/bin/sh
printf '%s\n' "$*" >> "$0.calls"
case "$*" in
  'api graphql '*) cat "$0.health" ;;
  'api user') printf '%s\n' '{"login":"acoliver"}' ;;
  'api repos/org/code/actions/runs/123') cat "$0.run" ;;
  'api repos/org/code/actions/jobs/456') cat "$0.job" ;;
  'api repos/org/code/actions/jobs/456/logs') printf '%s\n' 'assertion failed: configured gate regression' ;;
  'api --method POST repos/org/code/actions/runs/123/rerun-failed-jobs') exit 0 ;;
  *) echo "unexpected gh call: $*" >&2; exit 1 ;;
esac
"#).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(gh.with_extension("run"), json!({"id":123,"head_sha":f.prs.head,"repository":{"full_name":"org/code"},"status":"completed"}).to_string()).unwrap();
    f.prs.gh_health = Some(GhPullRequestReader::new(gh));
    health_page(f, 1, "FAILURE");
}

fn health_page(f: &Fixture, attempt: u64, conclusion: &str) {
    let gh = &f.prs.gh_health.as_ref().unwrap().executable;
    let check = |name: &str, result: &str| {
        json!({
            "__typename":"CheckRun", "databaseId":attempt, "name":name,
            "isRequired":false,"status":"COMPLETED","conclusion":result,
            "detailsUrl":"https://github.com/org/code/actions/runs/123/job/456",
            "checkSuite":{"databaseId":9,"workflowRun":{"databaseId":123,"runAttempt":attempt}}
        })
    };
    let response = json!({"data":{"repository":{"pullRequest":{
        "databaseId":4242,"headRefOid":f.prs.head,"mergeable":"MERGEABLE",
        "commits":{"nodes":[{"commit":{"oid":f.prs.head,"statusCheckRollup":{
            "contexts":{"nodes":[check("quality gates", conclusion),check("skipped gate", "SKIPPED")],
                "pageInfo":{"hasNextPage":false,"endCursor":null}}
        }}}]}
    }}}});
    fs::write(gh.with_extension("health"), response.to_string()).unwrap();
    fs::write(gh.with_extension("job"), json!({"id":456,"run_id":123,"head_sha":f.prs.head,"name":"quality gates","status":"completed","run_attempt":attempt}).to_string()).unwrap();
}

fn displayed(f: &Fixture, args: &[&str]) -> Value {
    serde_json::from_str(
        &luthor::cli::execute(
            &f.config.state_root,
            &args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn assert_source(f: &Fixture, source: &str) {
    assert_eq!(
        displayed(f, &["show", "task"])["required_checks_source"],
        source
    );
    let status = displayed(f, &["status"]);
    assert_eq!(status["tasks"][0]["required_checks_source"], source);
    assert_eq!(status["tasks"][0]["last_pr_health"]["head"], f.prs.head);
}

#[test]
fn configured_checks_without_branch_protection_rerun_once_then_repair() {
    for (checks, source) in [
        (
            RequiredChecks::Names(vec!["quality gates".into()]),
            "configured_list",
        ),
        (RequiredChecks::Rule(CheckRule::All {}), "configured_all"),
    ] {
        let mut f = Fixture::with_checks(checks);
        configure_gh(&mut f);
        assert!(f.repair().unwrap().is_empty());
        assert_eq!(f.prs.reruns, 1);
        assert_source(&f, source);
        assert!(f.repair().unwrap().is_empty());
        assert_eq!(f.prs.reruns, 1);
        // The rerun history survives restarting the local store.
        drop(f.store);
        f.store = StateStore::open(&f.config.state_root, 1).unwrap();
        health_page(&f, 2, "FAILURE");
        let plan = f.repair().unwrap().remove(0);
        assert!(
            plan.args
                .last()
                .unwrap()
                .contains("configured gate regression")
        );
        assert!(plan.args.last().unwrap().contains("quality gates"));
        assert!(!plan.args.last().unwrap().contains("skipped gate"));
        assert_eq!(plan.session_id, "task");
        assert_eq!(f.prs.reruns, 1);
        let gh = &f.prs.gh_health.as_ref().unwrap().executable;
        let calls = fs::read_to_string(gh.with_extension("calls")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.contains("--method POST"))
                .count(),
            1
        );
        assert!(calls.contains("api user\napi repos/org/code/actions/runs/123\napi --method POST"));
        assert!(calls.contains("api repos/org/code/actions/jobs/456/logs"));
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    }
}

#[test]
fn unconfigured_unprotected_repository_reports_none_without_rerun_or_repair() {
    let mut f = Fixture::new();
    configure_gh(&mut f);
    assert!(f.repair().unwrap().is_empty());
    assert_source(&f, "none");
    assert_eq!(f.prs.reruns, 0);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    let gh = &f.prs.gh_health.as_ref().unwrap().executable;
    let calls = fs::read_to_string(gh.with_extension("calls")).unwrap();
    assert!(!calls.contains("actions/"));
}

#[test]
fn configured_rerun_that_passes_records_flaky_evidence_without_worker() {
    let mut f = Fixture::with_checks(RequiredChecks::Names(vec!["quality gates".into()]));
    configure_gh(&mut f);
    assert!(f.repair().unwrap().is_empty());
    health_page(&f, 2, "SUCCESS");
    assert!(f.repair().unwrap().is_empty());
    assert_eq!(f.prs.reruns, 1);
    assert!(
        journal::evidence_kinds(&f.store, "task")
            .unwrap()
            .contains(&"pr_checks_flaky_looking".into())
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}

#[test]
fn changing_mapping_check_policy_does_not_silently_change_selected_task() {
    let mut f = Fixture::new();
    configure_gh(&mut f);
    f.config.mappings[0].required_checks = RequiredChecks::Rule(CheckRule::All {});
    let outcome = f.repair_outcome();
    assert!(outcome.launched.is_empty() && outcome.held.is_empty());
    assert_eq!(
        outcome.skipped[0].reason,
        "selection config differs: mappings"
    );
    assert_eq!(f.prs.reruns, 0);
    assert!(
        !f.prs
            .gh_health
            .as_ref()
            .unwrap()
            .executable
            .with_extension("calls")
            .exists()
    );
}

#[test]
fn configured_non_actions_failure_requires_person_without_worker() {
    let mut f = Fixture::with_checks(RequiredChecks::Names(vec!["quality gates".into()]));
    configure_gh(&mut f);
    let gh = &f.prs.gh_health.as_ref().unwrap().executable;
    let mut response: Value =
        serde_json::from_slice(&fs::read(gh.with_extension("health")).unwrap()).unwrap();
    response.pointer_mut("/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup/contexts/nodes/0/checkSuite/workflowRun").unwrap().take();
    fs::write(gh.with_extension("health"), response.to_string()).unwrap();
    assert!(f.repair().unwrap().is_empty());
    assert_eq!(f.prs.reruns, 0);
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert!(
        task_records::held_reason(&f.store, "task")
            .unwrap()
            .unwrap()
            .contains("no rerunnable Actions job")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}
