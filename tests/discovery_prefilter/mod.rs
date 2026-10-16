//! Issue 95: project content excludes idle history before direct issue reads.
use super::*;
use luthor::{
    config::{DependencyRule, IssueReference},
    eligibility::{discover, select_target},
};

fn history(count: u64, with_candidate: bool) -> Fixture {
    let mut nodes: Vec<_> = (1..=count)
        .map(|number| {
            let mut item = node("org/tracker", number);
            item["content"]["state"] = json!("CLOSED");
            item
        })
        .collect();
    if with_candidate {
        nodes.push(node("org/tracker", 1000));
    }
    let pages = if nodes.is_empty() {
        vec![page(vec![], None)]
    } else {
        let chunks: Vec<_> = nodes.chunks(100).collect();
        chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| {
                page(
                    chunk.to_vec(),
                    if index + 1 == chunks.len() {
                        None
                    } else if index == 0 {
                        Some("PAGE2")
                    } else {
                        Some("PAGE3")
                    },
                )
            })
            .collect()
    };
    Fixture {
        dir: tempdir().unwrap(),
        pages,
        issues: if with_candidate {
            vec![issue("org/tracker", 1000)]
        } else {
            vec![]
        },
    }
}

#[test]
fn closed_ready_history_produces_zero_per_issue_rest_reads_across_cycles() {
    for count in [0, 1, 90, 250] {
        let fixture = history(count, false);
        let mut reader = fixture.reader();
        for _ in 0..2 {
            let report = discover(
                &mut reader,
                &[source("org/tracker")],
                &[mapping("org/tracker")],
                None,
            )
            .unwrap();
            assert!(report.candidates.is_empty());
            assert!(report.waiting.is_empty());
            assert!(report.candidate_read_failures.is_empty());
            assert!(report.source_read_failures.is_empty());
        }
        let reads = fixture.reads();
        assert_eq!(reads.len(), fixture.pages.len() * 2, "history={count}");
        assert!(reads.iter().all(|read| read == "api graphql"));
    }
}

#[test]
fn open_ready_unassigned_candidate_is_rest_confirmed_each_cycle_despite_closed_history() {
    let fixture = history(250, true);
    let mut reader = fixture.reader();
    for _ in 0..2 {
        let report = discover(
            &mut reader,
            &[source("org/tracker")],
            &[mapping("org/tracker")],
            None,
        )
        .unwrap();
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].issue_number, 1000);
        assert_eq!(report.candidates[0].observed_state, "open");
    }
    assert_eq!(
        fixture
            .reads()
            .iter()
            .filter(|r| r.contains("/issues/"))
            .collect::<Vec<_>>(),
        vec![
            "api repos/org/tracker/issues/1000?per_page=100",
            "api repos/org/tracker/issues/1000?per_page=100"
        ]
    );
    // REST changing after the project snapshot must still prevent selection.
    let mut changed = fixture.issues[0].clone();
    changed["assignees"] = json!([{"login":"another-worker"}]);
    fs::write(fixture.dir.path().join("issue0.json"), changed.to_string()).unwrap();
    let report = discover(
        &mut reader,
        &[source("org/tracker")],
        &[mapping("org/tracker")],
        None,
    )
    .unwrap();
    assert!(report.candidates.is_empty());
    assert_eq!(
        fixture
            .reads()
            .iter()
            .filter(|r| r.contains("/issues/"))
            .count(),
        3
    );
}

#[test]
fn ineligible_graphql_content_is_filtered_for_all_selection_entry_points() {
    for (field, value) in [
        ("state", json!("CLOSED")),
        (
            "labels",
            json!({"nodes":[{"name":"Ready"}],"pageInfo":{"hasNextPage":false}}),
        ),
        (
            "assignees",
            json!({"nodes":[{"login":"acoliver"}],"pageInfo":{"hasNextPage":false}}),
        ),
        ("milestone", json!({"title":"other"})),
    ] {
        let mut fixture = history(0, true);
        fixture.pages[0]["data"]["node"]["items"]["nodes"][0]["content"][field] = value;
        let mut reader = fixture.reader();
        let sources = [source("org/tracker")];
        let mappings = [mapping("org/tracker")];
        assert!(select(&mut reader, &sources, &mappings).unwrap().is_empty());
        assert!(
            select_target(&mut reader, &sources, &mappings, "org/tracker", 1000)
                .unwrap()
                .is_empty()
        );
        for target in [None, Some(("org/tracker", 1000))] {
            let report = discover(&mut reader, &sources, &mappings, target).unwrap();
            assert!(report.candidates.is_empty(), "{field}");
            assert!(report.candidate_read_failures.is_empty(), "{field}");
        }
        assert_eq!(fixture.reads(), vec!["api graphql"; 4], "{field}");
    }
}

#[test]
fn project_field_marker_is_filtered_before_rest_and_does_not_require_a_label() {
    let mut fixture = history(0, true);
    fixture.pages[0]["data"]["node"]["items"]["nodes"][0]["content"]["labels"]["nodes"] = json!([]);
    let mut configured = source("org/tracker");
    configured.ready_marker = Marker::ProjectField {
        name: "Status".into(),
        value: "Ready".into(),
    };
    let report = discover(
        &mut fixture.reader(),
        &[configured.clone()],
        &[mapping("org/tracker")],
        None,
    )
    .unwrap();
    assert!(report.candidates.is_empty());
    assert_eq!(fixture.reads(), vec!["api graphql"]);
    fixture.pages[0]["data"]["node"]["items"]["nodes"][0]["fieldValues"]["nodes"] = json!([
        {"__typename":"ProjectV2ItemFieldSingleSelectValue","name":"Ready","field":{"name":"Status"}}
    ]);
    let report = discover(
        &mut fixture.reader(),
        &[configured],
        &[mapping("org/tracker")],
        None,
    )
    .unwrap();
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(
        fixture
            .reads()
            .iter()
            .filter(|r| r.contains("/issues/"))
            .count(),
        1
    );
}

#[test]
fn closed_dependency_is_read_only_for_current_candidate_not_unrelated_closed_history() {
    let mut fixture = history(250, true);
    let mut dependency = issue("org/tracker", 1);
    dependency["state"] = json!("closed");
    fixture.issues.push(dependency);
    let reference = |number| IssueReference {
        repository: "org/tracker".into(),
        number,
    };
    let mut configured = source("org/tracker");
    configured.dependencies = vec![
        DependencyRule {
            issue: reference(1000),
            blocked_by: vec![reference(1)],
            allow_closed_unmerged_prs: false,
        },
        // This closed former candidate must not trigger its own prerequisite read.
        DependencyRule {
            issue: reference(2),
            blocked_by: vec![reference(999)],
            allow_closed_unmerged_prs: false,
        },
    ];
    let mut reader = fixture.reader();
    for _ in 0..2 {
        let report = discover(
            &mut reader,
            &[configured.clone()],
            &[mapping("org/tracker")],
            None,
        )
        .unwrap();
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].issue_number, 1000);
        assert!(report.waiting.is_empty());
        assert!(report.candidate_read_failures.is_empty());
    }
    let reads = fixture.reads();
    assert_eq!(reads.iter().filter(|r| r.contains("/issues/")).count(), 4);
    assert_eq!(
        reads
            .iter()
            .filter(|r| r.as_str() == "api repos/org/tracker/issues/1")
            .count(),
        2
    );
    assert!(!reads.iter().any(|r| r.ends_with("issues/999")));
}

#[test]
fn malformed_or_incomplete_project_summary_fails_without_rest_fallback() {
    let cases = [
        ("state", Value::Null),
        ("state", json!("open")),
        ("state", json!("UNKNOWN")),
        ("milestone", json!({"title":null})),
        ("labels", Value::Null),
        (
            "labels",
            json!({"nodes":[],"pageInfo":{"hasNextPage":true}}),
        ),
        (
            "labels",
            json!({"nodes":[{"name":null}],"pageInfo":{"hasNextPage":false}}),
        ),
        ("assignees", json!({"nodes":[],"pageInfo":{}})),
        (
            "assignees",
            json!({"nodes":[{}],"pageInfo":{"hasNextPage":false}}),
        ),
    ];
    for (field, value) in cases {
        let mut fixture = history(0, true);
        fixture.pages[0]["data"]["node"]["items"]["nodes"][0]["content"][field] = value;
        let error = discover(
            &mut fixture.reader(),
            &[source("org/tracker")],
            &[mapping("org/tracker")],
            None,
        )
        .unwrap_err();
        assert!(
            matches!(error, EligibilityError::Project(ProjectError::Read(_))),
            "{field}"
        );
        assert_eq!(fixture.reads(), vec!["api graphql"], "{field}");
    }
    for field in ["state", "labels", "assignees", "milestone"] {
        let mut fixture = history(1, false);
        fixture.pages[0]["data"]["node"]["items"]["nodes"][0]["content"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            discover(
                &mut fixture.reader(),
                &[source("org/tracker")],
                &[mapping("org/tracker")],
                None
            )
            .is_err(),
            "{field}"
        );
        assert_eq!(fixture.reads(), vec!["api graphql"], "{field}");
    }
}
