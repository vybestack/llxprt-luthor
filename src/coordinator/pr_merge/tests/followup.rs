use super::*;
use crate::state::merge_followup;

#[test]
fn nondefault_base_merge_closes_tracker_with_durable_evidence_and_no_duplicate_write() {
    let mut f = Fixture::new();
    assert_eq!(f.run().unwrap().merged, ["task"]);
    assert_eq!(f.prs.tracker_writes, 1);
    let proof = merge_followup::closure(&f.store, "task").unwrap().unwrap();
    assert!(proof.closed_by_luthor);
    assert_eq!(proof.issue.target.node_id, "ISSUE");
    assert_eq!(proof.merge, f.evidence());
    assert!(proof.issue.closed_at.is_some());
    assert!(f.run().unwrap().merged.is_empty());
    assert_eq!(f.prs.tracker_writes, 1);
}

#[test]
fn github_closed_tracker_is_recorded_without_a_write() {
    let mut f = Fixture::new();
    f.prs.tracker_closed = true;
    assert_eq!(f.run().unwrap().merged, ["task"]);
    assert_eq!(f.prs.tracker_writes, 0);
    assert!(
        !merge_followup::closure(&f.store, "task")
            .unwrap()
            .unwrap()
            .closed_by_luthor
    );
}

#[test]
fn tracker_close_failure_is_not_hidden_or_replayed_and_merge_proof_is_retained() {
    let mut f = Fixture::new();
    f.prs.fail_close = true;
    f.held();
    assert!(pr_merge::completed(&f.store, "task").unwrap());
    assert!(merge_followup::closure(&f.store, "task").unwrap().is_none());
    f.prs.fail_close = false;
    f.held();
    assert_eq!(f.prs.tracker_writes, 1);
    assert_eq!(f.prs.writes.len(), 1);
}

#[test]
fn interrupted_tracker_close_recovers_by_read_without_remerging_or_reclosing() {
    let mut f = Fixture::new();
    f.prs.fail_close_read = true;
    f.held();
    f.prs.fail_close_read = false;
    assert_eq!(f.run().unwrap().merged, ["task"]);
    assert_eq!(f.prs.tracker_writes, 1);
    assert_eq!(f.prs.writes.len(), 1);
}

#[test]
fn behind_green_pr_updates_once_then_waits_for_existing_lineage_verification() {
    let mut f = Fixture::new();
    f.prs.detail["mergeable_state"] = json!("behind");
    assert_eq!(f.run().unwrap().waiting.len(), 1);
    assert_eq!(f.prs.updates, 1);
    assert!(f.prs.writes.is_empty());
    assert!(pr_merge::authorization(&f.store, "task").unwrap().is_none());
    let intent = merge_followup::update(&f.store, "task", &"a".repeat(40))
        .unwrap()
        .unwrap();
    assert_eq!(intent.pr.head_commit_sha, "a".repeat(40));
    assert_eq!(f.run().unwrap().waiting.len(), 1);
    assert_eq!(f.prs.updates, 1);
    assert!(f.prs.writes.is_empty());
    // Simulate the existing strict claim/exit/local/base-lineage verifier's
    // recorded head. Checks for the OLD head can never authorize the new head.
    let mut verified = intent.pr;
    verified.head_commit_sha = "b".repeat(40);
    journal::record_evidence(
        &mut f.store,
        "task",
        Some("attempt"),
        "pr_head_verified",
        &serde_json::to_string(&verified).unwrap(),
    )
    .unwrap();
    f.prs.detail["mergeable_state"] = json!("clean");
    f.held();
    assert!(f.prs.writes.is_empty());
    f.prs.checks.health.head = "b".repeat(40);
    f.prs.checks.health.required_checks_pending = true;
    assert_eq!(f.run().unwrap().waiting.len(), 1);
    assert!(f.prs.writes.is_empty());
    f.prs.checks.health.required_checks_pending = false;
    assert_eq!(f.run().unwrap().merged, ["task"]);
    assert_eq!(f.evidence().merged_head, "b".repeat(40));
}

#[test]
fn uncertain_branch_update_is_not_replayed_and_conflicts_are_left_to_repair() {
    let mut f = Fixture::new();
    f.prs.detail["mergeable_state"] = json!("behind");
    f.prs.fail_write = true;
    f.held();
    f.prs.fail_write = false;
    f.held();
    assert_eq!(f.prs.updates, 1);
    f.prs.detail["mergeable_state"] = json!("dirty");
    f.prs.checks.health.conflicting = true;
    assert_eq!(f.run().unwrap().waiting.len(), 1);
    assert!(f.prs.writes.is_empty());
    assert_eq!(f.prs.updates, 1);
}

#[test]
fn disabled_policy_never_closes_tracker_or_updates_behind_branch() {
    let mut f = Fixture::new();
    f.config.mappings[0].merge = MergePolicy::disabled();
    f.prs.detail["mergeable_state"] = json!("behind");
    f.run().unwrap();
    assert_eq!(
        (f.prs.updates, f.prs.tracker_writes, f.prs.reads),
        (0, 0, 0)
    );
}
