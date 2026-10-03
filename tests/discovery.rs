//! Unrestricted discovery must not depend on unrelated repositories' issue APIs.
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use luthor::{
    config::{Mapping, Marker, Source},
    eligibility::{EligibilityError, select},
    github::project::{GhProjectReader, ProjectError},
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

struct Fixture {
    dir: TempDir,
    pages: Vec<Value>,
    issues: Vec<Value>,
}

fn node(repository: &str, number: u64) -> Value {
    json!({
        "id": format!("ITEM_{repository}_{number}"),
        "content": {
            "__typename": "Issue", "id": format!("ISSUE_{repository}_{number}"),
            "number": number,
            "repository": {"id": format!("REPO_{repository}"), "nameWithOwner": repository}
        },
        "fieldValues": {"nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": null}}
    })
}

fn page(nodes: Vec<Value>, cursor: Option<&str>) -> Value {
    json!({"data": {"node": {"items": {
        "nodes": nodes,
        "pageInfo": {"hasNextPage": cursor.is_some(), "endCursor": cursor}
    }}}})
}

fn issue(repository: &str, number: u64) -> Value {
    json!({
        "node_id": format!("ISSUE_{repository}_{number}"), "number": number,
        "html_url": format!("https://github.com/{repository}/issues/{number}"),
        "repository_url": format!("https://api.github.com/repos/{repository}"),
        "state": "open", "labels": [{"name": "ready"}], "assignees": [],
        "milestone": {"title": "planned", "node_id": "M1"}
    })
}

fn source(repository: &str) -> Source {
    Source {
        project_id: "PROJECT".into(),
        repositories: vec![repository.into()],
        ready_marker: Marker::Label {
            name: "ready".into(),
        },
        milestone: Some("planned".into()),
    }
}

fn mapping(repository: &str) -> Mapping {
    Mapping {
        tracker_repository: repository.into(),
        code_repository: repository.into(),
        checkout: "/checkout".into(),
        base_branch: "main".into(),
        push_remote: "origin".into(),
        allowed_pr_head_repository: repository.into(),
        allowed_pr_author: "acoliver".into(),
    }
}

impl Fixture {
    fn shared_project() -> Self {
        let pages = (0..3)
            .map(|index| {
                let mut nodes: Vec<_> = (1..=40)
                    .map(|n| node("unrelated/repository", index * 40 + n))
                    .collect();
                nodes.push(node("org/tracker", index + 1));
                page(
                    nodes,
                    match index {
                        0 => Some("PAGE2"),
                        1 => Some("PAGE3"),
                        _ => None,
                    },
                )
            })
            .collect();
        Self {
            dir: tempdir().unwrap(),
            pages,
            issues: (1..=3).map(|number| issue("org/tracker", number)).collect(),
        }
    }

    fn reader(&self) -> GhProjectReader {
        let root = self.dir.path();
        for (index, page) in self.pages.iter().enumerate() {
            fs::write(root.join(format!("page{index}.json")), page.to_string()).unwrap();
        }
        for (index, issue) in self.issues.iter().enumerate() {
            fs::write(root.join(format!("issue{index}.json")), issue.to_string()).unwrap();
        }
        let mut script = String::from(
            "#!/bin/sh\ncd -- \"$(dirname -- \"$0\")\" || exit 1\nprintf '%s\\n' \"$1 $2\" >> reads\ncase \"$1 $2\" in\n'api graphql')\ncase \"$*\" in\n*cursor=PAGE3*) cat page2.json;;\n*cursor=PAGE2*) cat page1.json;;\n*) cat page0.json;;\nesac;;\n",
        );
        for repository in ["org/tracker", "org/other"] {
            script.push_str(&format!(
                "'api repos/{repository}') printf '%s' '{{\"node_id\":\"REPO_{repository}\"}}';;\n"
            ));
        }
        for (index, issue) in self.issues.iter().enumerate() {
            let repository = issue["repository_url"]
                .as_str()
                .unwrap()
                .strip_prefix("https://api.github.com/repos/")
                .unwrap();
            let number = issue["number"].as_u64().unwrap();
            script.push_str(&format!(
                "'api repos/{repository}/issues/{number}?per_page=100') cat issue{index}.json;;\n"
            ));
        }
        // All unrelated direct reads would fail, rather than return absent issues.
        script.push_str(
            "*) printf '%s' 'HTTP 403 forbidden unrelated endpoint' >&2; exit 1;;\nesac\n",
        );
        let executable: PathBuf = root.join("gh");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        GhProjectReader::new(executable)
    }

    fn reads(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("reads"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn shared_project_reads_only_configured_endpoints_and_refreshes_every_candidate() {
    let fixture = Fixture::shared_project();
    let mut reader = fixture.reader();
    for _ in 0..2 {
        let candidates = select(
            &mut reader,
            &[source("org/tracker")],
            &[mapping("org/tracker")],
        )
        .unwrap();
        assert_eq!(candidates.len(), 3);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.observed_state == "open")
        );
    }
    let reads = fixture.reads();
    assert_eq!(
        reads.iter().filter(|read| *read == "api graphql").count(),
        6
    );
    assert_eq!(
        reads
            .iter()
            .filter(|read| *read == "api repos/org/tracker")
            .count(),
        1
    );
    for number in 1..=3 {
        let endpoint = format!("api repos/org/tracker/issues/{number}?per_page=100");
        assert_eq!(reads.iter().filter(|read| **read == endpoint).count(), 2);
    }
    assert_eq!(
        reads.len(),
        13,
        "six page reads, six fresh issue reads, one repository read"
    );
    assert!(reads.iter().all(|read| !read.contains("unrelated/")));
}

#[test]
fn repositories_from_every_configured_source_are_read_without_eligibility_bypass() {
    let mut fixture = Fixture::shared_project();
    fixture.pages[2]["data"]["node"]["items"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(node("org/other", 4));
    fixture.issues.push(issue("org/other", 4));
    let mut other_source = source("org/other");
    other_source.ready_marker = Marker::Label {
        name: "different-marker".into(),
    };
    let candidates = select(
        &mut fixture.reader(),
        &[source("org/tracker"), other_source],
        &[mapping("org/tracker"), mapping("org/other")],
    )
    .unwrap();
    assert_eq!(candidates.len(), 3);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.repository == "org/tracker")
    );
    let reads = fixture.reads();
    assert_eq!(
        reads
            .iter()
            .filter(|read| read.contains("/issues/"))
            .count(),
        8
    );
    assert_eq!(
        reads
            .iter()
            .filter(|read| *read == "api repos/org/other/issues/4?per_page=100")
            .count(),
        2
    );
    assert!(reads.iter().all(|read| !read.contains("unrelated/")));
}

#[test]
fn configured_issues_still_require_readiness_state_assignees_and_milestone() {
    for (field, value) in [
        ("labels", json!([])),
        ("state", json!("closed")),
        ("assignees", json!([{"login": "someone-else"}])),
        ("milestone", json!({"title": "other", "node_id": "M2"})),
    ] {
        let mut fixture = Fixture::shared_project();
        fixture.issues[0][field] = value;
        let candidates = select(
            &mut fixture.reader(),
            &[source("org/tracker")],
            &[mapping("org/tracker")],
        )
        .unwrap();
        assert_eq!(candidates.len(), 2, "{field}");
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.issue_number != 1)
        );
        assert_eq!(
            fixture
                .reads()
                .iter()
                .filter(|read| read.contains("/issues/"))
                .count(),
            3
        );
    }
}

#[test]
fn unreadable_or_malformed_configured_issue_fails_closed() {
    for field in [
        "node_id",
        "html_url",
        "state",
        "assignees",
        "labels",
        "milestone",
    ] {
        let fixture = Fixture::shared_project();
        let mut reader = fixture.reader();
        let path = fixture.dir.path().join("issue0.json");
        let mut issue = fixture.issues[0].clone();
        issue.as_object_mut().unwrap().remove(field);
        fs::write(path, issue.to_string()).unwrap();
        assert!(
            select(
                &mut reader,
                &[source("org/tracker")],
                &[mapping("org/tracker")]
            )
            .is_err(),
            "{field}"
        );
    }
    let fixture = Fixture::shared_project();
    let mut reader = fixture.reader();
    fs::write(fixture.dir.path().join("issue0.json"), "not-json").unwrap();
    assert!(matches!(
        select(
            &mut reader,
            &[source("org/tracker")],
            &[mapping("org/tracker")]
        ),
        Err(EligibilityError::Project(ProjectError::IssueRead(_)))
    ));
}

#[test]
fn configured_issue_transport_failure_is_not_an_empty_candidate_set() {
    let fixture = Fixture::shared_project();
    let mut reader = fixture.reader();
    let executable = fixture.dir.path().join("gh");
    let script = fs::read_to_string(&executable).unwrap().replace(
        "cat issue0.json;;",
        "printf '%s' 'HTTP 403 configured endpoint forbidden' >&2; exit 1;;",
    );
    fs::write(executable, script).unwrap();
    assert!(matches!(
        select(
            &mut reader,
            &[source("org/tracker")],
            &[mapping("org/tracker")]
        ),
        Err(EligibilityError::Project(ProjectError::IssueRead(_)))
    ));
    assert_eq!(
        fixture
            .reads()
            .iter()
            .filter(|read| read.contains("/issues/"))
            .count(),
        1
    );
}

#[test]
fn configured_issue_identity_mismatch_fails_closed() {
    for (field, value) in [
        ("node_id", json!("WRONG")),
        ("number", json!(99)),
        (
            "repository_url",
            json!("https://api.github.com/repos/wrong/repository"),
        ),
        (
            "html_url",
            json!("https://github.com/org/tracker/issues/99"),
        ),
    ] {
        let fixture = Fixture::shared_project();
        let mut reader = fixture.reader();
        let mut issue = fixture.issues[0].clone();
        issue[field] = value;
        fs::write(fixture.dir.path().join("issue0.json"), issue.to_string()).unwrap();
        assert!(
            select(
                &mut reader,
                &[source("org/tracker")],
                &[mapping("org/tracker")]
            )
            .is_err(),
            "{field}"
        );
    }
    let mut fixture = Fixture::shared_project();
    fixture.pages[0]["data"]["node"]["items"]["nodes"][40]["content"]["repository"]["id"] =
        json!("WRONG");
    assert!(
        select(
            &mut fixture.reader(),
            &[source("org/tracker")],
            &[mapping("org/tracker")]
        )
        .is_err()
    );
}

#[test]
fn duplicate_project_and_issue_identities_are_checked_even_for_skipped_items() {
    for repository in ["org/tracker", "unrelated/repository"] {
        for duplicate_item in [true, false] {
            let mut fixture = Fixture::shared_project();
            let mut duplicate = node(repository, 1);
            if !duplicate_item {
                duplicate["id"] = json!("ANOTHER_ITEM");
            }
            fixture.pages[2]["data"]["node"]["items"]["nodes"]
                .as_array_mut()
                .unwrap()
                .push(duplicate);
            assert!(matches!(
                select(
                    &mut fixture.reader(),
                    &[source("org/tracker")],
                    &[mapping("org/tracker")]
                ),
                Err(EligibilityError::Project(ProjectError::Duplicate(_)))
            ));
        }
    }
}

#[test]
fn pagination_and_malformed_relevant_project_evidence_fail_closed() {
    for cursor in [Some("PAGE2"), None] {
        let mut fixture = Fixture::shared_project();
        fixture.pages[1]["data"]["node"]["items"]["pageInfo"]["endCursor"] = json!(cursor);
        let error = select(
            &mut fixture.reader(),
            &[source("org/tracker")],
            &[mapping("org/tracker")],
        )
        .unwrap_err();
        if cursor.is_some() {
            assert!(matches!(
                error,
                EligibilityError::Project(ProjectError::RepeatedCursor)
            ));
        } else {
            assert!(matches!(
                error,
                EligibilityError::Project(ProjectError::Read(_))
                    | EligibilityError::Project(ProjectError::MissingCursor)
            ));
        }
    }
    for field in ["id", "number", "repository"] {
        let mut fixture = Fixture::shared_project();
        fixture.pages[2]["data"]["node"]["items"]["nodes"][40]["content"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            select(
                &mut fixture.reader(),
                &[source("org/tracker")],
                &[mapping("org/tracker")]
            )
            .is_err(),
            "{field}"
        );
    }
    let fixture = Fixture::shared_project();
    let mut reader = fixture.reader();
    fs::write(fixture.dir.path().join("page2.json"), "not-json").unwrap();
    assert!(matches!(
        select(
            &mut reader,
            &[source("org/tracker")],
            &[mapping("org/tracker")]
        ),
        Err(EligibilityError::Project(ProjectError::Read(_)))
    ));
}
