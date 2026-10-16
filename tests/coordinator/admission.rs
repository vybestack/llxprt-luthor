//! Admission refusal is not a queue-wide failure; uncertain launch still is.
use super::{FakeGithub, FakeLauncher, FakePr, FakeWriter, FixedIds, Fixture};
use luthor::{
    coordinator::{ScheduleDependencies, schedule_candidates},
    state::{scheduling, task_records},
};

#[test]
fn refused_candidate_is_reported_and_independent_candidate_still_launches() {
    let mut f = Fixture::new(2);
    let mut independent = f.candidate.clone();
    independent.item_id = "independent-item".into();
    independent.issue_node_id = "independent-issue".into();
    independent.issue_number = 2;
    independent.issue_url = "https://github.com/org/tracker/issues/2".into();
    // The first candidate's membership changed before admission. Only the
    // independent issue is still present in the live Project read.
    let mut projects = FakeGithub::new(&independent);
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
    assert_eq!(report.admission_failures.len(), 1);
    let failure = &report.admission_failures[0];
    assert_eq!(failure.task_id, "task-1");
    assert_eq!(failure.attempt_id, "attempt-2");
    assert_eq!(failure.check, "candidate_admission");
    assert_eq!(
        failure.reason,
        "claim drift: issue assignment or source evidence changed"
    );
    assert_eq!(report.launched.len(), 1);
    assert_eq!(report.launched[0].task_id, "task-3");
    assert_eq!(report.launched[0].attempt_id, "attempt-4");
    assert_eq!(launcher.plans.len(), 1);
    assert_eq!(writer.calls, 1);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&f.store, "task-1")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert!(
        task_records::latest_attempt(&f.store, "task-1")
            .unwrap()
            .is_none()
    );
    let saved = luthor::state::journal::evidence_payloads(
        &f.store,
        "task-1",
        "attempt-2",
        "scheduling_admission_failed",
    )
    .unwrap();
    let saved: serde_json::Value = serde_json::from_str(&saved[0]).unwrap();
    assert_eq!(saved["reason"], failure.reason);
}

#[test]
fn uncertain_launch_is_contextual_fatal_and_preserves_reservation() {
    let mut f = Fixture::new(2);
    let mut projects = FakeGithub::new(&f.candidate);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher {
        fail: true,
        ..Default::default()
    };
    let error = schedule_candidates(
        &mut f.store,
        vec![f.candidate.clone()],
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
    .unwrap_err();
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic["task_id"], "task-1");
    assert_eq!(diagnostic["attempt_id"], "attempt-2");
    assert_eq!(diagnostic["check"], "candidate_dispatch");
    assert!(
        diagnostic["reason"]
            .as_str()
            .unwrap()
            .contains("supervisor execution is unavailable")
    );
    assert_eq!(launcher.plans.len(), 1);
    assert_eq!(writer.calls, 1);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&f.store, "task-1")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    assert_eq!(
        task_records::latest_attempt(&f.store, "task-1")
            .unwrap()
            .as_deref(),
        Some("attempt-2")
    );
}

#[test]
fn malformed_source_contract_is_diagnosed_without_disclosing_upstream_identity() {
    use luthor::{
        claim::ClaimError,
        coordinator::{DispatchError, ScheduleError},
        github::project::ProjectError,
    };
    for (source, reason) in [
        (
            ProjectError::MissingCursor,
            "Project pagination returned no cursor",
        ),
        (
            ProjectError::RepeatedCursor,
            "Project pagination repeated cursor",
        ),
        (
            ProjectError::Inconsistent {
                item_id: "private-upstream-value".into(),
                issue_id: "private-upstream-value".into(),
            },
            "Project item disagrees with direct issue identity",
        ),
        (
            ProjectError::Duplicate("private-upstream-value".into()),
            "Project enumeration returned duplicate issue identity",
        ),
    ] {
        let error = ScheduleError::Task {
            task_id: "task".into(),
            attempt_id: "attempt".into(),
            check: "candidate_dispatch",
            source: Box::new(DispatchError::Claim(ClaimError::Source(source))),
        };
        let diagnostic = error.diagnostic();
        assert_eq!(diagnostic["task_id"], "task");
        assert_eq!(diagnostic["attempt_id"], "attempt");
        assert_eq!(diagnostic["reason"], reason);
        assert!(!diagnostic.to_string().contains("private-upstream-value"));
    }
}

struct MalformedProject(FakeGithub, u32);
impl luthor::github::project::ProjectReader for MalformedProject {
    fn dependency(
        &mut self,
        reference: &luthor::config::IssueReference,
    ) -> Result<luthor::dependencies::DependencyState, luthor::github::project::ProjectReadError>
    {
        self.0.dependency(reference)
    }
    fn page(
        &mut self,
        _: &str,
        _: Option<&str>,
    ) -> Result<
        luthor::github::project::Page<luthor::github::project::ProjectItem>,
        luthor::github::project::ProjectReadError,
    > {
        self.1 += 1;
        Err("invalid-json".to_owned().into())
    }
    fn issue(
        &mut self,
        item: &luthor::github::project::ProjectItem,
    ) -> Result<luthor::github::project::Issue, luthor::github::project::ProjectReadError> {
        self.0.issue(item)
    }
}

#[test]
fn malformed_upstream_is_fatal_not_a_task_local_fallback() {
    let mut f = Fixture::new(2);
    let mut second = f.candidate.clone();
    second.issue_node_id = "issue-2".into();
    second.issue_number = 2;
    let mut projects = MalformedProject(FakeGithub::new(&f.candidate), 0);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();
    let mut ids = FixedIds::default();
    let error = schedule_candidates(
        &mut f.store,
        vec![f.candidate.clone(), second],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "revision",
            projects: &mut projects,
            prs: &mut FakePr::default(),
            assignments: &mut writer,
            launcher: &mut launcher,
            ids: &mut ids,
        },
    )
    .unwrap_err();
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic["task_id"], "task-1");
    assert_eq!(diagnostic["attempt_id"], "attempt-2");
    assert_eq!(diagnostic["check"], "candidate_dispatch");
    assert!(
        diagnostic["reason"]
            .as_str()
            .unwrap()
            .contains("ProjectPage Malformed invalid-json")
    );
    assert_eq!(projects.1, 1);
    assert_eq!(ids.0, 2);
    assert_eq!(writer.calls, 0);
    assert!(launcher.plans.is_empty());
    assert_eq!(task_records::task_count(&f.store).unwrap(), 1);
    assert!(
        task_records::latest_attempt(&f.store, "task-1")
            .unwrap()
            .is_none()
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}
