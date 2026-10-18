//! Issue 114: stale-head checks cannot authorize a strict behind-base merge.
use super::*;
use crate::state::merge_followup;

#[test]
fn behind_with_pending_stale_checks_updates_without_reading_checks() {
    let mut f = Fixture::new();
    f.prs.detail["mergeable_state"] = json!("behind");
    f.prs.checks.health.required_checks_pending = true;
    f.prs.checks.summary[0].state = "PENDING".into();
    assert_updates_without_checks(&mut f);
}

#[test]
fn behind_with_failed_stale_checks_updates_without_reading_checks() {
    let mut f = Fixture::new();
    f.prs.detail["mergeable_state"] = json!("behind");
    f.prs.checks.health.failed_required_checks = vec!["CI".into()];
    f.prs.checks.summary[0].state = "FAILURE".into();
    assert_updates_without_checks(&mut f);
}

fn assert_updates_without_checks(f: &mut Fixture) {
    let report = f.run().unwrap();
    assert!(report.merged.is_empty());
    assert_eq!(
        report.waiting[0].reason,
        "base merge requested; await verified new head and checks"
    );
    assert_eq!(f.prs.updates, 1);
    assert_eq!(f.prs.check_reads, 0);
    assert!(f.prs.writes.is_empty());
    assert!(pr_merge::authorization(&f.store, "task").unwrap().is_none());
    let intent = merge_followup::update(&f.store, "task", &"a".repeat(40))
        .unwrap()
        .unwrap();
    assert_eq!(intent.pr.head_commit_sha, "a".repeat(40));
    let updates =
        journal::evidence_payloads(&f.store, "task", "attempt", "pr_branch_updated").unwrap();
    assert_eq!(updates, [serde_json::to_string(&intent).unwrap()]);
    // Even green checks for the updated head cannot replace repair verification.
    f.prs.checks.health.head = "b".repeat(40);
    f.prs.checks.health.required_checks_pending = false;
    f.prs.checks.health.failed_required_checks.clear();
    f.prs.checks.summary[0].state = "SUCCESS".into();
    assert_eq!(
        f.run().unwrap().waiting[0].reason,
        "base update head awaits existing PR repair verification"
    );
    assert_eq!((f.prs.updates, f.prs.check_reads), (1, 0));
    assert!(f.prs.writes.is_empty());
}

#[test]
fn behind_drafts_never_update_even_when_draft_merging_is_enabled() {
    for allowed in [false, true] {
        let mut f = Fixture::new();
        f.prs.detail["mergeable_state"] = json!("behind");
        f.prs.detail["draft"] = json!(true);
        if let MergePolicy::Enabled { allow_draft, .. } = &mut f.config.mappings[0].merge {
            *allow_draft = allowed;
        }
        assert_eq!(f.run().unwrap().waiting.len(), 1);
        assert_eq!(f.prs.updates, 0);
        assert!(f.prs.writes.is_empty());
        assert!(pr_merge::authorization(&f.store, "task").unwrap().is_none());
    }
}

#[test]
fn behind_identity_or_verified_head_drift_never_updates() {
    for pointer in [
        "/head/sha",
        "/head/ref",
        "/head/repo/full_name",
        "/base/ref",
        "/user/login",
        "/body",
    ] {
        let mut f = Fixture::new();
        f.prs.detail["mergeable_state"] = json!("behind");
        *f.prs.detail.pointer_mut(pointer).unwrap() = json!("changed");
        f.held();
        assert_eq!((f.prs.updates, f.prs.check_reads), (0, 0));
        assert!(f.prs.writes.is_empty());
        assert!(
            merge_followup::update(&f.store, "task", &"a".repeat(40))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn behind_requires_the_verified_authenticated_account_before_updating() {
    let mut f = Fixture::new();
    f.prs.detail["mergeable_state"] = json!("behind");
    f.prs.login = "other".into();
    f.held();
    assert_eq!((f.prs.updates, f.prs.check_reads), (0, 0));
    assert!(f.prs.writes.is_empty());
    assert!(
        merge_followup::update(&f.store, "task", &"a".repeat(40))
            .unwrap()
            .is_none()
    );
}

#[test]
fn behind_without_mergeability_never_updates_or_merges() {
    for mergeable in [json!(false), Value::Null] {
        let mut f = Fixture::new();
        f.prs.detail["mergeable_state"] = json!("behind");
        f.prs.detail["mergeable"] = mergeable;
        assert_eq!(f.run().unwrap().waiting.len(), 1);
        assert_eq!((f.prs.updates, f.prs.check_reads), (0, 1));
        assert!(f.prs.writes.is_empty());
    }
}

#[test]
fn not_behind_pending_or_failed_checks_still_block_without_updating() {
    for pending in [true, false] {
        let mut f = Fixture::new();
        f.prs.checks.health.required_checks_pending = pending;
        if !pending {
            f.prs.checks.health.failed_required_checks = vec!["CI".into()];
        }
        let report = f.run().unwrap();
        assert!(report.merged.is_empty());
        assert_eq!(report.waiting.len(), 1);
        assert_eq!((f.prs.updates, f.prs.check_reads), (0, 1));
        assert!(f.prs.writes.is_empty());
        assert!(pr_merge::authorization(&f.store, "task").unwrap().is_none());
    }
}

#[test]
fn conflicting_prs_stay_on_the_repair_path_without_branch_updates() {
    for pending in [false, true] {
        let mut f = Fixture::new();
        f.prs.detail["mergeable_state"] = json!("dirty");
        f.prs.detail["mergeable"] = json!(false);
        f.prs.checks.health.conflicting = true;
        f.prs.checks.health.required_checks_pending = pending;
        let report = f.run().unwrap();
        assert!(report.merged.is_empty());
        assert_eq!(report.waiting.len(), 1);
        assert_eq!((f.prs.updates, f.prs.check_reads), (0, 1));
        assert!(f.prs.writes.is_empty());
        assert!(pr_merge::authorization(&f.store, "task").unwrap().is_none());
        assert!(
            merge_followup::update(&f.store, "task", &"a".repeat(40))
                .unwrap()
                .is_none()
        );
    }
}
