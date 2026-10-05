use super::*;
use luthor::state::{journal, scheduling, task_records, worktree_records};

// Receipt publication precedes supervisor exit/reaping. Completion-claim tests
// need a quiescent fixture, not merely a receipt-ready one. Do not retry Held:
// an unexpected identity/evidence failure must remain a test failure.
pub(crate) fn completion_fixture() -> (tempfile::TempDir, Config, StateStore) {
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    fs::write(
        &plan.executable,
        "#!/bin/sh\nprintf 'worker stdout\\n'\nprintf 'worker stderr\\n' >&2\nexit 7\n",
    )
    .unwrap();
    let attempts = config.state_root.join("attempts");
    let _cleanup = FixtureGroupGuard(attempts.join("attempt-real.child.json"));
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor")))
        .unwrap_or_else(|error| {
            let stderr = fs::read_to_string(attempts.join("attempt-real.supervisor.log"));
            let supervisor_error = fs::read_to_string(attempts.join("attempt-real.supervisor-error.json"));
            panic!("fixture launch failed: {error}; stderr={stderr:?}; supervisor_error={supervisor_error:?}");
        });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !receipt_path(&config).exists() {
        let error = fs::read_to_string(attempts.join("attempt-real.supervisor-error.json")).ok();
        assert!(error.is_none(), "fixture supervisor failed: {error:?}");
        assert!(
            Instant::now() < deadline,
            "fixture exit receipt not published"
        );
        thread::sleep(Duration::from_millis(10));
    }
    wait_for_fixture_exit(&config, &store);
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(task_records::held_reason(&store, "task").unwrap().is_none());
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|k| k == "attempt_exit")
    );
    (dir, config, store)
}

pub(crate) fn wait_for_fixture_exit(config: &Config, store: &StateStore) {
    assert!(
        receipt_path(config).exists(),
        "fixture exit receipt missing"
    );
    let ready =
        journal::evidence_payloads(store, "task", "attempt-real", "supervisor_ready").unwrap();
    assert_eq!(ready.len(), 1);
    let ready = &ready[0];
    let supervisor: serde_json::Value = serde_json::from_str(ready).unwrap();
    let pid = supervisor["pid"].as_u64().unwrap();
    wait_for_process_and_group_absence(libc::pid_t::try_from(pid).unwrap());
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt_path(config)).unwrap()).unwrap();
    wait_for_process_and_group_absence(libc::pid_t::try_from(receipt.child_pid).unwrap());
}

fn matching_readers(store: &StateStore) -> (OtherProject, ExitPr) {
    let selection = task_records::selection_evidence(store, "task")
        .unwrap()
        .unwrap();
    let identity = worktree_records::worktree_record(store, "task")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let prs = ExitPr {
        matching: Some((selection.candidate.issue_url.clone(), identity.branch)),
        ..ExitPr::default()
    };
    (OtherProject(selection.candidate, 0), prs)
}

fn reconcile_claim(
    store: &mut StateStore,
    projects: &mut OtherProject,
    prs: &mut ExitPr,
) -> Reconciliation {
    luthor::coordinator::reconcile_with_pr(store, "task", "attempt-real", projects, prs).unwrap()
}

fn assert_recorded_claim_hold(
    store: &StateStore,
    config: &Config,
    projects: &OtherProject,
    prs: &ExitPr,
) {
    assert_eq!(
        task_records::held_reason(store, "task").unwrap().as_deref(),
        Some("completion claim changed")
    );
    assert_eq!(
        task_records::task_phase(store, "task").unwrap().as_deref(),
        Some("held")
    );
    let kinds = journal::evidence_kinds(store, "task").unwrap();
    assert!(!kinds.iter().any(|kind| kind == "verified_open_pr"));
    assert!(kinds.iter().any(|kind| kind == "attempt_exit"));
    let connection = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let outcome: Option<String> = connection
        .query_row(
            "SELECT outcome FROM attempts WHERE id='attempt-real'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome.as_deref(), Some("exit_code=Some(7);signal=None"));
    assert_eq!(scheduling::reservation_count(store).unwrap(), 0);
    assert!(scheduling::ensure_dispatch_capacity(store).is_err());
    assert_eq!(prs.reads, 1);
    assert_eq!(projects.1, 1);
}

pub(crate) fn assert_claim_hold(result: Reconciliation) {
    assert_eq!(
        result,
        Reconciliation::Held {
            reason: "completion claim changed".into()
        }
    );
}

fn assert_early_hold(
    store: &StateStore,
    result: Reconciliation,
    reason: &str,
    projects: &OtherProject,
    prs: &ExitPr,
) {
    assert_eq!(
        result,
        Reconciliation::Held {
            reason: reason.into()
        }
    );
    // Early process/evidence holds deliberately do not pretend a completion
    // claim was checked. They must retain capacity and never accept the PR.
    assert!(task_records::held_reason(store, "task").unwrap().is_none());
    assert_eq!(scheduling::reservation_count(store).unwrap(), 1);
    assert!(scheduling::ensure_dispatch_capacity(store).is_err());
    let kinds = journal::evidence_kinds(store, "task").unwrap();
    assert!(
        !kinds
            .iter()
            .any(|k| k == "attempt_exit" || k == "verified_open_pr")
    );
    assert_eq!(prs.reads, 0);
    assert_eq!(projects.1, 0);
}

struct TrackedChild(std::process::Child);
impl Drop for TrackedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn live_descendant_hold_precedes_and_then_records_stale_assignee_hold() {
    let (_dir, config, mut store) = completion_fixture();
    let mut child = TrackedChild(Command::new("/bin/sleep").arg("30").spawn().unwrap());
    let (boot_identity, start_identity) = test_process_identity(child.0.id());
    let tracked = serde_json::json!({
        "pid": child.0.id(), "boot_identity": boot_identity, "start_identity": start_identity,
    });
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "tracked_descendant",
        &tracked.to_string(),
    )
    .unwrap();
    let (mut projects, mut prs) = matching_readers(&store);
    let result = reconcile_claim(&mut store, &mut projects, &mut prs);
    assert_early_hold(
        &store,
        result,
        "tracked descendant is alive",
        &projects,
        &prs,
    );
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert_eq!(
        reconcile_claim(&mut store, &mut projects, &mut prs),
        Reconciliation::Held {
            reason: "completion claim changed".into()
        }
    );
    assert_recorded_claim_hold(&store, &config, &projects, &prs);
}

#[test]
fn invalid_receipt_hold_does_not_claim_completion_was_checked() {
    let (_dir, config, mut store) = completion_fixture();
    edit_receipt(&config, |receipt| {
        receipt.child_start_identity.push_str("-changed")
    });
    let (mut projects, mut prs) = matching_readers(&store);
    let result = reconcile_claim(&mut store, &mut projects, &mut prs);
    assert_early_hold(
        &store,
        result,
        "receipt identity or shape mismatch",
        &projects,
        &prs,
    );
}

#[test]
fn log_failure_hold_does_not_claim_completion_was_checked() {
    let (_dir, _config, mut store) = completion_fixture();
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "log_failure",
        "fixture drain failure",
    )
    .unwrap();
    let (mut projects, mut prs) = matching_readers(&store);
    let result = reconcile_claim(&mut store, &mut projects, &mut prs);
    assert_early_hold(&store, result, "log drain failed", &projects, &prs);
}

#[test]
fn duplicate_supervisor_evidence_fails_closed() {
    let (_dir, _config, mut store) = completion_fixture();
    let ready =
        journal::evidence_payloads(&store, "task", "attempt-real", "supervisor_ready").unwrap();
    let mut identity: serde_json::Value = serde_json::from_str(&ready[0]).unwrap();
    identity["start_identity"] = serde_json::json!("contradictory fixture identity");
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "supervisor_ready",
        &identity.to_string(),
    )
    .unwrap();
    let (mut projects, mut prs) = matching_readers(&store);
    let result = luthor::coordinator::reconcile_with_pr(
        &mut store,
        "task",
        "attempt-real",
        &mut projects,
        &mut prs,
    );
    assert!(matches!(
        result,
        Err(luthor::supervisor::SupervisorError::State(
            luthor::state::StateError::LaunchBlocked
        ))
    ));
    assert!(task_records::held_reason(&store, "task").unwrap().is_none());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    let kinds = journal::evidence_kinds(&store, "task").unwrap();
    assert!(
        !kinds
            .iter()
            .any(|k| k == "attempt_exit" || k == "verified_open_pr")
    );
    assert_eq!(prs.reads, 0);
    assert_eq!(projects.1, 0);
}
