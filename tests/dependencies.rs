//! Behavioral coverage for configured ordering and live GitHub prerequisite reads.
mod dependency_fixture;
use dependency_fixture::Fixture;
use luthor::{
    config::{Config, IssueReference},
    dependencies::DependencyState,
    eligibility::{discover, select, select_target},
    github::project::{ProjectReader, ReadOperation},
};
use serde_json::{Value, json};

#[test]
fn chain_waits_for_open_prerequisite_outside_project_without_blocking_independent_work() {
    let f = Fixture::new();
    let mut reader = f.reader();
    let report = discover(&mut reader, &f.config.sources, &f.config.mappings, None).unwrap();
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].issue_number, 3);
    assert_eq!(report.waiting.len(), 1);
    assert_eq!(report.waiting[0].candidate.issue_number, 2);
    assert_eq!(report.waiting[0].dependencies[0].dependency.number, 1);
    assert_eq!(
        report.waiting[0].dependencies[0].state,
        DependencyState::Open
    );
    assert_eq!(
        select(&mut reader, &f.config.sources, &f.config.mappings)
            .unwrap()
            .len(),
        1
    );
    assert!(
        select_target(
            &mut reader,
            &f.config.sources,
            &f.config.mappings,
            "org/tracker",
            2
        )
        .unwrap()
        .is_empty()
    );
    f.set_state("closed", false, false);
    assert_eq!(
        select_target(
            &mut reader,
            &f.config.sources,
            &f.config.mappings,
            "org/tracker",
            2
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn closed_unmerged_pr_blocks_by_default_and_only_explicit_opt_in_satisfies_it() {
    let mut f = Fixture::new();
    f.set_state("closed", true, false);
    let report = discover(&mut f.reader(), &f.config.sources, &f.config.mappings, None).unwrap();
    assert_eq!(
        report.waiting[0].dependencies[0].state,
        DependencyState::ClosedUnmergedPr
    );
    assert_eq!(report.candidates[0].issue_number, 3);
    f.config.sources[0].dependencies[0].allow_closed_unmerged_prs = true;
    assert_eq!(
        select(&mut f.reader(), &f.config.sources, &f.config.mappings)
            .unwrap()
            .len(),
        2
    );
    f.config.sources[0].dependencies[0].allow_closed_unmerged_prs = false;
    f.set_state("closed", true, true);
    assert_eq!(
        select(&mut f.reader(), &f.config.sources, &f.config.mappings)
            .unwrap()
            .len(),
        2
    );
    f.set_state("open", true, false);
    f.config.sources[0].dependencies[0].allow_closed_unmerged_prs = true;
    assert_eq!(
        select(&mut f.reader(), &f.config.sources, &f.config.mappings)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn every_dependency_must_be_satisfied_with_repository_scoped_identity() {
    let mut f = Fixture::new();
    f.set_state("closed", false, false);
    f.config.sources[0].dependencies[0]
        .blocked_by
        .push(IssueReference {
            repository: "other/foundation".into(),
            number: 1,
        });
    let report = discover(&mut f.reader(), &f.config.sources, &f.config.mappings, None).unwrap();
    assert_eq!(report.waiting.len(), 1);
    assert_eq!(report.waiting[0].dependencies.len(), 1);
    assert_eq!(
        report.waiting[0].dependencies[0].dependency.repository,
        "other/foundation"
    );
    let mut value = f.read("cross-dependency");
    value["state"] = json!("closed");
    f.write("cross-dependency", &value);
    assert_eq!(
        select(&mut f.reader(), &f.config.sources, &f.config.mappings)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn malformed_or_mismatched_dependency_evidence_never_authorizes_selection() {
    let f = Fixture::new();
    let reference = &f.config.sources[0].dependencies[0].blocked_by[0];
    for (field, value) in [
        ("number", json!(9)),
        (
            "repository_url",
            json!("https://api.github.com/repos/other/repo"),
        ),
        ("html_url", json!("https://github.com/other/repo/issues/1")),
        ("node_id", json!("")),
        ("state", json!("unknown")),
        ("pull_request", Value::Null),
    ] {
        f.set_state("closed", false, false);
        let mut dependency = f.read("dependency");
        dependency[field] = value;
        f.write("dependency", &dependency);
        let error = f.reader().dependency(reference).unwrap_err();
        assert_eq!(error.operation, ReadOperation::Dependency);
        assert_eq!(error.issue_id.as_deref(), Some("org/tracker#1"));
        assert!(
            select(&mut f.reader(), &f.config.sources, &f.config.mappings).is_err(),
            "{field}"
        );
    }
}

#[test]
fn closed_pr_requires_live_consistent_merge_evidence_even_when_opted_in() {
    let mut f = Fixture::new();
    f.config.sources[0].dependencies[0].allow_closed_unmerged_prs = true;
    for (field, value) in [
        ("merged", Value::Null),
        ("state", json!("open")),
        ("number", json!(8)),
        ("html_url", json!("https://github.com/other/repo/pull/1")),
    ] {
        f.set_state("closed", true, false);
        let mut pull = f.read("pull");
        pull[field] = value;
        f.write("pull", &pull);
        assert!(
            select(&mut f.reader(), &f.config.sources, &f.config.mappings).is_err(),
            "{field}"
        );
    }
}

#[test]
fn dependency_permission_error_is_visible_and_never_treated_as_satisfied() {
    let f = Fixture::new();
    f.write(
        "dependency",
        &json!({"message":"Forbidden", "status":"403"}),
    );
    let report = discover(&mut f.reader(), &f.config.sources, &f.config.mappings, None).unwrap();
    assert!(report.waiting.is_empty());
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].issue_number, 3);
    assert_eq!(report.candidate_read_failures.len(), 1);
    assert_eq!(report.candidate_read_failures[0].issue, "ISSUE2");
    assert_eq!(report.candidate_read_failures[0].http_status, Some(403));
    assert!(select(&mut f.reader(), &f.config.sources, &f.config.mappings).is_err());
}

#[test]
fn status_discover_and_targeted_daemon_preview_report_waiting_without_writes() {
    let f = Fixture::new();
    let discovery = f.run(&["discover"]);
    let lines: Vec<Value> = discovery
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let waiting = lines
        .iter()
        .find(|line| line["evidence"]["eligibility"] == "waiting")
        .unwrap();
    assert_eq!(waiting["candidate"]["issue_number"], 2);
    assert_eq!(
        waiting["evidence"]["dependencies"][0]["dependency"]["number"],
        1
    );
    let status: Value = serde_json::from_str(&f.run(&["status"])).unwrap();
    assert!(status["tasks"].as_array().unwrap().is_empty());
    assert_eq!(status["waiting"][0]["candidate"]["issue_number"], 2);
    let preview: Value = serde_json::from_str(&f.run(&[
        "daemon",
        "--config-revision",
        "test",
        "--repository",
        "org/tracker",
        "--issues",
        "2",
        "--once",
    ]))
    .unwrap();
    assert!(preview["candidates"].as_array().unwrap().is_empty());
    assert_eq!(preview["waiting"][0]["dependencies"][0]["state"], "open");
    let execution: Value = serde_json::from_str(&f.run(&[
        "daemon",
        "--config-revision",
        "test",
        "--repository",
        "org/tracker",
        "--issues",
        "2",
        "--once",
        "--execute",
    ]))
    .unwrap();
    assert!(execution["launched"].as_array().unwrap().is_empty());
    assert_eq!(execution["waiting"][0]["candidate"]["issue_number"], 2);
    f.assert_read_only();
}

#[test]
fn dependency_configuration_rejects_invalid_conflicting_and_cyclic_rules() {
    let f = Fixture::new();
    let base = serde_json::to_value(&f.config).unwrap();
    for blocked_by in [
        json!([]),
        json!([{"repository":"org/tracker","number":0}]),
        json!([{"repository":"../bad","number":1}]),
        json!([{"repository":"org/tracker","number":2}]),
        json!([{"repository":"org/tracker","number":1},{"repository":"org/tracker","number":1}]),
    ] {
        let mut config = base.clone();
        config["sources"][0]["dependencies"][0]["blocked_by"] = blocked_by;
        assert!(Config::from_json(&config.to_string()).is_err());
    }
    let mut config = base.clone();
    let rule = config["sources"][0]["dependencies"][0].clone();
    config["sources"][0]["dependencies"]
        .as_array_mut()
        .unwrap()
        .push(rule);
    assert!(Config::from_json(&config.to_string()).is_err());
    let mut config = base;
    config["sources"][0]["dependencies"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "issue":{"repository":"org/tracker","number":1},
            "blocked_by":[{"repository":"org/tracker","number":2}]
        }));
    assert!(
        Config::from_json(&config.to_string())
            .unwrap_err()
            .to_string()
            .contains("cyclic")
    );
}

#[test]
fn overlapping_source_cannot_omit_or_override_dependency_policy() {
    let f = Fixture::new();
    let mut config = f.config.clone();
    let mut overlap = config.sources[0].clone();
    overlap.project_id = "OTHER_PROJECT".into();
    overlap.dependencies.clear();
    config.sources.push(overlap);
    assert!(config.validate().is_err());
    config.sources[1].dependencies = config.sources[0].dependencies.clone();
    config.validate().unwrap();
    config.sources[1].dependencies[0].allow_closed_unmerged_prs = true;
    assert!(config.validate().is_err());
}

#[test]
fn existing_configuration_without_dependency_rules_remains_compatible() {
    let f = Fixture::new();
    let mut value = serde_json::to_value(&f.config).unwrap();
    value["sources"][0]
        .as_object_mut()
        .unwrap()
        .remove("dependencies");
    let config = Config::from_json(&value.to_string()).unwrap();
    assert!(config.sources[0].dependencies.is_empty());
    assert_eq!(
        select(&mut f.reader(), &config.sources, &config.mappings)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn two_ready_phases_are_not_selected_together_and_next_phase_unblocks_on_close() {
    let f = Fixture::new();
    let mut page = f.read("page");
    let mut first = page["data"]["node"]["items"]["nodes"][0].clone();
    first["id"] = json!("ITEM1");
    first["content"]["id"] = json!("ISSUE1");
    first["content"]["number"] = json!(1);
    page["data"]["node"]["items"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(first);
    f.write("page", &page);
    let report = discover(&mut f.reader(), &f.config.sources, &f.config.mappings, None).unwrap();
    let mut numbers: Vec<_> = report
        .candidates
        .iter()
        .map(|candidate| candidate.issue_number)
        .collect();
    numbers.sort();
    assert_eq!(numbers, [1, 3]);
    assert_eq!(report.waiting[0].candidate.issue_number, 2);
    f.set_state("closed", false, false);
    let mut numbers: Vec<_> = select(&mut f.reader(), &f.config.sources, &f.config.mappings)
        .unwrap()
        .iter()
        .map(|candidate| candidate.issue_number)
        .collect();
    numbers.sort();
    assert_eq!(numbers, [2, 3]);
}
