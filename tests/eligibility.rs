use luthor::{
    config::{Mapping, Marker, Source},
    eligibility::select,
    github::project::{Issue, Page, ProjectItem, ProjectReader},
};

struct Fake {
    pages: Vec<Page<ProjectItem>>,
    issues: Vec<Issue>,
    calls: usize,
    fail_page: bool,
}
impl ProjectReader for Fake {
    fn page(&mut self, _cursor: Option<&str>) -> Result<Page<ProjectItem>, String> {
        if self.fail_page {
            return Err("network unavailable".into());
        }
        let page = self
            .pages
            .get(self.calls)
            .cloned()
            .ok_or("unexpected page")?;
        self.calls += 1;
        Ok(page)
    }
    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, String> {
        self.issues
            .iter()
            .find(|issue| issue.node_id == item.issue_node_id)
            .cloned()
            .ok_or("missing issue".into())
    }
}
fn item(id: &str, issue: &str, fields: Vec<(String, String)>) -> ProjectItem {
    ProjectItem {
        item_id: id.into(),
        issue_node_id: issue.into(),
        repository: "org/tracker".into(),
        issue_number: 7,
        fields,
    }
}
fn issue(state: &str, labels: Vec<&str>, assignees: Vec<&str>, milestone: Option<&str>) -> Issue {
    Issue {
        node_id: "N7".into(),
        repository: "org/tracker".into(),
        number: 7,
        state: state.into(),
        assignees: assignees.into_iter().map(str::to_string).collect(),
        labels: labels.into_iter().map(str::to_string).collect(),
        milestone: milestone.map(str::to_string),
    }
}
fn source(marker: Marker, milestone: Option<&str>) -> Source {
    Source {
        project_id: "P1".into(),
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
