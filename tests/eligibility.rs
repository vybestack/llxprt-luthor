use luthor::{
    config::{Mapping, Marker, Source},
    eligibility::select,
    github::project::{
        Issue, Page, ProjectItem, ProjectReadError, ProjectReader, ReadCategory, ReadOperation,
    },
};

struct Fake {
    pages: Vec<Page<ProjectItem>>,
    issues: Vec<Issue>,
    calls: usize,
    fail_page: bool,
}
impl ProjectReader for Fake {
    fn page(
        &mut self,
        _project_id: &str,
        _cursor: Option<&str>,
    ) -> Result<Page<ProjectItem>, ProjectReadError> {
        if self.fail_page {
            return Err(ProjectReadError {
                operation: ReadOperation::ProjectPage,
                project_id: None,
                item_id: None,
                issue_id: None,
                category: ReadCategory::Transport,
                status: None,
                code: "transport-error".into(),
            });
        }
        let page = self
            .pages
            .get(self.calls % self.pages.len())
            .cloned()
            .ok_or_else(|| "unexpected-page".to_owned())?;
        self.calls += 1;
        Ok(page)
    }
    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        self.issues
            .iter()
            .find(|issue| issue.node_id == item.issue_node_id)
            .cloned()
            .ok_or_else(|| "missing-issue".to_owned().into())
    }
}
fn item(id: &str, issue: &str, fields: Vec<(String, String)>) -> ProjectItem {
    ProjectItem {
        item_id: id.into(),
        issue_node_id: issue.into(),
        repository: "org/tracker".into(),
        tracker_repo_id: "R1".into(),
        issue_number: 7,
        fields,
        unsupported_fields: Vec::new(),
    }
}
fn issue(state: &str, labels: Vec<&str>, assignees: Vec<&str>, milestone: Option<&str>) -> Issue {
    Issue {
        node_id: "N7".into(),
        repository: "org/tracker".into(),
        tracker_repo_id: "R1".into(),
        number: 7,
        url: "https://github.com/org/tracker/issues/7".into(),
        state: state.into(),
        assignees: assignees.into_iter().map(str::to_string).collect(),
        labels: labels.into_iter().map(str::to_string).collect(),
        milestone: milestone.map(str::to_string),
        milestone_id: milestone.map(|_| "MILESTONE1".to_string()),
        observed_at_unix_secs: 1_700_000_000,
    }
}
fn source(marker: Marker, milestone: Option<&str>) -> Source {
    source_in("P1", marker, milestone)
}
fn source_in(project_id: &str, marker: Marker, milestone: Option<&str>) -> Source {
    Source {
        project_id: project_id.into(),
        repositories: vec!["org/tracker".into()],
        ready_marker: marker,
        milestone: milestone.map(str::to_string),
    }
}
fn mapping() -> Mapping {
    Mapping {
        tracker_repository: "org/tracker".into(),
        code_repository: "org/code".into(),
        checkout: "/checkout".into(),
        base_branch: "main".into(),
        push_remote: "origin".into(),
        allowed_pr_head_repository: "org/fork".into(),
        allowed_pr_author: "alice".into(),
    }
}

#[test]
fn requires_membership_direct_open_unassigned_exact_label_and_milestone() {
    let mut fake = Fake {
        pages: vec![Page {
            items: vec![item("I1", "N7", vec![])],
            has_next_page: false,
            end_cursor: None,
        }],
        issues: vec![issue("open", vec!["luthor-ready"], vec![], Some("0.12.0"))],
        calls: 0,
        fail_page: false,
    };
    let candidates = select(
        &mut fake,
        &[source(
            Marker::Label {
                name: "luthor-ready".into(),
            },
            Some("0.12.0"),
        )],
        &[mapping()],
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].issue_url,
        "https://github.com/org/tracker/issues/7"
    );
    assert_eq!(candidates[0].milestone_id.as_deref(), Some("MILESTONE1"));
    assert_eq!(candidates[0].milestone_title.as_deref(), Some("0.12.0"));
    assert_eq!(candidates[0].tracker_repo_id, "R1");
    assert_eq!(candidates[0].observed_at_unix_secs, 1_700_000_000);
    assert_eq!(candidates[0].observed_state, "open");
    assert_eq!(candidates[0].observed_assignees, Vec::<String>::new());
    assert_eq!(candidates[0].observed_labels, vec!["luthor-ready"]);
    assert!(candidates[0].observed_project_fields.is_empty());
    assert_eq!(
        candidates[0].marker,
        Marker::Label {
            name: "luthor-ready".into()
        }
    );
    let mut wrong = Fake {
        issues: vec![issue("open", vec!["OK for Luther"], vec![], Some("0.12.0"))],
        calls: 0,
        ..fake
    };
    assert!(
        select(
            &mut wrong,
            &[source(
                Marker::Label {
                    name: "luthor-ready".into()
                },
                Some("0.12.0")
            )],
            &[mapping()]
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn same_issue_number_in_two_repositories_keeps_distinct_stable_ids() {
    let mut first_item = item("I1", "N7-A", vec![]);
    first_item.issue_number = 7;
    let mut second_item = item("I2", "N7-B", vec![]);
    second_item.repository = "org/other".into();
    second_item.tracker_repo_id = "R2".into();
    second_item.issue_number = 7;
    let mut first_issue = issue("open", vec!["ready"], vec![], None);
    first_issue.node_id = "N7-A".into();
    let mut second_issue = first_issue.clone();
    second_issue.node_id = "N7-B".into();
    second_issue.repository = "org/other".into();
    second_issue.tracker_repo_id = "R2".into();
    second_issue.url = "https://github.com/org/other/issues/7".into();
    let mut fake = Fake {
        pages: vec![Page {
            items: vec![first_item, second_item],
            has_next_page: false,
            end_cursor: None,
        }],
        issues: vec![first_issue, second_issue],
        calls: 0,
        fail_page: false,
    };
    let mut both_repos = source(
        Marker::Label {
            name: "ready".into(),
        },
        None,
    );
    both_repos.repositories.push("org/other".into());
    let mut second_mapping = mapping();
    second_mapping.tracker_repository = "org/other".into();
    let candidates = select(&mut fake, &[both_repos], &[mapping(), second_mapping]).unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].issue_number, candidates[1].issue_number);
    assert_ne!(candidates[0].issue_node_id, candidates[1].issue_node_id);
    assert_ne!(candidates[0].repository, candidates[1].repository);
}

#[test]
fn direct_issue_url_must_match_project_item() {
    let mut fake = Fake {
        pages: vec![Page {
            items: vec![item("I1", "N7", vec![])],
            has_next_page: false,
            end_cursor: None,
        }],
        issues: vec![issue("open", vec!["ready"], vec![], None)],
        calls: 0,
        fail_page: false,
    };
    fake.issues[0].url = "https://github.com/org/tracker/issues/8".into();
    assert!(
        select(
            &mut fake,
            &[source(
                Marker::Label {
                    name: "ready".into()
                },
                None
            )],
            &[mapping()],
        )
        .is_err()
    );
    fake.issues[0].url = "https://github.com/org/tracker/issues/7".into();
    fake.issues[0].tracker_repo_id = "different-repository-id".into();
    assert!(
        select(
            &mut fake,
            &[source(
                Marker::Label {
                    name: "ready".into()
                },
                None
            )],
            &[mapping()],
        )
        .is_err()
    );
}

#[test]
fn optional_milestone_and_project_field_marker_are_exact() {
    let mut fake = Fake {
        pages: vec![Page {
            items: vec![item("I1", "N7", vec![("Status".into(), "Ready".into())])],
            has_next_page: false,
            end_cursor: None,
        }],
        issues: vec![issue("open", vec![], vec![], None)],
        calls: 0,
        fail_page: false,
    };
    let candidates = select(
        &mut fake,
        &[source(
            Marker::ProjectField {
                name: "Status".into(),
                value: "Ready".into(),
            },
            None,
        )],
        &[mapping()],
    )
    .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].observed_project_fields,
        vec![("Status".into(), "Ready".into())]
    );
}

#[test]
fn closed_or_milestone_mismatched_issue_is_stale_and_not_eligible() {
    let mut fake = Fake {
        pages: vec![Page {
            items: vec![item("I1", "N7", vec![])],
            has_next_page: false,
            end_cursor: None,
        }],
        issues: vec![issue("closed", vec!["ready"], vec![], Some("old"))],
        calls: 0,
        fail_page: false,
    };
    let selected = select(
        &mut fake,
        &[source(
            Marker::Label {
                name: "ready".into(),
            },
            Some("current"),
        )],
        &[mapping()],
    )
    .unwrap();
    assert!(selected.is_empty());
}

#[test]
fn issue_from_another_project_is_not_eligible() {
    let mut fake = Fake {
        pages: vec![Page {
            items: vec![],
            has_next_page: false,
            end_cursor: None,
        }],
        issues: vec![issue("open", vec!["ready"], vec![], None)],
        calls: 0,
        fail_page: false,
    };
    let selected = select(
        &mut fake,
        &[source_in(
            "P2",
            Marker::Label {
                name: "ready".into(),
            },
            None,
        )],
        &[mapping()],
    )
    .unwrap();
    assert!(selected.is_empty());
}

#[test]
fn compatible_overlapping_sources_deduplicate_issue() {
    let mut fake = Fake {
        pages: vec![Page {
            items: vec![item("I1", "N7", vec![])],
            has_next_page: false,
            end_cursor: None,
        }],
        issues: vec![issue("open", vec!["ready"], vec![], None)],
        calls: 0,
        fail_page: false,
    };
    let sources = [
        source_in(
            "P1",
            Marker::Label {
                name: "ready".into(),
            },
            None,
        ),
        source_in(
            "P2",
            Marker::Label {
                name: "ready".into(),
            },
            None,
        ),
    ];
    let selected = select(&mut fake, &sources, &[mapping()]).unwrap();
    assert_eq!(selected.len(), 1);
}

#[test]
fn incompatible_overlapping_source_rules_are_rejected() {
    let mut fake = Fake {
        pages: vec![Page {
            items: vec![item("I1", "N7", vec![])],
            has_next_page: false,
            end_cursor: None,
        }],
        issues: vec![issue("open", vec!["ready", "other"], vec![], None)],
        calls: 0,
        fail_page: false,
    };
    let sources = [
        source_in(
            "P1",
            Marker::Label {
                name: "ready".into(),
            },
            None,
        ),
        source_in(
            "P2",
            Marker::Label {
                name: "other".into(),
            },
            None,
        ),
    ];
    assert!(select(&mut fake, &sources, &[mapping()]).is_err());
}

#[test]
fn pagination_failure_is_not_an_empty_candidate_set() {
    let mut fake = Fake {
        pages: vec![],
        issues: vec![],
        calls: 0,
        fail_page: true,
    };
    assert!(
        select(
            &mut fake,
            &[source(
                Marker::Label {
                    name: "ready".into()
                },
                None
            )],
            &[mapping()]
        )
        .is_err()
    );
}

#[test]
fn non_issue_item_aborts_discovery_even_when_an_issue_is_eligible() {
    let mut fake = Fake {
        pages: vec![Page {
            items: vec![item("I1", "N7", vec![]), item("I2", "", vec![])],
            has_next_page: false,
            end_cursor: None,
        }],
        issues: vec![issue("open", vec!["ready"], vec![], None)],
        calls: 0,
        fail_page: false,
    };
    let result = select(
        &mut fake,
        &[source(
            Marker::Label {
                name: "ready".into(),
            },
            None,
        )],
        &[mapping()],
    );
    assert!(matches!(
        result,
        Err(luthor::eligibility::EligibilityError::Project(_))
    ));
}

#[test]
fn unsupported_configured_marker_field_errors_but_unrelated_field_does_not() {
    let mut unsupported = item("I1", "N7", vec![]);
    unsupported.unsupported_fields.push("Sprint".into());
    let mut fake = Fake {
        pages: vec![Page {
            items: vec![unsupported],
            has_next_page: false,
            end_cursor: None,
        }],
        issues: vec![issue("open", vec![], vec![], None)],
        calls: 0,
        fail_page: false,
    };
    let error = select(
        &mut fake,
        &[source(
            Marker::ProjectField {
                name: "Sprint".into(),
                value: "Ready".into(),
            },
            None,
        )],
        &[mapping()],
    )
    .unwrap_err();
    assert_eq!(
        error,
        luthor::eligibility::EligibilityError::UnsupportedMarkerField {
            project_id: "P1".into(),
            item_id: "I1".into(),
            name: "Sprint".into(),
        }
    );

    let mut unrelated = item("I1", "N7", vec![("Status".into(), "Ready".into())]);
    unrelated.unsupported_fields.push("Sprint".into());
    fake.pages[0].items = vec![unrelated];
    let selected = select(
        &mut fake,
        &[source(
            Marker::ProjectField {
                name: "Status".into(),
                value: "Ready".into(),
            },
            None,
        )],
        &[mapping()],
    )
    .unwrap();
    assert_eq!(selected.len(), 1);
}
