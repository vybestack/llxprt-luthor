//! Issue 87: real worker envelopes/receipts and bounded existing-PR relaunches.
use super::*;
use luthor::config::{RetryPolicy, TransientFailure};
use luthor::coordinator::RepairOutcome;
use luthor::supervisor::LaunchPlan;

pub(super) fn bounded(f: &mut Fixture, max_attempts: u32) {
    f.config.retry_policy = RetryPolicy::Bounded {
        max_attempts,
        backoff_secs: 60,
        classes: vec![
            TransientFailure::TransientServer,
            TransientFailure::TurnBudget,
        ],
    };
}

pub(super) fn db(f: &Fixture) -> rusqlite::Connection {
    rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap()
}

pub(super) fn run_script(f: &mut Fixture, plan: &LaunchPlan, script: &str) {
    fs::write(plan.executable.with_extension("behavior"), script).unwrap();
    let owner = luthor::WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap();
    execute_with_binary(
        &mut f.store,
        plan,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    drop(owner);
    wait_for_receipt(&f.config, &f.store, &plan.attempt_id);
    supervisor_support::wait_for_worktree_owner_release(f.store.root(), "task");
}

pub(super) fn fail(f: &mut Fixture, plan: &LaunchPlan, code: &str, exit: i32) {
    let envelope = serde_json::json!({"status":"error", "session_id":plan.session_id,
        "error":{"code":code,"message":"body read failed 503; not classified by message"}});
    run_script(
        f,
        plan,
        &format!("#!/bin/sh\nprintf '%s\\n' '{envelope}'\nexit {exit}\n"),
    );
}

pub(super) fn reconcile(f: &mut Fixture, attempt: &str) -> Result<Reconciliation, SupervisorError> {
    luthor::coordinator::reconcile_with_pr(
        &mut f.store,
        "task",
        attempt,
        &mut f.projects,
        &mut f.prs,
    )
}

/// Model an earlier observation/deadline without sleeping or changing policy.
fn elapsed(f: &Fixture, attempt: &str) {
    let db = db(f);
    for (kind, key) in [
        ("worker_outcome", "observed_at_unix_secs"),
        ("pr_repair_retry_scheduled", "backoff_until_unix_secs"),
    ] {
        assert_eq!(db.execute("UPDATE evidence SET payload=json_set(payload,?1,1) WHERE task_id='task' AND attempt_id=?2 AND kind=?3", rusqlite::params![format!("$.{key}"), attempt, kind]).unwrap(), 1);
    }
}

pub(super) fn phase(f: &Fixture, expected: &str) {
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some(expected)
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}

fn assert_scheduled(f: &Fixture, outcome: &RepairOutcome, attempt: &str) -> u64 {
    assert!(outcome.launched.is_empty(), "{outcome:?}");
    assert!(outcome.held.is_empty(), "{outcome:?}");
    assert!(outcome.skipped.is_empty(), "{outcome:?}");
    assert_eq!(outcome.retry_scheduled.len(), 1);
    let retry = &outcome.retry_scheduled[0];
    assert_eq!(retry.task_id, "task");
    assert_eq!(retry.attempt_id, attempt);
    assert!(retry.backoff_until_unix_secs > 60);
    phase(f, "pr_complete");
    assert!(scheduling::ensure_dispatch_capacity(&f.store).is_ok());
    assert!(scheduling::pending_attempts(&f.store).unwrap().is_empty());
    let shown: serde_json::Value = serde_json::from_str(
        &luthor::cli::execute(&f.config.state_root, &["show".into(), "task".into()]).unwrap(),
    )
    .unwrap();
    assert_eq!(shown["verified_pr"]["attempt_id"], attempt);
    for kind in ["pr_repair_result", "verified_open_pr", "exit_pr_lookup"] {
        assert!(
            journal::evidence_payloads(&f.store, "task", attempt, kind)
                .unwrap()
                .is_empty(),
            "fabricated {kind}"
        );
    }
    retry.backoff_until_unix_secs
}

#[test]
fn typed_transient_repair_schedules_one_bounded_relaunch_after_restart() {
    let mut f = Fixture::new();
    bounded(&mut f, 3); // initial + first repair + one retry
    let first = f.repair().unwrap().remove(0);
    fail(&mut f, &first, "model-transient-server", 5);
    assert!(matches!(
        reconcile(&mut f, &first.attempt_id).unwrap(),
        Reconciliation::Held { .. }
    ));
    let outcome = f.repair_outcome();
    let deadline = assert_scheduled(&f, &outcome, &first.attempt_id);
    drop(f.store);
    f.store = StateStore::open(&f.config.state_root, 1).unwrap();
    let outcome = f.repair_outcome();
    assert_eq!(assert_scheduled(&f, &outcome, &first.attempt_id), deadline);
    assert!(matches!(
        reconcile(&mut f, &first.attempt_id).unwrap(),
        Reconciliation::Completed { .. }
    ));
    elapsed(&f, &first.attempt_id);
    let mut result = f.repair_outcome();
    assert_eq!(
        result.launched.len(),
        1,
        "{result:?}; phase {:?}",
        task_records::task_phase(&f.store, "task").unwrap()
    );
    let next = result.launched.remove(0);
    assert_ne!(next.attempt_id, first.attempt_id);
    assert_eq!(next.session_id, first.session_id);
    assert_eq!(next.worktree, first.worktree);
    assert_eq!(next.worker_build, first.worker_build);
    assert!(f.repair().unwrap().is_empty());
    fail(&mut f, &next, "model-transient-server", 5);
    reconcile(&mut f, &next.attempt_id).unwrap();
    assert!(f.repair_outcome().retry_scheduled.is_empty());
    phase(&f, "held");
    assert_eq!(
        db(&f)
            .query_row(
                "SELECT COUNT(*) FROM attempts WHERE task_id='task'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        3
    );
}

#[test]
fn retry_repair_can_complete_without_fabricating_the_failed_attempts_result() {
    let mut f = Fixture::new();
    bounded(&mut f, 4);
    let first = f.repair().unwrap().remove(0);
    fail(&mut f, &first, "turn-time-exhausted", 6);
    reconcile(&mut f, &first.attempt_id).unwrap();
    let outcome = f.repair_outcome();
    assert_scheduled(&f, &outcome, &first.attempt_id);
    elapsed(&f, &first.attempt_id);
    let mut result = f.repair_outcome();
    assert_eq!(
        result.launched.len(),
        1,
        "{result:?}; phase {:?}",
        task_records::task_phase(&f.store, "task").unwrap()
    );
    let next = result.launched.remove(0);
    f.run(&next, "repaired");
    assert!(matches!(
        f.reconcile(&next.attempt_id),
        Reconciliation::Completed { .. }
    ));
    phase(&f, "pr_complete");
    assert!(
        journal::evidence_payloads(&f.store, "task", &first.attempt_id, "pr_repair_result")
            .unwrap()
            .is_empty()
    );
    f.prs.conflict = false;
    assert!(f.repair().unwrap().is_empty());
}

#[test]
fn pre_upgrade_result_unavailable_hold_restores_using_verified_original_receipt() {
    let mut f = Fixture::new();
    bounded(&mut f, 3);
    let plan = f.repair().unwrap().remove(0);
    fail(&mut f, &plan, "model-transient-server", 5);
    // The old coordinator held after receipt accounting, without classifying stdout.
    assert!(matches!(
        luthor::supervisor::reconcile_attempt(&mut f.store, "task", &plan.attempt_id).unwrap(),
        Reconciliation::Completed { .. }
    ));
    journal::record_evidence(
        &mut f.store,
        "task",
        Some(&plan.attempt_id),
        "pr_repair_blocked",
        "PR repair result unavailable or invalid",
    )
    .unwrap();
    task_records::hold_task(
        &mut f.store,
        "task",
        "PR repair result unavailable or invalid",
    )
    .unwrap();
    drop(f.store);
    f.store = StateStore::open(&f.config.state_root, 1).unwrap();
    let outcome = f.repair_outcome();
    assert_scheduled(&f, &outcome, &plan.attempt_id);
    elapsed(&f, &plan.attempt_id);
    assert_eq!(f.repair().unwrap().len(), 1);
}

#[test]
fn untyped_malformed_wrong_exit_and_signal_repair_failures_stay_held() {
    for scenario in [
        "untyped",
        "malformed",
        "wrong_exit",
        "signal",
        "wrong_session",
        "terminal",
        "disabled_class",
        "stop",
    ] {
        let mut f = Fixture::new();
        bounded(&mut f, 4);
        let plan = f.repair().unwrap().remove(0);
        match scenario {
            "untyped" => fail(&mut f, &plan, "model", 5),
            "malformed" => run_script(&mut f, &plan, "#!/bin/sh\necho not-json\nexit 5\n"),
            "wrong_exit" => fail(&mut f, &plan, "model-transient-server", 6),
            "signal" => run_script(&mut f, &plan, "#!/bin/sh\nkill -TERM $$\n"),
            "wrong_session" => run_script(
                &mut f,
                &plan,
                "#!/bin/sh\necho '{\"status\":\"error\",\"session_id\":\"other\",\"error\":{\"code\":\"model-transient-server\",\"message\":\"error\"}}'\nexit 5\n",
            ),
            "terminal" => run_script(
                &mut f,
                &plan,
                "#!/bin/sh\necho '{\"status\":\"error\",\"session_id\":\"task\",\"error\":{\"code\":\"model-transient-server\",\"message\":\"error\",\"terminal_outcome\":\"output_truncated\"}}'\nexit 5\n",
            ),
            "disabled_class" => fail(&mut f, &plan, "model-rate-limit", 5),
            "stop" => {
                journal::record_stop_intent(&mut f.store, "task", &plan.attempt_id).unwrap();
                fail(&mut f, &plan, "model-transient-server", 5);
            }
            _ => unreachable!(),
        }
        let result = reconcile(&mut f, &plan.attempt_id);
        if scenario == "disabled_class" {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err(), "{scenario}: {result:?}");
        }
        assert!(f.repair_outcome().retry_scheduled.is_empty(), "{scenario}");
        phase(&f, "held");
        assert!(f.repair().unwrap().is_empty(), "{scenario}");
    }
}

#[test]
fn exhausted_total_attempts_and_disabled_policy_stay_held() {
    for disabled in [false, true] {
        let mut f = Fixture::new();
        bounded(&mut f, if disabled { 4 } else { 2 });
        let plan = f.repair().unwrap().remove(0);
        fail(&mut f, &plan, "model-transient-server", 5);
        reconcile(&mut f, &plan.attempt_id).unwrap();
        if disabled {
            f.config.retry_policy = RetryPolicy::Disabled {};
        }
        assert!(f.repair_outcome().retry_scheduled.is_empty());
        phase(&f, "held");
        assert!(f.repair().unwrap().is_empty());
    }
}

#[test]
fn typed_failure_retries_count_toward_the_existing_repair_limit() {
    let mut f = Fixture::new();
    bounded(&mut f, 10);
    for index in 0..MAX_PR_REPAIRS {
        let plan = f.repair().unwrap().remove(0);
        fail(&mut f, &plan, "model-transient-server", 5);
        reconcile(&mut f, &plan.attempt_id).unwrap();
        let outcome = f.repair_outcome();
        if index + 1 < MAX_PR_REPAIRS {
            assert_scheduled(&f, &outcome, &plan.attempt_id);
            elapsed(&f, &plan.attempt_id);
        } else {
            assert!(outcome.retry_scheduled.is_empty());
            phase(&f, "held");
        }
    }
    assert!(f.repair().unwrap().is_empty());
}

#[test]
fn failed_repair_reauthorizes_the_fresh_pr_head_proved_by_the_same_worktree() {
    let mut f = Fixture::new();
    bounded(&mut f, 3);
    let plan = f.repair().unwrap().remove(0);
    let envelope = serde_json::json!({"status":"error", "session_id":plan.session_id,
        "error":{"code":"model-transient-server","message":"error after committing"}});
    run_script(
        &mut f,
        &plan,
        &format!(
            "#!/bin/sh\ngit commit --allow-empty -m 'repair progress' >/dev/null || exit 99\nprintf '%s\\n' '{envelope}'\nexit 5\n"
        ),
    );
    let output = Command::new("git")
        .arg("-C")
        .arg(&plan.worktree)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());
    f.prs.head = String::from_utf8(output.stdout).unwrap().trim().into();
    reconcile(&mut f, &plan.attempt_id).unwrap();
    let outcome = f.repair_outcome();
    assert_scheduled(&f, &outcome, &plan.attempt_id);
    let proofs =
        journal::evidence_payloads(&f.store, "task", &plan.attempt_id, "pr_head_verified").unwrap();
    let proof: serde_json::Value = serde_json::from_str(&proofs[0]).unwrap();
    assert_eq!(proof["head_commit_sha"], f.prs.head);
    elapsed(&f, &plan.attempt_id);
    let result = f.repair_outcome();
    assert_eq!(result.launched.len(), 1, "{result:?}");
    assert_eq!(result.launched[0].expected_worktree.head, f.prs.head);
}
