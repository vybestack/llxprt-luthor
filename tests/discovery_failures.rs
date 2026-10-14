//! In-memory reader failures exercise source isolation without network or CLI dependencies.
use luthor::{
    config::{Mapping, Marker, Source},
    eligibility,
    github::project::{
        Issue, Page, ProjectItem, ProjectReadError, ProjectReader, ReadCategory, ReadOperation,
    },
};

struct Reader {
    fail_project: bool,
    fail_issue: bool,
    malformed: bool,
}

fn read_error(operation: ReadOperation, malformed: bool) -> ProjectReadError {
    ProjectReadError {
        operation,
        project_id: None,
        item_id: None,
        issue_id: None,
        category: if malformed {
            ReadCategory::Malformed
        } else {
            ReadCategory::Unknown
        },
        status: if malformed { None } else { Some(500) },
        code: if malformed {
            "invalid-json"
        } else {
            "command-failed"
        }
        .into(),
    }
}

impl ProjectReader for Reader {
    fn page(
        &mut self,
        project: &str,
        _cursor: Option<&str>,
    ) -> Result<Page<ProjectItem>, ProjectReadError> {
        if project == "BAD" && self.fail_project {
            return Err(read_error(ReadOperation::ProjectPage, self.malformed));
        }
        let number = if project == "BAD" { 1 } else { 2 };
        Ok(Page {
            items: vec![ProjectItem {
                item_id: format!("ITEM{number}"),
                issue_node_id: format!("ISSUE{number}"),
                repository: "org/repo".into(),
                tracker_repo_id: "REPO".into(),
                issue_number: number,
                fields: vec![],
                unsupported_fields: vec![],
            }],
            has_next_page: false,
            end_cursor: None,
        })
    }
    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        if item.issue_number == 1 && self.fail_issue {
            return Err(read_error(ReadOperation::DirectIssue, self.malformed));
        }
        Ok(Issue {
            node_id: item.issue_node_id.clone(),
            repository: item.repository.clone(),
            tracker_repo_id: item.tracker_repo_id.clone(),
            number: item.issue_number,
            url: format!("https://github.com/org/repo/issues/{}", item.issue_number),
            state: "open".into(),
            closed_at: None,
            assignees: vec![],
            labels: vec!["ready".into()],
            milestone: None,
            milestone_id: None,
            observed_at_unix_secs: 1,
        })
    }
}

fn sources() -> Vec<Source> {
    ["BAD", "GOOD"]
        .map(|project| Source {
            project_id: project.into(),
            repositories: vec!["org/repo".into()],
            ready_marker: Marker::Label {
                name: "ready".into(),
            },
            milestone: None,
        })
        .into()
}

fn mappings() -> Vec<Mapping> {
    vec![Mapping {
        tracker_repository: "org/repo".into(),
        code_repository: "org/repo".into(),
        checkout: "/unused".into(),
        base_branch: "main".into(),
        push_remote: "origin".into(),
        allowed_pr_head_repository: "org/repo".into(),
        allowed_pr_author: "acoliver".into(),
        required_checks: Default::default(),
    }]
}

#[test]
fn exhausted_project_read_skips_only_its_source_even_with_shared_repository() {
    let mut reader = Reader {
        fail_project: true,
        fail_issue: false,
        malformed: false,
    };
    let report = eligibility::discover(&mut reader, &sources(), &mappings(), None).unwrap();
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].project_id, "GOOD");
    assert_eq!(report.candidates[0].issue_number, 2);
    assert_eq!(report.source_read_failures[0].project_id, "BAD");
    assert_eq!(report.source_read_failures[0].http_status, Some(500));
}

#[test]
fn exhausted_candidate_read_skips_only_unverified_issue() {
    let mut reader = Reader {
        fail_project: false,
        fail_issue: true,
        malformed: false,
    };
    let report = eligibility::discover(&mut reader, &sources(), &mappings(), None).unwrap();
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].issue_number, 2);
    assert_eq!(report.candidate_read_failures[0].item, "ITEM1");
    assert_eq!(report.candidate_read_failures[0].issue, "ISSUE1");
    assert_eq!(report.candidate_read_failures[0].http_status, Some(500));
}

#[test]
fn malformed_reader_evidence_is_not_external_unavailability() {
    for fail_project in [true, false] {
        let mut reader = Reader {
            fail_project,
            fail_issue: !fail_project,
            malformed: true,
        };
        assert!(eligibility::discover(&mut reader, &sources(), &mappings(), None).is_err());
    }
}
