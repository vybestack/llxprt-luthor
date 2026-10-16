//! Issue 93: discovery revisions cannot poison retries or the rest of a cycle.
use super::*;
use luthor::config::{DependencyRule, IssueReference};

fn add_dependencies(f: &mut Fixture) {
    f.config.sources[0].dependencies.push(DependencyRule {
        issue: IssueReference {
            repository: "org/tracker".into(),
            number: 8,
        },
        blocked_by: vec![IssueReference {
            repository: "org/tracker".into(),
            number: 9,
        }],
        allow_closed_unmerged_prs: false,
    });
    f.config.validate().unwrap();
}

#[test]
fn dependencies_only_revision_retries_turn_budget_with_immutable_selection_and_audit() {
    let mut f = Fixture::new("turn-budget", 6);
    f.config.retry_policy = RetryPolicy::Bounded {
        max_attempts: 2,
        backoff_secs: 60,
        classes: vec![TransientFailure::TurnBudget],
    };
    let original = selection_evidence(&f.store, "task").unwrap().unwrap();
    let original_plan = launch_intent(&f.store, "attempt-real").unwrap().unwrap();
    let original_receipt = fs::read(receipt_path(&f.config)).unwrap();
    add_dependencies(&mut f);
    f.elapsed("attempt-real", TransientFailure::TurnBudget);
    f.launcher.real = true;
    let report = f.schedule().unwrap();
    assert_eq!(
        report.automatic_retries[0].decision,
        RetryDecision::Launched
    );
    assert!(report.automatic_retries[0].failure.is_none());
    let plan = &report.launched[0];
    assert_eq!(plan.task_id, "task");
    assert_eq!(plan.session_id, "task");
    assert_eq!(plan.worktree, f.config.worktree_root.join("task"));
    f.settle(&plan.attempt_id);
    restart(&mut f);
    assert_eq!(
        f.schedule().unwrap().automatic_retries[0].decision,
        RetryDecision::Exhausted
    );
    assert_eq!(
        selection_evidence(&f.store, "task").unwrap().unwrap(),
        original
    );
    assert_eq!(
        launch_intent(&f.store, "attempt-real").unwrap().unwrap(),
        original_plan
    );
    assert_eq!(fs::read(receipt_path(&f.config)).unwrap(), original_receipt);
    let audit = evidence_payloads(&f.store, "task", &plan.attempt_id, "retry_authorized").unwrap();
    let audit: serde_json::Value = serde_json::from_str(&audit[0]).unwrap();
    assert_eq!(
        audit["config"]["sources"][0]["dependencies"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        audit["previous_config"]["sources"][0]
            .get("dependencies")
            .is_none()
    );
    assert_eq!(reservation_count(&f.store).unwrap(), 0);
}

#[test]
fn discovery_markers_milestones_and_other_repositories_do_not_invalidate_selected_retry() {
    let mut f = Fixture::new("model-connectivity", 5);
    f.elapsed("attempt-real", TransientFailure::Connectivity);
    f.config.sources[0].ready_marker = Marker::Label {
        name: "new-ready".into(),
    };
    f.config.sources[0].milestone = Some("future".into());
    f.config.sources[0].repositories.push("org/other".into());
    let mut mapping = f.config.mappings[0].clone();
    mapping.tracker_repository = "org/other".into();
    mapping.code_repository = "org/other-code".into();
    mapping.base_branch = "unrelated-base".into();
    f.config.mappings.insert(0, mapping);
    f.config.validate().unwrap();
    let report = f.schedule().unwrap();
    assert_eq!(
        report.automatic_retries[0].decision,
        RetryDecision::Launched
    );
    assert_eq!(report.launched.len(), 1);
    assert_eq!(report.launched[0].expected_worktree.base, "main");
    assert_eq!(f.launcher.plans.len(), 1);
}

#[test]
fn task_affecting_mismatch_is_durable_visible_and_does_not_consume_an_attempt() {
    for field in ["mappings", "assignment_login", "worktree_root", "cleanup"] {
        let mut f = Fixture::new("model-connectivity", 5);
        f.elapsed("attempt-real", TransientFailure::Connectivity);
        match field {
            "mappings" => f.config.mappings[0].base_branch = "changed-base".into(),
            "assignment_login" => f.config.assignment_login = "changed-login".into(),
            "worktree_root" => f.config.worktree_root = f.dir.path().join("changed-worktrees"),
            "cleanup" => f.config.cleanup = luthor::config::CleanupPolicy::Worktree,
            _ => unreachable!(),
        }
        for _ in 0..2 {
            let report = f.schedule().unwrap();
            let retry = &report.automatic_retries[0];
            assert_eq!(retry.decision, RetryDecision::ConfigMismatch);
            let failure = retry.failure.as_ref().unwrap();
            assert_eq!(failure.task_id, "task");
            assert_eq!(failure.attempt_id, "attempt-real");
            assert_eq!(failure.check, "retry_admission");
            assert_eq!(failure.differing_fields, [field]);
            assert!(failure.reason.contains(field));
            assert!(report.launched.is_empty());
        }
        assert!(f.launcher.plans.is_empty());
        assert_eq!(reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            latest_attempt(&f.store, "task").unwrap().as_deref(),
            Some("attempt-real")
        );
        assert_eq!(
            task_phase(&f.store, "task").unwrap().as_deref(),
            Some("attention")
        );
        assert_eq!(
            evidence_payloads(
                &f.store,
                "task",
                "attempt-real",
                "scheduling_admission_failed"
            )
            .unwrap()
            .len(),
            2
        );
        for args in [vec!["show".into(), "task".into()], vec!["status".into()]] {
            let view: serde_json::Value =
                serde_json::from_str(&luthor::cli::execute(&f.config.state_root, &args).unwrap())
                    .unwrap();
            let task = if args[0] == "status" {
                &view["tasks"][0]
            } else {
                &view
            };
            assert_eq!(
                task["scheduling_failure"]["differing_fields"],
                serde_json::json!([field])
            );
        }
    }
}

#[test]
fn blocked_retry_does_not_prevent_independent_candidate_dispatch() {
    let mut f = Fixture::new("model-connectivity", 5);
    f.elapsed("attempt-real", TransientFailure::Connectivity);
    f.config.cleanup = luthor::config::CleanupPolicy::Worktree;
    let mut independent = f.projects.0.clone();
    independent.item_id = "independent".into();
    independent.issue_node_id = "independent".into();
    independent.issue_number = 8;
    independent.issue_url = "https://github.com/org/tracker/issues/8".into();
    let mut projects = OtherProject(independent.clone(), 0);
    let report = schedule_candidates(
        &mut f.store,
        vec![independent],
        ScheduleDependencies {
            config: &f.config,
            config_revision: "changed-cleanup",
            projects: &mut projects,
            prs: &mut f.prs,
            assignments: &mut OtherWriter,
            launcher: &mut f.launcher,
            ids: &mut f.ids,
        },
    )
    .unwrap();
    assert_eq!(
        report.automatic_retries[0].decision,
        RetryDecision::ConfigMismatch
    );
    assert_eq!(report.launched.len(), 1);
    assert_ne!(report.launched[0].task_id, "task");
    assert_eq!(
        task_phase(&f.store, "task").unwrap().as_deref(),
        Some("attention")
    );
    assert_eq!(
        latest_attempt(&f.store, "task").unwrap().as_deref(),
        Some("attempt-real")
    );
    assert_eq!(reservation_count(&f.store).unwrap(), 1);
}
