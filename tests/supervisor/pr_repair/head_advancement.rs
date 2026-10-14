//! Issue 69: external base merges and historically held open PRs.
use super::*;
use luthor::coordinator::restore_open_held_prs;

const IDENTITY_HOLD: &str =
    "PR repair held: PR, process, receipt or ownership evidence did not verify";

fn read_git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn local(f: &Fixture) -> std::path::PathBuf {
    f.config.worktree_root.join("task")
}

/// Model GitHub update-branch without changing the task worktree. The fixture's
/// SSH transport serves this isolated bare remote, never the real repository.
fn advance(f: &mut Fixture, kind: &str) -> String {
    let checkout = &f.config.mappings[0].checkout;
    let remote = checkout.parent().unwrap().join("remote.git");
    let old = f.prs.head.clone();
    fs::write(checkout.join("base-change"), &old).unwrap();
    git(checkout, &["add", "base-change"]);
    git(checkout, &["commit", "-m", "new base"]);
    let base = read_git(checkout, &["rev-parse", "HEAD"]);
    read_git(
        &remote,
        &[
            "fetch",
            checkout.to_str().unwrap(),
            "refs/heads/main:refs/heads/main",
        ],
    );
    let tree = read_git(checkout, &["rev-parse", &format!("{base}^{{tree}}")]);
    let head = match kind {
        "base_merge" => read_git(
            checkout,
            &[
                "commit-tree",
                &tree,
                "-p",
                &old,
                "-p",
                &base,
                "-m",
                "Merge base into task",
            ],
        ),
        "foreign" => read_git(
            checkout,
            &[
                "commit-tree",
                &tree,
                "-p",
                &old,
                "-m",
                "Foreign task commit",
            ],
        ),
        "foreign_merge" => {
            let foreign = read_git(
                checkout,
                &["commit-tree", &tree, "-p", &old, "-m", "Foreign parent"],
            );
            read_git(
                checkout,
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    &old,
                    "-p",
                    &foreign,
                    "-m",
                    "Merge foreign branch",
                ],
            )
        }
        _ => unreachable!(),
    };
    read_git(
        &remote,
        &[
            "fetch",
            checkout.to_str().unwrap(),
            &format!("{head}:refs/heads/luthor/task"),
        ],
    );
    f.prs.head = head.clone();
    head
}

fn restore(f: &mut Fixture) -> luthor::coordinator::RestoreReport {
    restore_open_held_prs(&mut f.store, &mut f.projects, &mut f.prs).unwrap()
}

#[test]
fn update_branch_base_merge_followed_by_required_failure_authorizes_repair() {
    let mut f = Fixture::new();
    let original = f.prs.head.clone();
    let head = advance(&mut f, "base_merge");
    f.prs.conflict = false;
    f.prs.failures = vec!["quality gates".into()];
    assert!(f.repair().unwrap().is_empty());
    assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), head);
    assert_eq!(f.prs.reruns, 1);
    assert_eq!(
        fs::read_to_string(local(&f).join("base-change")).unwrap(),
        original
    );
    f.prs.run_attempt = 2;
    let plan = f.repair().unwrap().remove(0);
    assert_eq!(plan.expected_worktree.head, head);
    let rows =
        journal::evidence_payloads(&f.store, "task", "attempt-real", "verified_open_pr").unwrap();
    assert_eq!(rows.len(), 1, "original completion is immutable");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rows[0]).unwrap()["head_commit_sha"],
        original
    );
    assert_eq!(
        journal::evidence_payloads(&f.store, "task", "attempt-real", "pr_base_merge_adopted")
            .unwrap()
            .len(),
        1
    );
    f.run(&plan, "repaired");
    assert!(matches!(
        f.reconcile(&plan.attempt_id),
        Reconciliation::Completed { .. }
    ));
    assert_eq!(
        task_records::task_phase(&f.store, "task")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
}

#[test]
fn multiple_base_merges_are_adopted_and_shown_without_changing_base_branch() {
    let mut f = Fixture::new();
    advance(&mut f, "base_merge");
    let head = advance(&mut f, "base_merge");
    let base = read_git(&f.config.mappings[0].checkout, &["rev-parse", "HEAD"]);
    f.prs.conflict = false;
    assert!(f.repair().unwrap().is_empty());
    assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        read_git(&f.config.mappings[0].checkout, &["rev-parse", "HEAD"]),
        base
    );
    let shown: serde_json::Value = serde_json::from_str(
        &luthor::cli::execute(&f.config.state_root, &["show".into(), "task".into()]).unwrap(),
    )
    .unwrap();
    let proof = shown["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "pr_head_verified")
        .unwrap();
    assert_eq!(proof["detail"]["head_commit_sha"], head);
    assert_eq!(
        shown["verified_pr"]["observed_at"],
        proof["detail"]["observed_at"]
    );
    assert_eq!(
        shown["last_pr_verification_at_unix_secs"],
        proof["detail"]["observed_at"]
    );
}

#[test]
fn foreign_commits_and_non_base_merges_stay_held_without_fast_forward() {
    for kind in ["foreign", "foreign_merge"] {
        let mut f = Fixture::new();
        let old = f.prs.head.clone();
        advance(&mut f, kind);
        assert_eq!(f.repair().unwrap_err().len(), 1, "{kind}");
        assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), old);
        task_records::hold_task(&mut f.store, "task", IDENTITY_HOLD).unwrap();
        assert!(restore(&mut f).restored.is_empty());
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("held")
        );
        assert_eq!(scheduling::reservation_count(&f.store).unwrap(), 0);
    }
}

#[test]
fn held_task_restores_after_operator_fast_forward_or_interrupted_adoption() {
    for reason in [IDENTITY_HOLD, "exit PR present"] {
        let mut f = Fixture::new();
        let head = advance(&mut f, "base_merge");
        // The object exists locally via the fixture checkout's commit-tree.
        git(&local(&f), &["merge", "--ff-only", &head]);
        task_records::hold_task(&mut f.store, "task", reason).unwrap();
        drop(f.store);
        f.store = StateStore::open(&f.config.state_root, 1).unwrap();
        assert_eq!(restore(&mut f).restored, ["task"]);
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("pr_complete")
        );
        assert!(restore(&mut f).restored.is_empty());
        let plan = f.repair().unwrap().remove(0);
        assert_eq!(plan.expected_worktree.head, head);
    }
}

#[test]
fn held_task_can_fetch_and_fast_forward_the_legitimate_pr_head() {
    let mut f = Fixture::new();
    let head = advance(&mut f, "base_merge");
    task_records::hold_task(&mut f.store, "task", IDENTITY_HOLD).unwrap();
    assert_eq!(restore(&mut f).restored, ["task"]);
    assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), head);
    assert_eq!(f.repair().unwrap().len(), 1);
}

#[test]
fn restoration_and_adoption_require_fresh_claim_receipt_identity_and_clean_tree() {
    for scenario in [
        "dirty", "receipt", "claim", "author", "pr_id", "live", "semantic",
    ] {
        let mut f = Fixture::new();
        let old = f.prs.head.clone();
        advance(&mut f, "base_merge");
        task_records::hold_task(&mut f.store, "task", IDENTITY_HOLD).unwrap();
        match scenario {
            "dirty" => fs::write(local(&f).join("keep-me"), "operator work").unwrap(),
            "receipt" => fs::remove_file(execution::receipt_path(&f.config)).unwrap(),
            "claim" => f.projects.1 = 0,
            "author" => f.prs.inner.author = Some("foreign".into()),
            "pr_id" => f.prs.wrong_pr_id = true,
            "live" => {
                scheduling::reserve(&mut f.store, "task", "attempt-live").unwrap();
            }
            "semantic" => {
                journal::record_evidence(
                    &mut f.store,
                    "task",
                    Some("attempt-real"),
                    "pr_repair_blocked",
                    "requires person",
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(restore(&mut f).restored.is_empty(), "{scenario}");
        assert_eq!(
            read_git(&local(&f), &["rev-parse", "HEAD"]),
            old,
            "{scenario}"
        );
        assert_eq!(
            task_records::task_phase(&f.store, "task")
                .unwrap()
                .as_deref(),
            Some("held"),
            "{scenario}"
        );
        if scenario == "dirty" {
            assert_eq!(
                fs::read_to_string(local(&f).join("keep-me")).unwrap(),
                "operator work"
            );
        }
    }
}

#[test]
fn historical_exit_pr_present_without_completion_proof_restores_only_exact_local_head() {
    for matching in [true, false] {
        let mut f = Fixture::new();
        let connection =
            rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
        connection
            .execute("DELETE FROM evidence WHERE kind='verified_open_pr'", [])
            .unwrap();
        drop(connection);
        task_records::hold_task(&mut f.store, "task", "exit PR present").unwrap();
        if !matching {
            advance(&mut f, "foreign");
        }
        let report = restore(&mut f);
        assert_eq!(report.restored.len(), usize::from(matching));
        if matching {
            assert_eq!(f.repair().unwrap().len(), 1);
        } else {
            assert_eq!(
                task_records::task_phase(&f.store, "task")
                    .unwrap()
                    .as_deref(),
                Some("held")
            );
        }
    }
}

#[test]
fn manually_matching_a_foreign_pr_head_does_not_erase_recorded_lineage() {
    let mut f = Fixture::new();
    let head = advance(&mut f, "foreign");
    git(&local(&f), &["merge", "--ff-only", &head]);
    task_records::hold_task(&mut f.store, "task", IDENTITY_HOLD).unwrap();
    let report = restore(&mut f);
    assert!(report.restored.is_empty());
    assert_eq!(report.retained.len(), 1);
    assert_eq!(read_git(&local(&f), &["rev-parse", "HEAD"]), head);
    assert!(
        !journal::evidence_kinds(&f.store, "task")
            .unwrap()
            .contains(&"pr_head_verified".into())
    );
}
