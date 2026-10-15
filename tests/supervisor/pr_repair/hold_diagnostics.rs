//! Issue 83: diagnostics identify the failed safety check without weakening it.
use super::*;
use luthor::coordinator::restore_open_held_prs;

fn assert_hold(f: &Fixture, outcome: &luthor::coordinator::RepairOutcome, code: &str) {
    assert!(outcome.launched.is_empty() && outcome.skipped.is_empty());
    assert_eq!(outcome.held.len(), 1);
    let reason = &outcome.held[0].reason;
    assert_eq!(
        reason.split(';').next().unwrap(),
        format!("PR repair held: {code}")
    );
    assert_eq!(
        task_records::held_reason(&f.store, "task")
            .unwrap()
            .as_ref(),
        Some(reason)
    );
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("held")
    );
    // This is the shape copied into the execute cycle's pr_repair.held field.
    let report = luthor::coordinator::PrRepairReport {
        held: outcome.held.clone(),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(report).unwrap()["held"][0]["reason"],
        *reason
    );
}

fn restore(f: &mut Fixture) -> luthor::coordinator::RestoreReport {
    restore_open_held_prs(&mut f.store, &mut f.projects, &mut f.prs).unwrap()
}

#[test]
fn safety_check_failures_have_distinct_persisted_and_reported_codes() {
    for (scenario, code) in [
        ("receipt_missing", "receipt-unverified"),
        ("receipt_invalid", "receipt-unverified"),
        ("process", "process-evidence-unverified"),
        ("plan", "attempt-evidence-unverified"),
        ("owner", "owner-unavailable"),
        ("protocol", "owner-protocol-unverified"),
        ("pr", "pr-identity-unverified"),
    ] {
        let mut f = Fixture::new();
        let attempts = f.config.state_root.join("attempts");
        match scenario {
            "receipt_missing" => fs::remove_file(execution::receipt_path(&f.config)).unwrap(),
            "receipt_invalid" => {
                fs::write(execution::receipt_path(&f.config), "secret-invalid-receipt").unwrap()
            }
            "process" => fs::remove_file(attempts.join("attempt-real.child.json")).unwrap(),
            "plan" => fs::remove_file(attempts.join("attempt-real.plan.json")).unwrap(),
            "owner" => fs::remove_file(f.store.root().join("worktree-task.lock")).unwrap(),
            "protocol" => {
                let db =
                    rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
                db.execute("UPDATE evidence SET payload='secret-invalid-protocol' WHERE kind='worktree_owner_protocol'", []).unwrap();
            }
            "pr" => f.prs.wrong_pr_id = true,
            _ => unreachable!(),
        }
        let outcome = f.repair_outcome();
        assert_hold(&f, &outcome, code);
        assert!(!outcome.held[0].reason.contains("secret"));
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
        let report = restore(&mut f);
        assert!(report.restored.is_empty(), "{scenario}");
        assert_eq!(report.retained.len(), 1, "{scenario}");
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("held")
        );
    }
}

#[test]
fn busy_owner_tells_operator_to_wait_and_restore_retries_after_release() {
    let mut f = Fixture::new();
    let owner = luthor::WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap();
    let outcome = f.repair_outcome();
    assert_hold(&f, &outcome, "owner-lock-busy");
    assert!(
        outcome.held[0]
            .reason
            .contains("restore will retry next cycle")
    );
    let retained = restore(&mut f);
    assert!(retained.restored.is_empty());
    assert!(
        retained.retained[0]
            .reason
            .contains("owner-lock-busy; restore will retry")
    );
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    drop(owner);
    drop(f.store);
    f.store = StateStore::open(&f.config.state_root, 1).unwrap();
    assert_eq!(restore(&mut f).restored, ["task"]);
    assert_eq!(f.repair().unwrap().len(), 1);
}

#[test]
fn receipt_hold_restores_only_after_the_original_receipt_verifies_again() {
    let mut f = Fixture::new();
    let path = execution::receipt_path(&f.config);
    let receipt = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let outcome = f.repair_outcome();
    assert_hold(&f, &outcome, "receipt-unverified");
    assert!(restore(&mut f).restored.is_empty());
    fs::write(&path, receipt).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(restore(&mut f).restored, ["task"]);
    assert_eq!(f.repair().unwrap().len(), 1);
}

#[test]
fn health_identity_mismatch_is_named_without_launching_a_worker() {
    let mut f = Fixture::new();
    f.prs.wrong_health_head = true;
    let outcome = f.repair_outcome();
    assert_hold(&f, &outcome, "pr-identity-unverified");
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}

#[test]
fn new_restore_reasons_do_not_bypass_live_attempt_or_semantic_session_guards() {
    for blocker in ["reservation", "unfinished", "semantic", "session"] {
        let mut f = Fixture::new();
        let owner = luthor::WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap();
        let outcome = f.repair_outcome();
        assert_hold(&f, &outcome, "owner-lock-busy");
        drop(owner);
        let db = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
        match blocker {
            "reservation" => {
                scheduling::reserve(&mut f.store, "task", "attempt-live").unwrap();
            }
            "unfinished" => {
                db.execute(
                    "UPDATE attempts SET lifecycle='running',outcome=NULL WHERE id='attempt-real'",
                    [],
                )
                .unwrap();
            }
            "semantic" | "session" => {
                let kind = if blocker == "semantic" {
                    "pr_repair_blocked"
                } else {
                    "session_incompatible"
                };
                journal::record_evidence(
                    &mut f.store,
                    "task",
                    Some("attempt-real"),
                    kind,
                    "requires person",
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(restore(&mut f).restored.is_empty(), "{blocker}");
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("held")
        );
    }
}

#[test]
fn legacy_worker_build_hold_never_records_the_upstream_payload() {
    let mut f = Fixture::new();
    crate::pin_adoption_fixture::legacy(&f.config, &f.store);
    let db = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    let before: i64 = db
        .query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM evidence",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let outcome = f.repair_outcome();
    assert_hold(&f, &outcome, "worker-build-held");
    let reasons: Vec<String> = db.prepare("SELECT payload FROM evidence WHERE kind='held_reason' AND sequence>?1 ORDER BY sequence").unwrap()
        .query_map([before], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(reasons, ["PR repair held: worker-build-held"]);
    assert!(restore(&mut f).restored.is_empty());
}
