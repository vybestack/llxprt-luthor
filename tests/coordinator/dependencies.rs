use super::{FakeGithub, FakeLauncher, FakePr, FakeWriter, FixedIds, Fixture};
use luthor::{
    config::{DependencyRule, IssueReference},
    coordinator::{DispatchDependencies, ScheduleDependencies, dispatch_one, schedule_candidates},
    dependencies::DependencyState,
    github::project::{Issue, Page, ProjectItem, ProjectReadError, ProjectReader},
    state::{scheduling, task_records},
};
use std::collections::VecDeque;

struct Projects {
    inner: FakeGithub,
    states: VecDeque<Result<DependencyState, ProjectReadError>>,
}

impl ProjectReader for Projects {
    fn dependency(
        &mut self,
        reference: &IssueReference,
    ) -> Result<DependencyState, ProjectReadError> {
        assert_eq!(reference.repository, "org/foundation");
        assert_eq!(reference.number, 77);
        self.states.pop_front().expect("unexpected dependency read")
    }

    fn page(
        &mut self,
        project: &str,
        cursor: Option<&str>,
    ) -> Result<Page<ProjectItem>, ProjectReadError> {
        self.inner.page(project, cursor)
    }

    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        self.inner.issue(item)
    }
}

fn configure(fixture: &mut Fixture) {
    let rule = DependencyRule {
        issue: IssueReference {
            repository: fixture.candidate.repository.clone(),
            number: fixture.candidate.issue_number,
        },
        blocked_by: vec![IssueReference {
            repository: "org/foundation".into(),
            number: 77,
        }],
        allow_closed_unmerged_prs: false,
    };
    fixture.config.sources[0].dependencies.push(rule);
    fixture.candidate.source = fixture.config.sources[0].clone();
}

#[test]
fn dependency_reopened_before_dispatch_claim_or_launch_never_launches() {
    for blocking_read in 0..4 {
        let mut f = Fixture::new(2);
        configure(&mut f);
        let mut projects = Projects {
            inner: FakeGithub::new(&f.candidate),
            states: (0..=blocking_read)
                .map(|read| {
                    Ok(if read == blocking_read {
                        DependencyState::Open
                    } else {
                        DependencyState::ClosedIssue
                    })
                })
                .collect(),
        };
        let mut writer = FakeWriter::default();
        let mut launcher = FakeLauncher::default();
        let result = dispatch_one(
            &mut f.store,
            &f.candidate,
            DispatchDependencies {
                config: &f.config,
                config_revision: "revision",
                task_id: "task-dependency",
                attempt_id: "attempt-dependency",
                projects: &mut projects,
                prs: &mut FakePr::default(),
                assignments: &mut writer,
                launcher: &mut launcher,
            },
        );
        assert!(result.is_err(), "blocking read {blocking_read}");
        assert_eq!(writer.calls, usize::from(blocking_read >= 2));
        assert!(launcher.plans.is_empty());
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_records::task_phase(&f.store, "task-dependency").unwrap(),
            (blocking_read > 0).then(|| "held".into())
        );
        assert!(projects.states.is_empty());
    }
}

#[test]
fn blocked_issue_does_not_consume_capacity_or_prevent_independent_dispatch() {
    let mut f = Fixture::new(2);
    configure(&mut f);
    let mut independent = f.candidate.clone();
    independent.issue_number += 1;
    independent.issue_node_id = "independent".into();
    independent.item_id = "independent-item".into();
    independent.issue_url = format!(
        "https://github.com/{}/issues/{}",
        independent.repository, independent.issue_number
    );
    let mut projects = Projects {
        inner: FakeGithub::new(&independent),
        states: VecDeque::from([Ok(DependencyState::Open)]),
    };
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let report = schedule_candidates(
        &mut f.store,
        vec![f.candidate.clone(), independent],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut projects,
            prs: &mut FakePr::default(),
            assignments: &mut writer,
            launcher: &mut launcher,
            ids: &mut FixedIds::default(),
        },
    )
    .unwrap();
    assert_eq!(report.waiting.len(), 1);
    assert_eq!(report.waiting[0].candidate, f.candidate);
    assert_eq!(report.waiting[0].dependencies[0].dependency.number, 77);
    assert_eq!(report.launched.len(), 1);
    assert_eq!(writer.calls, 1);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert!(
        !task_records::existing_target(&f.store, &f.candidate.repository, f.candidate.issue_number)
            .unwrap()
    );
}

#[test]
fn dependency_read_failure_fails_before_task_assignment_or_launch() {
    let mut f = Fixture::new(2);
    configure(&mut f);
    let mut projects = Projects {
        inner: FakeGithub::new(&f.candidate),
        states: VecDeque::from([Err("dependency-unavailable".to_owned().into())]),
    };
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let result = dispatch_one(
        &mut f.store,
        &f.candidate,
        DispatchDependencies {
            config: &f.config,
            config_revision: "revision",
            task_id: "task-dependency",
            attempt_id: "attempt-dependency",
            projects: &mut projects,
            prs: &mut FakePr::default(),
            assignments: &mut writer,
            launcher: &mut launcher,
        },
    );
    assert!(result.is_err());
    assert_eq!(writer.calls, 0);
    assert!(launcher.plans.is_empty());
    assert!(
        task_records::task_phase(&f.store, "task-dependency")
            .unwrap()
            .is_none()
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}
