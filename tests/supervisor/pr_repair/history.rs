//! Issue 51: finished tasks selected under an older configuration.
use super::*;
use luthor::{
    coordinator::{RestoreReport, restore_wrongly_held_prs},
    pr_repair::UNSPECIFIC_REPAIR_HOLD_REASON,
};

const DRIFT: &str = "selection config differs: min_free_space_bytes, initial.args";

/// A task selected before `min_free_space_bytes` existed and with other worker
/// arguments, as every task from before the first deployment was.
fn drifted() -> Fixture {
    let mut f = Fixture::new();
    f.config.min_free_space_bytes = 4096;
    f.config.initial.args = vec!["--changed".into()];
    f
}

fn evidence_rows(f: &Fixture) -> Vec<String> {
    journal::evidence_kinds(&f.store, "task").unwrap()
}

/// What GitHub returns for the saved PR once it is finished.
struct FinishedPr {
    detail: serde_json::Value,
}
impl FinishedPr {
    fn merged(f: &mut Fixture) -> Self {
        let mut detail = f.prs.detail("org/code", 42).unwrap();
        detail["state"] = serde_json::json!("closed");
        detail["merged"] = serde_json::json!(true);
        detail["closed_at"] = serde_json::json!("2026-02-01T00:00:00Z");
        detail["merged_at"] = serde_json::json!("2026-02-01T00:00:00Z");
        f.prs.inner.matching = None;
        Self { detail }
    }
    fn closed(f: &mut Fixture) -> Self {
        let mut finished = Self::merged(f);
        finished.detail["merged"] = serde_json::json!(false);
        finished.detail["merged_at"] = serde_json::Value::Null;
        finished
    }
}
impl PullRequestReader for FinishedPr {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        Ok("operator".into())
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<serde_json::Value>, LookupError> {
        Ok(vec![])
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<serde_json::Value, LookupError> {
        Ok(self.detail.clone())
    }
    fn repository_identity(&mut self, _: &str) -> Result<u64, LookupError> {
        unreachable!("restoration compares saved identities only")
    }
}

fn hold_with_unspecific_reason(f: &mut Fixture) {
    task_records::hold_task(&mut f.store, "task", UNSPECIFIC_REPAIR_HOLD_REASON).unwrap();
}

fn restore(f: &mut Fixture, prs: &mut FinishedPr) -> RestoreReport {
    restore_wrongly_held_prs(&mut f.store, prs).unwrap()
}

fn restoration_proofs(f: &Fixture) -> Vec<String> {
    let connection = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    let mut statement = connection
        .prepare("SELECT payload FROM evidence WHERE task_id='task' AND kind='pr_hold_restored' ORDER BY sequence")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn phase(f: &Fixture) -> Option<String> {
    task_records::task_phase(&f.store, "task").unwrap()
}

#[test]
fn merged_pr_with_different_selection_config_is_neither_held_nor_inspected() {
    let mut f = drifted();
    FinishedPr::merged(&mut f);
    let before = evidence_rows(&f);
    let reason_before = task_records::held_reason(&f.store, "task").unwrap();
    let outcome = f.repair_outcome();
    assert!(outcome.launched.is_empty());
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    assert!(outcome.held.is_empty());
    assert_eq!(phase(&f).as_deref(), Some("pr_complete"));
    assert_eq!(
        task_records::held_reason(&f.store, "task").unwrap(),
        reason_before
    );
    assert_eq!(evidence_rows(&f), before);
}

#[test]
fn open_pr_with_different_selection_config_is_skipped_with_its_reason() {
    let mut f = drifted();
    let before = evidence_rows(&f);
    let outcome = f.repair_outcome();
    assert!(outcome.launched.is_empty() && outcome.held.is_empty());
    assert_eq!(
        outcome.skipped,
        [luthor::coordinator::TaskNote {
            task_id: "task".into(),
            reason: DRIFT.into()
        }]
    );
    assert_eq!(phase(&f).as_deref(), Some("pr_complete"));
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert_eq!(evidence_rows(&f), before);
    assert!(scheduling::ensure_dispatch_capacity(&f.store).is_ok());
}

#[test]
fn skipped_task_is_repaired_again_once_the_configuration_matches() {
    let mut f = Fixture::new();
    let selection_time = f.config.clone();
    f.config.min_free_space_bytes = 4096;
    f.config.initial.args = vec!["--changed".into()];
    assert_eq!(f.repair_outcome().skipped.len(), 1);
    f.config = selection_time;
    let outcome = f.repair_outcome();
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    assert_eq!(outcome.launched.len(), 1);
}

#[test]
fn unreadable_pr_lookup_skips_without_holding() {
    let mut f = Fixture::new();
    f.prs.inner.fail = true;
    let before = evidence_rows(&f);
    let outcome = f.repair_outcome();
    assert!(outcome.held.is_empty() && outcome.launched.is_empty());
    assert_eq!(outcome.skipped.len(), 1);
    assert!(
        outcome.skipped[0]
            .reason
            .starts_with("PR lookup unavailable")
    );
    assert_eq!(phase(&f).as_deref(), Some("pr_complete"));
    assert_eq!(evidence_rows(&f), before);
}

#[test]
fn safety_conflict_on_an_open_pr_holds_with_a_specific_reason() {
    let mut f = Fixture::new();
    f.prs.inner.author = Some("other".into());
    let held = f.repair().unwrap_err();
    assert_eq!(held.len(), 1);
    assert_eq!(phase(&f).as_deref(), Some("held"));
    let reason = task_records::held_reason(&f.store, "task")
        .unwrap()
        .unwrap();
    assert_eq!(reason, held[0].reason);
    assert!(reason.starts_with("PR repair held: "), "{reason}");
    assert_ne!(reason, UNSPECIFIC_REPAIR_HOLD_REASON);
}

#[test]
fn restoring_a_wrongly_held_task_with_a_merged_pr_frees_capacity() {
    let mut f = drifted();
    let mut finished = FinishedPr::merged(&mut f);
    hold_with_unspecific_reason(&mut f);
    assert!(matches!(
        scheduling::ensure_dispatch_capacity(&f.store),
        Err(luthor::state::StateError::Capacity { .. })
    ));
    let rows_before = evidence_rows(&f);
    let holds_before = rows_before.iter().filter(|k| *k == "held_reason").count();
    let report = restore(&mut f, &mut finished);
    assert_eq!(report.restored, ["task"]);
    assert!(report.retained.is_empty());
    assert_eq!(phase(&f).as_deref(), Some("pr_complete"));
    assert!(scheduling::ensure_dispatch_capacity(&f.store).is_ok());
    let rows_after = evidence_rows(&f);
    assert_eq!(rows_after[..rows_before.len()], rows_before[..]);
    assert_eq!(rows_after[rows_before.len()..], ["pr_hold_restored"]);
    let holds_after = rows_after.iter().filter(|k| *k == "held_reason").count();
    assert_eq!(
        holds_after, holds_before,
        "hold evidence is kept, never deleted"
    );
    let outcome = f.repair_outcome();
    assert!(outcome.skipped.is_empty() && outcome.held.is_empty());
    assert_eq!(phase(&f).as_deref(), Some("pr_complete"));
    assert!(restore(&mut f, &mut finished).restored.is_empty());
}

#[test]
fn restoration_records_the_terminal_proof_it_relied_on() {
    let mut f = Fixture::new();
    let mut finished = FinishedPr::closed(&mut f);
    hold_with_unspecific_reason(&mut f);
    assert_eq!(restore(&mut f, &mut finished).restored, ["task"]);
    let proofs = restoration_proofs(&f);
    assert_eq!(proofs.len(), 1);
    let proof: serde_json::Value = serde_json::from_str(&proofs[0]).unwrap();
    assert_eq!(proof["restored_from_reason"], UNSPECIFIC_REPAIR_HOLD_REASON);
    assert_eq!(proof["terminal_pr"]["state"], "closed");
    assert_eq!(proof["terminal_pr"]["number"], 42);
    assert!(proof["observed_at_unix_secs"].as_u64().unwrap() > 0);
}

#[test]
fn restoration_leaves_everything_it_cannot_prove_finished_held() {
    for scenario in [
        "still_open",
        "other_reason",
        "identity_changed",
        "unreadable",
        "live_reservation",
    ] {
        let mut f = Fixture::new();
        let mut finished = FinishedPr::merged(&mut f);
        hold_with_unspecific_reason(&mut f);
        match scenario {
            "still_open" => {
                finished.detail["state"] = serde_json::json!("open");
                finished.detail["merged"] = serde_json::json!(false);
            }
            "other_reason" => {
                task_records::hold_task(&mut f.store, "task", "PR repair held: claim").unwrap()
            }
            "identity_changed" => finished.detail["id"] = serde_json::json!(1),
            "unreadable" => finished.detail = serde_json::Value::Null,
            "live_reservation" => {
                scheduling::reserve(&mut f.store, "task", "attempt-live").unwrap()
            }
            _ => unreachable!(),
        }
        let report = restore(&mut f, &mut finished);
        assert!(report.restored.is_empty(), "{scenario}");
        assert_eq!(phase(&f).as_deref(), Some("held"), "{scenario}");
        let expected = usize::from(!matches!(scenario, "other_reason" | "live_reservation"));
        assert_eq!(report.retained.len(), expected, "{scenario}");
        assert!(!evidence_rows(&f).contains(&"pr_hold_restored".into()));
    }
}
