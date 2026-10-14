#![cfg(unix)]
use super::*;
mod fixture;
use fixture::Fixture;
use luthor::state::{
    journal::evidence_payloads,
    launches::launch_intent,
    scheduling::reservation_count,
    task_records::{latest_attempt, selection_evidence, task_phase},
};
use luthor::{
    config::{RetryPolicy, TransientFailure},
    coordinator::{RetryDecision, WorkerOutcome},
};

#[test]
fn automatic_scheduler_retries_typed_failures_with_bounded_history_across_restart() {
    let mut f = Fixture::new("model-connectivity", 5);
    f.launcher.real = true;
    let original = selection_evidence(&f.store, "task").unwrap().unwrap();
    let old_plan = launch_intent(&f.store, "attempt-real").unwrap().unwrap();
    let old_receipt = fs::read(receipt_path(&f.config)).unwrap();
    let worktree = f.config.worktree_root.join("task");
    fs::write(worktree.join("unfinished"), "preserve worker progress").unwrap();
    let mut previous = "attempt-real".to_string();
    for expected in 1..=2 {
        f.elapsed(&previous, TransientFailure::Connectivity);
        let report = f.schedule().unwrap();
        assert_eq!(
            report.automatic_retries[0].decision,
            RetryDecision::Launched
        );
        assert_eq!(report.launched.len(), 1);
        let plan = &report.launched[0];
        assert_eq!(plan.session_id, "task");
        assert_eq!(plan.worktree, worktree);
        assert_eq!(f.launcher.plans.len(), expected);
        let audit =
            evidence_payloads(&f.store, "task", &plan.attempt_id, "retry_authorized").unwrap();
        let audit: serde_json::Value = serde_json::from_str(&audit[0]).unwrap();
        assert_eq!(audit["actor"], "operator");
        assert_eq!(audit["previous_plan"]["attempt_id"], previous);
        assert_eq!(
            audit["reason"],
            "automatic retry of typed transient worker failure"
        );
        assert_eq!(audit["pr"]["status"]["status"], "absent");
        let request =
            evidence_payloads(&f.store, "task", &previous, "automatic_retry_requested").unwrap();
        let request: serde_json::Value = serde_json::from_str(&request[0]).unwrap();
        assert_eq!(request["policy"]["max_attempts"], 3);
        assert_eq!(request["previous_attempt_count"], expected);
        assert_eq!(request["attempt_id"], plan.attempt_id);
        previous = plan.attempt_id.clone();
        // Polling while the new reservation is held cannot duplicate the launch.
        assert!(f.schedule().unwrap().launched.is_empty());
        f.settle(&previous);
        restart(&mut f);
    }
    let report = f.schedule().unwrap();
    assert_eq!(
        report.automatic_retries[0].decision,
        RetryDecision::Exhausted
    );
    assert!(report.launched.is_empty());
    assert_eq!(f.launcher.plans.len(), 2);
    assert_eq!(reservation_count(&f.store).unwrap(), 0);
    assert_eq!(
        selection_evidence(&f.store, "task").unwrap().unwrap(),
        original
    );
    assert_eq!(
        launch_intent(&f.store, "attempt-real").unwrap().unwrap(),
        old_plan
    );
    assert_eq!(fs::read(receipt_path(&f.config)).unwrap(), old_receipt);
    assert_eq!(
        fs::read_to_string(worktree.join("unfinished")).unwrap(),
        "preserve worker progress"
    );
    assert_eq!(fs::read_dir(&f.config.worktree_root).unwrap().count(), 1);
}

fn restart(f: &mut Fixture) {
    let placeholder = StateStore::open(f.dir.path().join("placeholder"), 1).unwrap();
    drop(std::mem::replace(&mut f.store, placeholder));
    f.store = StateStore::open(&f.config.state_root, f.config.capacity).unwrap();
}

#[test]
fn automatic_retry_backoff_is_durable_and_capacity_is_not_bypassed() {
    let mut f = Fixture::new("model-rate-limit", 5);
    assert_eq!(
        f.schedule().unwrap().automatic_retries[0].decision,
        RetryDecision::Backoff
    );
    restart(&mut f);
    assert_eq!(
        f.schedule().unwrap().automatic_retries[0].decision,
        RetryDecision::Backoff
    );
    assert_eq!(
        evidence_payloads(&f.store, "task", "attempt-real", "worker_outcome")
            .unwrap()
            .len(),
        1
    );
    f.elapsed("attempt-real", TransientFailure::RateLimit);
    let mut other = f.projects.0.clone();
    other.issue_node_id = "other".into();
    other.item_id = "other".into();
    other.issue_number = 8;
    other.issue_url = "https://github.com/org/tracker/issues/8".into();
    luthor::state::task_records::create_task(&mut f.store, "other", &other, "rev", &f.config)
        .unwrap();
    luthor::state::scheduling::reserve(&mut f.store, "other", "other-attempt").unwrap();
    assert_eq!(
        f.schedule().unwrap().automatic_retries[0].decision,
        RetryDecision::CapacityFull
    );
    assert!(f.launcher.plans.is_empty());
    assert_eq!(
        latest_attempt(&f.store, "task").unwrap().as_deref(),
        Some("attempt-real")
    );
    assert_eq!(reservation_count(&f.store).unwrap(), 1);
}

#[test]
fn automatic_retry_is_opt_in_and_human_or_untyped_outcomes_are_distinct() {
    for (code, exit, decision) in [
        ("model-connectivity", 5, RetryDecision::ClassDisabled),
        ("model-quota-exhausted", 5, RetryDecision::NonTransient),
        ("model", 5, RetryDecision::Unclassified),
        ("turn", 6, RetryDecision::Unclassified),
        ("", 0, RetryDecision::NeedsHumanInput),
    ] {
        let mut f = Fixture::new(code, exit);
        f.config.retry_policy = RetryPolicy::Disabled {};
        assert!(f.schedule().unwrap().automatic_retries.is_empty());
        assert!(
            evidence_payloads(&f.store, "task", "attempt-real", "worker_outcome")
                .unwrap()
                .is_empty()
        );
        f.config.retry_policy = RetryPolicy::Bounded {
            max_attempts: 3,
            backoff_secs: 60,
            classes: vec![TransientFailure::RateLimit],
        };
        let report = f.schedule().unwrap();
        assert_eq!(report.automatic_retries[0].decision, decision);
        assert!(report.launched.is_empty());
        assert_eq!(reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_phase(&f.store, "task").unwrap().as_deref(),
            Some("attention")
        );
        if exit == 0 {
            assert_eq!(
                report.automatic_retries[0].outcome,
                WorkerOutcome::NeedsHumanInput
            );
            let args = vec!["show".into(), "task".into()];
            let output = luthor::cli::execute(&f.config.state_root, &args).unwrap();
            let output: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(output["worker_outcome"]["kind"], "needs_human_input");
            let status = luthor::cli::execute(&f.config.state_root, &["status".into()]).unwrap();
            let status: serde_json::Value = serde_json::from_str(&status).unwrap();
            assert_eq!(
                status["tasks"][0]["worker_outcome"]["kind"],
                "needs_human_input"
            );
        }
    }
}

#[test]
fn automatic_retry_refuses_changed_claim_pr_identity_and_unverified_exit() {
    for refusal in [
        "claim",
        "pr_present",
        "pr_error",
        "identity",
        "process",
        "receipt",
        "stop",
        "launch",
    ] {
        let mut f = Fixture::new("model-transient-server", 5);
        f.elapsed("attempt-real", TransientFailure::TransientServer);
        match refusal {
            "claim" => f.projects.1 = 0,
            "pr_present" => {
                f.prs.matching = Some((f.projects.0.issue_url.clone(), "luthor/task".into()))
            }
            "pr_error" => f.prs.fail = true,
            "identity" => f.prs.login = Some("another".into()),
            "process" => {
                let (boot, start) = test_process_identity(std::process::id());
                luthor::state::journal::record_evidence(&mut f.store, "task", Some("attempt-real"), "tracked_descendant", &serde_json::json!({"pid":std::process::id(),"boot_identity":boot,"start_identity":start}).to_string()).unwrap();
            }
            "receipt" => {
                fs::remove_file(receipt_path(&f.config)).unwrap();
            }
            "stop" => {
                luthor::state::journal::record_intent(
                    &mut f.store,
                    "late-stop",
                    "task",
                    Some("attempt-real"),
                    "stop",
                    "{}",
                )
                .unwrap();
            }
            "launch" => f.launcher.fail = true,
            _ => unreachable!(),
        }
        if refusal == "stop" {
            let report = f.schedule().unwrap();
            assert_eq!(
                report.automatic_retries[0].decision,
                RetryDecision::Ineligible
            );
        } else {
            assert!(f.schedule().is_err(), "{refusal}");
            assert_eq!(
                evidence_payloads(&f.store, "task", "attempt-real", "automatic_retry_failed")
                    .unwrap()
                    .len(),
                1,
                "{refusal}"
            );
        }
        if refusal == "launch" {
            assert_eq!(f.launcher.plans.len(), 1);
            assert_eq!(reservation_count(&f.store).unwrap(), 1);
            assert_eq!(
                task_phase(&f.store, "task").unwrap().as_deref(),
                Some("held")
            );
            assert!(f.schedule().unwrap().launched.is_empty());
        } else {
            assert!(f.launcher.plans.is_empty(), "{refusal}");
            assert_eq!(reservation_count(&f.store).unwrap(), 0, "{refusal}");
            assert_eq!(
                latest_attempt(&f.store, "task").unwrap().as_deref(),
                Some("attempt-real")
            );
        }
    }
}

#[test]
fn automatic_limit_counts_an_explicit_operator_continuation() {
    let mut f = Fixture::new("model-connectivity", 5);
    f.launcher.real = true;
    let manual = luthor::coordinator::retry_one(
        &mut f.store,
        luthor::coordinator::RetryDependencies {
            task_id: "task",
            previous_attempt_id: "attempt-real",
            attempt_id: "operator-retry",
            config: &f.config,
            config_revision: "operator-revision",
            actor: "operator",
            reason: "explicit continuation",
            revalidate_terminal_exit: false,
            projects: &mut f.projects,
            prs: &mut f.prs,
            launcher: &mut f.launcher,
        },
    )
    .unwrap();
    f.settle(&manual.attempt_id);
    f.config.retry_policy = RetryPolicy::Bounded {
        max_attempts: 2,
        backoff_secs: 60,
        classes: vec![TransientFailure::Connectivity],
    };
    restart(&mut f);
    let report = f.schedule().unwrap();
    assert_eq!(
        report.automatic_retries[0].decision,
        RetryDecision::Exhausted
    );
    assert!(report.launched.is_empty());
    assert_eq!(f.launcher.plans.len(), 1);
    assert_eq!(reservation_count(&f.store).unwrap(), 0);
    assert!(
        evidence_payloads(
            &f.store,
            "task",
            &manual.attempt_id,
            "automatic_retry_requested"
        )
        .unwrap()
        .is_empty()
    );
}
