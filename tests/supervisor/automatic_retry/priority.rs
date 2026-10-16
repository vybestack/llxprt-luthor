//! A released slot must not send nearly-finished work behind a new issue.
use super::*;
use luthor::coordinator::ScheduleReport;

struct Projects {
    retry: OtherProject,
    ready: OtherProject,
}

impl ProjectReader for Projects {
    fn dependency(
        &mut self,
        reference: &luthor::config::IssueReference,
    ) -> Result<luthor::dependencies::DependencyState, ProjectReadError> {
        self.ready.dependency(reference)
    }

    fn page(
        &mut self,
        project: &str,
        cursor: Option<&str>,
    ) -> Result<Page<ProjectItem>, ProjectReadError> {
        let mut page = self.retry.page(project, cursor)?;
        page.items.extend(self.ready.page(project, cursor)?.items);
        Ok(page)
    }

    fn issue(&mut self, item: &ProjectItem) -> Result<Issue, ProjectReadError> {
        if item.issue_node_id == self.retry.0.issue_node_id {
            self.retry.issue(item)
        } else {
            self.ready.issue(item)
        }
    }
}

pub(super) fn candidate(f: &Fixture) -> Candidate {
    let mut ready = f.projects.0.clone();
    ready.item_id = "new-item".into();
    ready.issue_node_id = "new-issue".into();
    ready.issue_number = 8;
    ready.issue_url = "https://github.com/org/tracker/issues/8".into();
    ready
}

pub(super) fn schedule(f: &mut Fixture) -> ScheduleReport {
    let ready = candidate(f);
    let mut projects = Projects {
        retry: OtherProject(f.projects.0.clone(), 1),
        ready: OtherProject(ready.clone(), 0),
    };
    schedule_candidates(
        &mut f.store,
        vec![ready],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "retry-priority",
            projects: &mut projects,
            prs: &mut f.prs,
            assignments: &mut OtherWriter,
            launcher: &mut f.launcher,
            ids: &mut f.ids,
        },
    )
    .unwrap()
}

#[test]
fn backoff_defers_new_selection_across_restart_then_retry_wins_capacity_one() {
    let mut f = Fixture::new("turn-budget", 6);
    f.config.retry_policy = RetryPolicy::Bounded {
        max_attempts: 2,
        backoff_secs: 600,
        classes: vec![TransientFailure::TurnBudget],
    };
    let mut deadline = None;
    for _ in 0..2 {
        let report = schedule(&mut f);
        let retry = &report.automatic_retries[0];
        assert_eq!(retry.decision, RetryDecision::Backoff);
        let saved = evidence_payloads(&f.store, "task", "attempt-real", "worker_outcome").unwrap();
        let saved: serde_json::Value = serde_json::from_str(&saved[0]).unwrap();
        let expected = saved["observed_at_unix_secs"].as_u64().unwrap() + 600;
        assert_eq!(retry.backoff_until_unix_secs, Some(expected));
        let json = serde_json::to_value(retry).unwrap();
        assert_eq!(json["task_id"], "task");
        assert_eq!(json["attempt_id"], "attempt-real");
        assert_eq!(json["backoff_until_unix_secs"], expected);
        if let Some(previous) = deadline {
            assert_eq!(previous, expected);
        }
        deadline = Some(expected);
        assert!(report.launched.is_empty());
        assert_eq!(
            luthor::state::task_records::task_count(&f.store).unwrap(),
            1
        );
        assert_eq!(reservation_count(&f.store).unwrap(), 0);
        assert_eq!(f.ids.0, 0);
        restart(&mut f);
    }
    f.elapsed("attempt-real", TransientFailure::TurnBudget);
    let report = schedule(&mut f);
    assert_eq!(
        report.automatic_retries[0].decision,
        RetryDecision::Launched
    );
    assert_eq!(report.launched.len(), 1);
    assert_eq!(report.launched[0].task_id, "task");
    assert!(report.capacity_full);
    assert_eq!(f.launcher.plans.len(), 1);
    assert_eq!(
        luthor::state::task_records::task_count(&f.store).unwrap(),
        1
    );
    assert_eq!(reservation_count(&f.store).unwrap(), 1);
}

#[test]
fn terminal_or_disabled_retries_do_not_block_new_selection() {
    for (code, exit, policy, decision) in [
        (
            "model-connectivity",
            5,
            "exhausted",
            Some(RetryDecision::Exhausted),
        ),
        (
            "model-connectivity",
            5,
            "class_disabled",
            Some(RetryDecision::ClassDisabled),
        ),
        (
            "model-quota-exhausted",
            5,
            "bounded",
            Some(RetryDecision::NonTransient),
        ),
        ("model", 5, "bounded", Some(RetryDecision::Unclassified)),
        ("", 0, "bounded", Some(RetryDecision::NeedsHumanInput)),
        ("model-connectivity", 5, "disabled", None),
        (
            "model-connectivity",
            5,
            "ineligible",
            Some(RetryDecision::Ineligible),
        ),
    ] {
        let mut f = Fixture::new(code, exit);
        match policy {
            "exhausted" => {
                f.elapsed("attempt-real", TransientFailure::Connectivity);
                f.launcher.real = true;
                let report = f.schedule().unwrap();
                f.settle(&report.launched[0].attempt_id);
                f.launcher.real = false;
                f.config.retry_policy = RetryPolicy::Bounded {
                    max_attempts: 2,
                    backoff_secs: 60,
                    classes: vec![TransientFailure::Connectivity],
                };
            }
            "class_disabled" => {
                f.config.retry_policy = RetryPolicy::Bounded {
                    max_attempts: 3,
                    backoff_secs: 60,
                    classes: vec![TransientFailure::RateLimit],
                }
            }
            "disabled" => f.config.retry_policy = RetryPolicy::Disabled {},
            "ineligible" => force_refusal(&mut f, "stop"),
            _ => (),
        }
        let previous = latest_attempt(&f.store, "task").unwrap();
        let report = schedule(&mut f);
        assert_eq!(
            report.automatic_retries.first().map(|r| r.decision.clone()),
            decision,
            "{policy}"
        );
        assert!(
            report
                .automatic_retries
                .iter()
                .all(|r| r.backoff_until_unix_secs.is_none())
        );
        assert_eq!(report.launched.len(), 1, "{policy}");
        assert_ne!(report.launched[0].task_id, "task", "{policy}");
        assert_eq!(reservation_count(&f.store).unwrap(), 1);
        assert_eq!(latest_attempt(&f.store, "task").unwrap(), previous);
    }
}
