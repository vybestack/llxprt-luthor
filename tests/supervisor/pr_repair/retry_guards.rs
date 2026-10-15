//! Retry restoration must retain every existing launch-safety boundary.
use super::typed_retry::{bounded, db, fail, phase, reconcile};
use super::*;

#[test]
fn missing_receipt_or_busy_owner_retains_hold_until_original_proof_returns() {
    for busy in [false, true] {
        let mut f = Fixture::new();
        bounded(&mut f, 4);
        let plan = f.repair().unwrap().remove(0);
        fail(&mut f, &plan, "model-transient-server", 5);
        reconcile(&mut f, &plan.attempt_id).unwrap();
        let path = f
            .store
            .root()
            .join(format!("attempts/{}.receipt.json", plan.attempt_id));
        let bytes = fs::read(&path).unwrap();
        let permissions = fs::metadata(&path).unwrap().permissions();
        let owner = if busy {
            Some(luthor::WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap())
        } else {
            fs::remove_file(&path).unwrap();
            None
        };
        let outcome = f.repair_outcome();
        assert!(outcome.retry_scheduled.is_empty());
        assert!(outcome.launched.is_empty());
        assert_eq!(outcome.skipped.len(), 1, "{outcome:?}");
        phase(&f, "held");
        drop(owner);
        if !busy {
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(&path, permissions).unwrap();
        }
        drop(f.store);
        f.store = StateStore::open(&f.config.state_root, 1).unwrap();
        let restored = f.repair_outcome();
        assert_eq!(restored.retry_scheduled.len(), 1, "{restored:?}");
        phase(&f, "pr_complete");
    }
}

#[test]
fn retry_restoration_never_bypasses_claim_pr_local_tip_or_session_guards() {
    for scenario in [
        "claim",
        "pr_identity",
        "local_tip",
        "semantic",
        "session",
        "reserved",
        "unfinished",
    ] {
        let mut f = Fixture::new();
        bounded(&mut f, 4);
        let plan = f.repair().unwrap().remove(0);
        fail(&mut f, &plan, "model-transient-server", 5);
        reconcile(&mut f, &plan.attempt_id).unwrap();
        match scenario {
            "claim" => f.projects.1 = 0,
            "pr_identity" => f.prs.wrong_pr_id = true,
            "local_tip" => git(
                &plan.worktree,
                &["commit", "--allow-empty", "-m", "external local tip"],
            ),
            "semantic" => {
                journal::record_evidence(
                    &mut f.store,
                    "task",
                    Some(&plan.attempt_id),
                    "pr_repair_result",
                    &serde_json::json!({"attempt_id":plan.attempt_id,"status":"semantic_conflict"})
                        .to_string(),
                )
                .unwrap();
            }
            "session" => {
                journal::record_evidence(
                    &mut f.store,
                    "task",
                    Some(&plan.attempt_id),
                    "session_incompatible",
                    "incompatible",
                )
                .unwrap();
            }
            "reserved" => {
                db(&f)
                    .execute(
                        "UPDATE reservations SET status='reserved' WHERE attempt_id=?1",
                        [&plan.attempt_id],
                    )
                    .unwrap();
            }
            "unfinished" => {
                db(&f)
                    .execute(
                        "UPDATE attempts SET lifecycle='launch_intended',outcome=NULL WHERE id=?1",
                        [&plan.attempt_id],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let outcome = f.repair_outcome();
        assert!(
            outcome.retry_scheduled.is_empty(),
            "{scenario}: {outcome:?}"
        );
        assert!(outcome.launched.is_empty(), "{scenario}");
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("held"),
            "{scenario}"
        );
    }
}

#[test]
fn valid_semantic_report_wins_even_if_worker_later_emits_a_typed_error() {
    let mut f = Fixture::new();
    bounded(&mut f, 4);
    let plan = f.repair().unwrap().remove(0);
    super::typed_retry::run_script(
        &mut f,
        &plan,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' '{{\"attempt_id\":\"{}\",\"status\":\"semantic_conflict\"}}' > {REPAIR_RESULT_FILE}\necho '{{\"status\":\"error\",\"session_id\":\"task\",\"error\":{{\"code\":\"model-transient-server\",\"message\":\"error\"}}}}'\nexit 5\n",
            plan.attempt_id
        ),
    );
    assert!(matches!(
        reconcile(&mut f, &plan.attempt_id).unwrap(),
        Reconciliation::Held { .. }
    ));
    assert!(f.repair_outcome().retry_scheduled.is_empty());
    phase(&f, "held");
    assert_eq!(
        task_records::held_reason(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("semantic PR conflict requires a person")
    );
}

#[test]
fn scheduled_repairs_do_not_release_capacity_with_corrupt_or_non_natural_exit_evidence() {
    for scenario in [
        "signal",
        "stop",
        "wrong_attempt",
        "malformed",
        "duplicate_schedule",
    ] {
        let mut f = Fixture::new();
        bounded(&mut f, 3);
        let plan = f.repair().unwrap().remove(0);
        fail(&mut f, &plan, "model-transient-server", 5);
        reconcile(&mut f, &plan.attempt_id).unwrap();
        assert_eq!(f.repair_outcome().retry_scheduled.len(), 1);
        assert!(scheduling::ensure_dispatch_capacity(&f.store).is_ok());
        let db = db(&f);
        match scenario {
            "signal" => {
                db.execute("UPDATE evidence SET payload=json_set(payload,'$.signal',15) WHERE attempt_id=?1 AND kind='attempt_exit'", [&plan.attempt_id]).unwrap();
            }
            "stop" => {
                db.execute("UPDATE evidence SET payload=json_set(payload,'$.stop_signals',json('[15]')) WHERE attempt_id=?1 AND kind='attempt_exit'", [&plan.attempt_id]).unwrap();
            }
            "wrong_attempt" => {
                db.execute("UPDATE evidence SET payload=json_set(payload,'$.attempt_id','wrong') WHERE attempt_id=?1 AND kind='attempt_exit'", [&plan.attempt_id]).unwrap();
            }
            "malformed" => {
                db.execute("UPDATE evidence SET payload='not-json' WHERE attempt_id=?1 AND kind='attempt_exit'", [&plan.attempt_id]).unwrap();
            }
            "duplicate_schedule" => {
                db.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,'not-json' FROM evidence WHERE attempt_id=?1 AND kind='pr_repair_retry_scheduled'", [&plan.attempt_id]).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            scheduling::ensure_dispatch_capacity(&f.store).is_err(),
            "{scenario}"
        );
    }
}

#[test]
fn scheduled_repair_rechecks_the_original_typed_stdout_before_relaunch() {
    let mut f = Fixture::new();
    bounded(&mut f, 3);
    let plan = f.repair().unwrap().remove(0);
    fail(&mut f, &plan, "model-transient-server", 5);
    reconcile(&mut f, &plan.attempt_id).unwrap();
    assert_eq!(f.repair_outcome().retry_scheduled.len(), 1);
    let stdout = f
        .store
        .root()
        .join(format!("attempts/{}.stdout.log", plan.attempt_id));
    let original = fs::read(&stdout).unwrap();
    let replacement = String::from_utf8(original)
        .unwrap()
        .replace("model-transient-server", "model-permanent-foobar");
    fs::write(stdout, replacement).unwrap();
    let result = f.repair_outcome();
    assert!(result.retry_scheduled.is_empty());
    assert!(result.launched.is_empty());
    assert_eq!(result.held.len(), 1, "{result:?}");
    phase(&f, "held");
}
