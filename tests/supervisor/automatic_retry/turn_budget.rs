use super::*;

fn budget_fixture(code: &str, exit: i32) -> Fixture {
    let mut f = Fixture::new(code, exit);
    f.config.retry_policy = RetryPolicy::Bounded {
        max_attempts: 2,
        backoff_secs: 60,
        classes: vec![TransientFailure::TurnBudget],
    };
    f
}

#[test]
fn typed_turn_budgets_resume_once_then_exhaust_without_losing_progress() {
    for code in ["turn-budget", "turn-time-exhausted"] {
        let mut f = budget_fixture(code, 6);
        f.launcher.real = true;
        let original = launch_intent(&f.store, "attempt-real").unwrap().unwrap();
        let original_receipt = fs::read(receipt_path(&f.config)).unwrap();
        let worktree = f.config.worktree_root.join("task");
        fs::write(worktree.join("unfinished"), "partial issue work").unwrap();
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
        f.elapsed("attempt-real", TransientFailure::TurnBudget);
        let report = f.schedule().unwrap();
        assert_eq!(
            report.automatic_retries[0].decision,
            RetryDecision::Launched
        );
        assert_eq!(report.launched.len(), 1);
        let plan = &report.launched[0];
        assert_eq!(plan.session_id, "task");
        assert_eq!(plan.worktree, worktree);
        assert!(plan.args.contains(&"--prompt".to_string()));
        assert_eq!(
            plan.args.last().unwrap().lines().next().unwrap(),
            format!(
                "Continue https://github.com/org/tracker/issues/7 for {}",
                plan.attempt_id
            )
        );
        assert!(plan.args.last().unwrap().contains("Before continuing, inspect the files left in the worktree by the naturally exited worker."));
        assert_budget_retry_audit(&f, plan);
        assert_eq!(reservation_count(&f.store).unwrap(), 1);
        assert!(f.schedule().unwrap().launched.is_empty());
        f.settle(&plan.attempt_id);
        restart(&mut f);
        for _ in 0..2 {
            let report = f.schedule().unwrap();
            assert_eq!(
                report.automatic_retries[0].decision,
                RetryDecision::Exhausted
            );
            assert!(report.launched.is_empty());
        }
        assert_eq!(f.launcher.plans.len(), 1);
        assert_eq!(reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_phase(&f.store, "task").unwrap().as_deref(),
            Some("attention")
        );
        assert_eq!(
            launch_intent(&f.store, "attempt-real").unwrap().unwrap(),
            original
        );
        assert_eq!(fs::read(receipt_path(&f.config)).unwrap(), original_receipt);
        assert_eq!(
            fs::read_to_string(worktree.join("unfinished")).unwrap(),
            "partial issue work"
        );
        assert_eq!(fs::read_dir(&f.config.worktree_root).unwrap().count(), 1);
    }
}

fn assert_budget_retry_audit(f: &Fixture, plan: &luthor::supervisor::LaunchPlan) {
    let audit = evidence_payloads(&f.store, "task", &plan.attempt_id, "retry_authorized").unwrap();
    assert_eq!(audit.len(), 1);
    let audit: serde_json::Value = serde_json::from_str(&audit[0]).unwrap();
    assert_eq!(audit["actor"], "operator");
    assert_eq!(
        audit["reason"],
        "automatic retry of typed worker turn budget"
    );
    let request = evidence_payloads(
        &f.store,
        "task",
        "attempt-real",
        "automatic_retry_requested",
    )
    .unwrap();
    assert_eq!(request.len(), 1);
    let request: serde_json::Value = serde_json::from_str(&request[0]).unwrap();
    assert_eq!(
        request["policy"]["classes"],
        serde_json::json!(["turn_budget"])
    );
    assert_eq!(request["previous_attempt_count"], 1);
    assert_eq!(request["attempt_id"], plan.attempt_id);
}

#[test]
fn turn_budget_retry_waits_for_exclusive_worktree_ownership_without_consuming_attempts() {
    let mut f = budget_fixture("turn-budget", 6);
    f.elapsed("attempt-real", TransientFailure::TurnBudget);
    let owner = luthor::WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap();
    for _ in 0..2 {
        let report = f.schedule().unwrap();
        assert_eq!(
            report.automatic_retries[0].decision,
            RetryDecision::WorktreeBusy
        );
        assert!(report.launched.is_empty());
        assert!(f.launcher.plans.is_empty());
        assert_eq!(f.ids.0, 0);
        assert_eq!(reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_phase(&f.store, "task").unwrap().as_deref(),
            Some("attention")
        );
        assert_eq!(
            latest_attempt(&f.store, "task").unwrap().as_deref(),
            Some("attempt-real")
        );
        for kind in ["automatic_retry_requested", "automatic_retry_failed"] {
            assert!(
                evidence_payloads(&f.store, "task", "attempt-real", kind)
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(matches!(
            luthor::WorktreeOwner::acquire_existing(f.store.root(), "task"),
            Err(luthor::OwnershipError::Busy)
        ));
        restart(&mut f);
    }
    drop(owner);
    let report = f.schedule().unwrap();
    assert_eq!(
        report.automatic_retries[0].decision,
        RetryDecision::Launched
    );
    assert_eq!(report.launched.len(), 1);
    assert_eq!(f.launcher.plans.len(), 1);
    assert_eq!(f.ids.0, 1);
    assert!(f.schedule().unwrap().launched.is_empty());
}

#[test]
fn turn_budget_class_is_opt_in_and_does_not_match_other_codes_or_exit_families() {
    for (code, exit, decision) in [
        ("turn-budget", 5, RetryDecision::Unclassified),
        ("turn-time-exhausted", 5, RetryDecision::Unclassified),
        ("turn", 6, RetryDecision::Unclassified),
        ("limit", 6, RetryDecision::Unclassified),
        ("max-tool-calls", 2, RetryDecision::Unclassified),
        ("model-connectivity", 5, RetryDecision::ClassDisabled),
        ("", 0, RetryDecision::NeedsHumanInput),
    ] {
        let mut f = budget_fixture(code, exit);
        let report = f.schedule().unwrap();
        assert_eq!(report.automatic_retries[0].decision, decision, "{code}");
        assert!(report.launched.is_empty());
        assert!(f.launcher.plans.is_empty());
        assert_eq!(reservation_count(&f.store).unwrap(), 0);
        assert_eq!(
            task_phase(&f.store, "task").unwrap().as_deref(),
            Some("attention")
        );
    }
    for code in ["turn-budget", "turn-time-exhausted"] {
        let mut f = Fixture::new(code, 6);
        f.elapsed("attempt-real", TransientFailure::TurnBudget);
        assert_eq!(
            f.schedule().unwrap().automatic_retries[0].decision,
            RetryDecision::ClassDisabled
        );
        assert!(f.launcher.plans.is_empty());
        f.config.retry_policy = RetryPolicy::Disabled {};
        assert!(f.schedule().unwrap().automatic_retries.is_empty());
    }
}

#[test]
fn non_natural_turn_budget_exits_are_ineligible() {
    for stopped in [false, true] {
        let mut f = budget_fixture("turn-budget", 6);
        let mut receipt: luthor::supervisor::ExitReceipt =
            serde_json::from_slice(&fs::read(receipt_path(&f.config)).unwrap()).unwrap();
        if stopped {
            receipt.stop_signals.push(libc::SIGTERM);
        } else {
            receipt.exit_code = None;
            receipt.signal = Some(libc::SIGTERM);
        }
        // Model already-accounted non-natural exit evidence, not a new natural
        // worker outcome. Eligibility must reject it before any retry request.
        f.db().execute("UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind='attempt_exit'", [serde_json::to_string(&receipt).unwrap()]).unwrap();
        let report = f.schedule().unwrap();
        assert_eq!(
            report.automatic_retries[0].decision,
            RetryDecision::Ineligible
        );
        assert!(report.launched.is_empty());
        assert!(f.launcher.plans.is_empty());
        assert_eq!(reservation_count(&f.store).unwrap(), 0);
        assert!(
            evidence_payloads(&f.store, "task", "attempt-real", "worker_outcome")
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn unavailable_worktree_ownership_is_an_error_not_a_waiting_retry() {
    let mut f = budget_fixture("turn-budget", 6);
    f.elapsed("attempt-real", TransientFailure::TurnBudget);
    fs::remove_file(f.store.root().join("worktree-task.lock")).unwrap();
    assert!(f.schedule().is_err());
    assert!(f.launcher.plans.is_empty());
    assert_eq!(reservation_count(&f.store).unwrap(), 0);
}
