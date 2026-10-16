//! Issue 104: a completed repair must not strand the next base update.
use super::*;

fn completed_repair() -> Fixture {
    let mut f = Fixture::for_author(Default::default(), "acoliver");
    f.prs.merge_state = Some("clean");
    let plan = f.repair().unwrap().remove(0);
    git(&plan.worktree, &["commit", "--allow-empty", "-m", "repair"]);
    f.prs.head = read_git(&plan.worktree, &["rev-parse", "HEAD"]);
    f.run(&plan, "repaired");
    assert!(matches!(
        f.reconcile(&plan.attempt_id),
        Reconciliation::Completed { .. }
    ));
    f.prs.conflict = false;
    f
}

#[test]
fn completed_repair_adopts_next_base_update_without_a_hold() {
    let mut f = completed_repair();
    let report = fs::read(local(&f).join(REPAIR_RESULT_FILE)).unwrap();
    for iteration in 0..2 {
        if iteration == 1 {
            ignore_report(&f);
        }
        let head = advance(&mut f, "base_merge");
        let outcome = f.repair_outcome();
        assert!(outcome.held.is_empty(), "{:?}", outcome.held);
        assert!(outcome.launched.is_empty());
        assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), head);
        assert_eq!(
            fs::read(local(&f).join(REPAIR_RESULT_FILE)).unwrap(),
            report
        );
    }
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
}

#[test]
fn completed_repair_base_update_and_green_ci_merge_without_a_hold() {
    let mut f = completed_repair();
    let old = f.prs.head.clone();
    let head = advance(&mut f, "base_merge");
    f.prs.head = old.clone();
    f.prs.merge_state = Some("behind");
    f.prs.update_to = Some(head.clone());
    enable_merge(&mut f);
    let report = merge(&mut f);
    assert_eq!(report.waiting.len(), 1);
    assert!(report.merged.is_empty());
    assert_eq!(
        f.prs.updates.as_slice(),
        std::slice::from_ref(&old),
        "{:?}",
        report.waiting
    );
    assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), old);
    let updated = journal::evidence_kinds(&f.store, "task").unwrap();
    assert!(updated.contains(&"pr_branch_updated".into()));
    f.prs.pending = true;
    assert!(f.repair_outcome().held.is_empty());
    assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), head);
    assert!(merge(&mut f).merged.is_empty());
    assert!(f.prs.merges.is_empty(), "pending CI must not merge");
    f.prs.pending = false;
    let report = merge(&mut f);
    assert!(report.waiting.is_empty(), "{:?}", report.waiting);
    assert_eq!(report.merged, ["task"]);
    assert_eq!(f.prs.merges, [head]);
    assert_eq!(f.prs.updates, [old]);
    assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("pr_merged")
    );
}

fn enable_merge(f: &mut Fixture) {
    f.config.mappings[0].merge = luthor::config::MergePolicy::Enabled {
        method: luthor::config::MergeMethod::Squash,
        required_checks: Default::default(),
        allow_draft: false,
    };
}

fn merge(f: &mut Fixture) -> luthor::coordinator::MergeReport {
    luthor::coordinator::merge_verified_prs(&mut f.store, &f.config, &mut f.prs).unwrap()
}

#[test]
fn completed_repair_restores_historical_tip_hold_after_base_update() {
    let mut f = completed_repair();
    let report = fs::read(local(&f).join(REPAIR_RESULT_FILE)).unwrap();
    let head = advance(&mut f, "base_merge");
    task_records::hold_task(
        &mut f.store,
        "task",
        "PR repair held: worktree identity or tip did not verify",
    )
    .unwrap();
    drop(f.store);
    f.store = StateStore::open(&f.config.state_root, 1).unwrap();
    let restored = restore(&mut f);
    assert_eq!(restored.restored, ["task"]);
    assert!(restored.retained.is_empty());
    assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read(local(&f).join(REPAIR_RESULT_FILE)).unwrap(),
        report
    );
    assert!(f.repair_outcome().held.is_empty());
}

#[test]
fn completed_repair_foreign_heads_still_hold_without_fast_forward() {
    for kind in ["foreign", "foreign_merge"] {
        let mut f = completed_repair();
        let old = f.prs.head.clone();
        let head = advance(&mut f, kind);
        // An authored update intent is never a substitute for base lineage.
        f.prs.head = old.clone();
        f.prs.merge_state = Some("behind");
        f.prs.update_to = Some(head);
        enable_merge(&mut f);
        assert_eq!(merge(&mut f).waiting.len(), 1);
        assert_eq!(f.prs.updates.as_slice(), std::slice::from_ref(&old));
        assert_eq!(f.repair_outcome().held.len(), 1, "{kind}");
        let restored = restore(&mut f);
        assert!(restored.restored.is_empty(), "{kind}");
        assert_eq!(restored.retained.len(), 1, "{kind}");
        assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), old);
        assert!(f.prs.merges.is_empty());
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    }
}

mod merger;
mod reports;

#[test]
fn an_untracked_report_without_a_completed_repair_is_not_exempt() {
    let mut f = Fixture::new();
    let old = f.prs.head.clone();
    fs::write(
        local(&f).join(REPAIR_RESULT_FILE),
        r#"{"attempt_id":"attempt-real","status":"repaired"}"#,
    )
    .unwrap();
    advance(&mut f, "base_merge");
    assert_eq!(f.repair_outcome().held.len(), 1);
    assert!(restore(&mut f).restored.is_empty());
    assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), old);
}

fn ignore_report(f: &Fixture) {
    let excludes = f._dir.path().join("report-ignore");
    fs::write(&excludes, format!("/{REPAIR_RESULT_FILE}\n")).unwrap();
    git(
        &local(f),
        &["config", "core.excludesFile", excludes.to_str().unwrap()],
    );
}

#[test]
fn base_update_cannot_overwrite_an_ignored_retained_report() {
    let mut f = completed_repair();
    ignore_report(&f);
    let old = f.prs.head.clone();
    let report = local(&f).join(REPAIR_RESULT_FILE);
    let bytes = fs::read(&report).unwrap();
    let checkout = &f.config.mappings[0].checkout;
    fs::write(checkout.join(REPAIR_RESULT_FILE), "incoming base file").unwrap();
    git(checkout, &["add", "--force", REPAIR_RESULT_FILE]);
    git(
        checkout,
        &["commit", "-m", "base introduces reserved report path"],
    );
    advance(&mut f, "base_merge");
    assert_eq!(f.repair_outcome().held.len(), 1);
    assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), old);
    assert_eq!(fs::read(&report).unwrap(), bytes);
    assert!(restore(&mut f).restored.is_empty());
    assert_eq!(fs::read(&report).unwrap(), bytes);
}
