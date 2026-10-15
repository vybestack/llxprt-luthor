use super::*;
use serde_json::json;
mod fixture;
use fixture::Fixture;

#[test]
fn disabled_policy_leaves_history_unchanged_and_never_reads_github() {
    let mut f = Fixture::new();
    f.config.mappings[0].merge = MergePolicy::Disabled {};
    let before = f.store.connection.total_changes();
    assert!(f.run().unwrap().merged.is_empty());
    assert_eq!(f.store.connection.total_changes(), before);
    assert_eq!(f.prs.reads, 0);
    assert!(f.prs.writes.is_empty());
}

#[test]
fn clean_verified_head_merges_once_and_persists_commit_head_and_checks() {
    let mut f = Fixture::new();
    assert_eq!(f.run().unwrap().merged, ["task"]);
    let evidence = f.evidence();
    assert_eq!(evidence.merge_commit, "c".repeat(40));
    assert_eq!(evidence.merged_head, "a".repeat(40));
    assert_eq!(evidence.authorization.checks.summary[0].name, "CI");
    assert_eq!(evidence.authorization.checks.summary[0].state, "SUCCESS");
    assert!(pr_merge::authorization(&f.store, "task").unwrap().is_some());
    assert!(f.run().unwrap().merged.is_empty());
    assert_eq!(f.prs.writes, [format!("squash:{}", "a".repeat(40))]);
}

#[test]
fn later_pushed_head_and_changes_to_pr_ownership_are_never_merged() {
    for pointer in [
        "/head/sha",
        "/head/ref",
        "/head/repo/full_name",
        "/base/ref",
        "/user/login",
        "/body",
    ] {
        let mut f = Fixture::new();
        *f.prs.detail.pointer_mut(pointer).unwrap() = json!("changed");
        f.held();
        assert!(f.prs.writes.is_empty());
        assert!(pr_merge::authorization(&f.store, "task").unwrap().is_none());
    }
}

#[test]
fn head_change_after_check_read_is_not_adopted_or_merged() {
    let mut f = Fixture::new();
    f.prs.race = true;
    f.held();
    assert!(f.prs.writes.is_empty());
    assert!(pr_merge::authorization(&f.store, "task").unwrap().is_some());
}

#[test]
fn pending_checks_unknown_mergeability_and_unclean_states_wait() {
    for state in ["unknown", "blocked", "dirty", "unstable", "has_hooks"] {
        let mut f = Fixture::new();
        f.prs.detail["mergeable_state"] = json!(state);
        assert_eq!(f.run().unwrap().waiting.len(), 1);
        assert!(f.prs.writes.is_empty());
    }
    let mut f = Fixture::new();
    f.prs.detail["mergeable"] = Value::Null;
    assert_eq!(f.run().unwrap().waiting.len(), 1);
    f.prs.detail["mergeable"] = json!(true);
    f.prs.checks.health.required_checks_pending = true;
    assert_eq!(f.run().unwrap().waiting.len(), 1);
    assert!(f.prs.writes.is_empty());
}

#[test]
fn unstable_pr_reports_failed_check_names_and_retains_the_observation() {
    let mut f = Fixture::new();
    f.prs.detail["mergeable_state"] = json!("unstable");
    f.prs.checks.health.failed_required_checks = vec!["Linux CI".into(), "macOS CI".into()];
    let report = f.run().unwrap();
    assert_eq!(
        report.waiting[0].reason,
        "failing checks: Linux CI, macOS CI"
    );
    assert!(f.prs.writes.is_empty());
    let payloads =
        journal::evidence_payloads(&f.store, "task", "attempt", "pr_merge_checks").unwrap();
    assert!(payloads[0].contains("Linux CI"));
}

#[test]
fn draft_policy_defaults_to_excluded_and_opt_in_promotes_before_merging() {
    let mut f = Fixture::new();
    f.prs.detail["draft"] = json!(true);
    f.prs.detail["mergeable_state"] = json!("draft");
    assert_eq!(
        f.run().unwrap().waiting[0].reason,
        "draft PR excluded by merge policy"
    );
    assert!(f.prs.writes.is_empty());
    if let MergePolicy::Enabled { allow_draft, .. } = &mut f.config.mappings[0].merge {
        *allow_draft = true;
    }
    assert_eq!(f.run().unwrap().merged, ["task"]);
    assert_eq!(
        f.prs.writes,
        ["ready".into(), format!("squash:{}", "a".repeat(40))]
    );
}

#[test]
fn changed_authenticated_identity_or_check_head_fails_before_any_write() {
    let mut f = Fixture::new();
    f.prs.login = "other".into();
    f.held();
    assert!(f.prs.writes.is_empty());
    f.prs.login = "acoliver".into();
    f.prs.checks.health.head = "b".repeat(40);
    f.held();
    assert!(f.prs.writes.is_empty());
}

#[test]
fn failed_write_is_recorded_and_never_replayed_or_reported_as_merged() {
    let mut f = Fixture::new();
    f.prs.fail_write = true;
    f.held();
    assert!(!pr_merge::completed(&f.store, "task").unwrap());
    f.prs.fail_write = false;
    f.held();
    assert_eq!(f.prs.writes.len(), 1);
    assert!(
        journal::evidence_kinds(&f.store, "task")
            .unwrap()
            .contains(&"pr_merge_error".into())
    );
}

#[test]
fn interrupted_success_recovers_exact_terminal_proof_without_another_write() {
    let mut f = Fixture::new();
    f.prs.fail_terminal = true;
    f.held();
    assert!(!pr_merge::completed(&f.store, "task").unwrap());
    f.prs.fail_terminal = false;
    assert_eq!(f.run().unwrap().merged, ["task"]);
    assert_eq!(f.prs.writes.len(), 1);
    assert_eq!(f.evidence().merged_head, "a".repeat(40));
}

#[test]
fn mapping_drift_and_unaccounted_attempts_do_not_authorize_merges() {
    let mut f = Fixture::new();
    f.config.mappings[0].base_branch = "other".into();
    assert_eq!(f.run().unwrap().waiting.len(), 1);
    assert_eq!(f.prs.reads, 0);
    assert!(f.run().unwrap().waiting.is_empty());
    assert_eq!(f.prs.reads, 0);
    assert!(f.prs.writes.is_empty());
    f.config.mappings[0].base_branch = "main".into();
    f.store
        .connection
        .execute("UPDATE attempts SET outcome=NULL", [])
        .unwrap();
    assert!(f.run().is_err());
    assert!(f.prs.writes.is_empty());
}

#[test]
fn cached_status_and_show_expose_merge_head_commit_and_check_summary() {
    let mut f = Fixture::new();
    f.run().unwrap();
    let root = f.store.root().to_path_buf();
    drop(f.store);
    let output = crate::cli::execute(&root, &["show".into(), "task".into()]).unwrap();
    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["tracker_closure"]["issue"]["target"]["number"], 40);
    assert_eq!(output["tracker_closure"]["closed_by_luthor"], true);
    assert_eq!(output["merge_evidence"]["merged_head"], "a".repeat(40));
    assert_eq!(output["merge_evidence"]["merge_commit"], "c".repeat(40));
    assert_eq!(output["last_merge_checks"]["summary"][0]["name"], "CI");
    let output = crate::cli::execute(&root, &["status".into()]).unwrap();
    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        output["tasks"][0]["merge_evidence"]["merged_head"],
        "a".repeat(40)
    );
}

#[test]
fn recovery_rejects_a_different_terminal_head_and_closed_unmerged_pr() {
    for changed_head in [true, false] {
        let mut f = Fixture::new();
        f.prs.fail_terminal = true;
        f.held();
        f.prs.fail_terminal = false;
        if changed_head {
            f.prs.detail["head"]["sha"] = json!("b".repeat(40));
        } else {
            f.prs.detail["merged"] = json!(false);
        }
        f.held();
        assert!(!pr_merge::completed(&f.store, "task").unwrap());
        assert_eq!(f.prs.writes.len(), 1);
    }
}

#[test]
fn malformed_api_state_fails_instead_of_hiding_an_upstream_error() {
    for field in ["state", "draft", "merged", "mergeable", "mergeable_state"] {
        let mut f = Fixture::new();
        f.prs.detail.as_object_mut().unwrap().remove(field);
        f.held();
        assert!(f.prs.writes.is_empty());
    }
}

#[test]
fn merge_preserves_existing_acoliver_live_write_restriction_even_for_another_mapping_author() {
    let mut f = Fixture::with_login("release-bot");
    f.held();
    assert!(f.prs.writes.is_empty());
    assert!(pr_merge::authorization(&f.store, "task").unwrap().is_none());
}

#[test]
fn conflicting_journal_id_must_not_authorize_an_unjournaled_write() {
    let mut f = Fixture::new();
    journal::record_intent(
        &mut f.store,
        "pr-merge-task",
        "task",
        None,
        "unrelated",
        "{}",
    )
    .unwrap();
    assert!(f.run().is_err());
    assert!(f.prs.writes.is_empty());
    assert!(pr_merge::authorization(&f.store, "task").unwrap().is_none());
    assert!(!pr_merge::completed(&f.store, "task").unwrap());
}

#[test]
fn corrupt_terminal_evidence_fails_instead_of_being_treated_as_a_completed_merge() {
    let mut f = Fixture::new();
    journal::record_evidence(&mut f.store, "task", None, "pr_merged", "{}").unwrap();
    assert!(f.run().is_err());
    assert!(f.prs.writes.is_empty());
}

#[test]
fn external_merge_with_changed_identity_holds_the_task_without_retiring_it() {
    for pointer in [
        "/user/login",
        "/base/ref",
        "/head/ref",
        "/body",
        "/created_at",
    ] {
        let mut f = Fixture::new();
        f.prs.merged();
        f.prs.detail["head"]["sha"] = json!("b".repeat(40));
        *f.prs.detail.pointer_mut(pointer).unwrap() = json!("changed");
        assert_eq!(
            f.held(),
            "merge step failed: merge verification failed: merged PR identity differs from the verified PR",
            "{pointer}"
        );
        assert_not_delivered(&f);
    }
}

#[test]
fn external_merge_without_verifiable_lineage_holds_the_task_with_its_reason() {
    let mut f = Fixture::new();
    f.prs.merged();
    f.prs.detail["head"]["sha"] = json!("b".repeat(40));
    let reason = f.held();
    assert!(reason.starts_with("merge step failed:"), "{reason}");
    assert_not_delivered(&f);
}

#[test]
fn external_merge_at_the_saved_head_settles_once_without_fabricated_merge_authorization() {
    let mut f = Fixture::new();
    f.prs.merged();
    let report = f.run().unwrap();
    assert_eq!(report.delivered.len(), 1);
    assert!(report.merged.is_empty() && report.waiting.is_empty());
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("pr_merged")
    );
    assert!(!pr_merge::completed(&f.store, "task").unwrap());
    assert!(f.prs.writes.is_empty());
    let reads = f.prs.reads;
    assert!(f.run().unwrap().delivered.is_empty());
    assert_eq!(f.prs.reads, reads);
    assert_eq!(f.prs.tracker_writes, 1);
}

fn assert_not_delivered(f: &Fixture) {
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    assert!(!pr_merge::completed(&f.store, "task").unwrap());
    assert!(f.prs.writes.is_empty());
    assert!(
        !journal::evidence_kinds(&f.store, "task")
            .unwrap()
            .contains(&"task_retired".into())
    );
}

mod followup;
