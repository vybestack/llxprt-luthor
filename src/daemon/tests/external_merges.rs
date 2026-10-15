//! Issue 78: a PR merged outside Luthor after its head advanced must not fail
//! the execute cycle, and only a base-merge-only advance counts as delivered.
use super::merges::{merge_enabled, seed_aligned_issue12};
use super::repairs::{assert_only_331_dispatched, pr_detail, serve_pr_42_detail};
use super::*;
use crate::state::journal;

fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
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

enum Advance {
    /// What `update-branch` with `update_method=merge` does.
    BaseMerge,
    /// A commit on the PR branch that is not a merge of the base.
    ForeignCommit,
    /// A merge whose second parent is not on the configured base.
    ForeignMerge,
}

/// Moves the base and the PR head on the fixture's remote the way GitHub would,
/// leaving the task worktree on its recorded tip.
fn advance_head(f: &Fixture, old: &str, kind: Advance) -> String {
    let checkout = &f.config.mappings[0].checkout;
    let remote = checkout.parent().unwrap().join("remote.git");
    fs::write(checkout.join("base-change"), old).unwrap();
    git_out(checkout, &["add", "base-change"]);
    git_out(checkout, &["commit", "-m", "new base"]);
    let base = git_out(checkout, &["rev-parse", "HEAD"]);
    let tree = git_out(checkout, &["rev-parse", &format!("{base}^{{tree}}")]);
    let head = match kind {
        Advance::BaseMerge => git_out(
            checkout,
            &[
                "commit-tree",
                &tree,
                "-p",
                old,
                "-p",
                &base,
                "-m",
                "Merge base",
            ],
        ),
        Advance::ForeignCommit => git_out(
            checkout,
            &["commit-tree", &tree, "-p", old, "-m", "Foreign commit"],
        ),
        Advance::ForeignMerge => {
            let foreign = git_out(
                checkout,
                &["commit-tree", &tree, "-p", old, "-m", "Foreign parent"],
            );
            git_out(
                checkout,
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    old,
                    "-p",
                    &foreign,
                    "-m",
                    "Merge foreign",
                ],
            )
        }
    };
    let checkout = checkout.to_str().unwrap();
    git_out(
        &remote,
        &["fetch", checkout, "refs/heads/main:refs/heads/main"],
    );
    git_out(
        &remote,
        &["fetch", checkout, &format!("{head}:refs/pull/42/head")],
    );
    head
}

fn merged_detail(f: &Fixture, head: &str, branch: &str) -> serde_json::Value {
    let mut detail = pr_detail(f, false);
    detail["head"]["sha"] = json!(head);
    detail["head"]["ref"] = json!(branch);
    detail["merge_commit_sha"] = json!("c".repeat(40));
    detail
}

struct Case {
    f: Fixture,
    config: Config,
    original: String,
}

/// Task-12 is `pr_complete` with a verified PR that GitHub merged at a newer
/// head; issue 331 is eligible in the same cycle.
fn external_merge(kind: Advance, tweak: impl FnOnce(&mut serde_json::Value)) -> Case {
    let f = Fixture::new();
    let (saved, worktree) = seed_aligned_issue12(&f);
    let head = advance_head(&f, &saved.head_commit_sha, kind);
    let mut detail = merged_detail(&f, &head, &worktree.branch);
    tweak(&mut detail);
    serve_pr_42_detail(&f, false, &detail);
    serve_tracker(&f);
    let config = merge_enabled(&f);
    Case {
        f,
        config,
        original: saved.head_commit_sha,
    }
}

fn run(case: &Case) -> (ScheduleReport, RecordingLauncher) {
    let mut launcher = RecordingLauncher::default();
    let report = case.f.cycle(&case.config, &mut launcher).unwrap().unwrap();
    (report, launcher)
}

fn kinds(case: &Case) -> Vec<String> {
    let store = StateStore::open(&case.config.state_root, 2).unwrap();
    journal::evidence_kinds(&store, "task-12").unwrap()
}

fn assert_no_merge_writes(case: &Case) {
    let calls = fs::read_to_string(&case.f.calls).unwrap();
    assert!(!calls.contains("--method PUT") && !calls.contains("pr merge"));
}

fn assert_held_with(case: &Case, reason: &str) {
    let (report, launcher) = run(case);
    assert!(report.pr_merge.delivered.is_empty() && report.pr_merge.merged.is_empty());
    assert_eq!(report.pr_merge.waiting.len(), 1);
    assert_eq!(report.pr_merge.waiting[0].task_id, "task-12");
    assert!(
        report.pr_merge.waiting[0].reason.contains(reason),
        "{}",
        report.pr_merge.waiting[0].reason
    );
    assert_eq!(repairs::phase_of_12(&case.config), "pr_complete");
    let kinds = kinds(case);
    assert!(kinds.contains(&"pr_merge_error".into()));
    assert!(!kinds.contains(&"task_retired".into()) && !kinds.contains(&"pr_merged".into()));
    assert_only_331_dispatched(&launcher, &report);
    assert_no_merge_writes(case);
    assert!(
        !fs::read_to_string(&case.f.calls)
            .unwrap()
            .contains("--method PATCH")
    );
}

#[test]
fn merge_after_base_merge_head_advance_is_recorded_as_delivered_and_work_continues() {
    let case = external_merge(Advance::BaseMerge, |_| {});
    let head = merged_head(&case);
    let (report, launcher) = run(&case);
    assert_eq!(report.pr_merge.delivered.len(), 1);
    assert_eq!(report.pr_merge.delivered[0].task_id, "task-12");
    assert!(report.pr_merge.waiting.is_empty() && report.pr_merge.merged.is_empty());
    assert_only_331_dispatched(&launcher, &report);
    assert_eq!(repairs::phase_of_12(&case.config), "pr_merged");
    let kinds = kinds(&case);
    assert!(kinds.contains(&"task_retired".into()));
    assert!(
        !kinds.contains(&"pr_merged".into()),
        "no fabricated Luthor merge evidence"
    );
    assert!(!kinds.contains(&"pr_merge_error".into()));
    let store = StateStore::open(&case.config.state_root, 2).unwrap();
    let retired = crate::state::retirement::latest(&store.connection, "task-12")
        .unwrap()
        .unwrap();
    assert_eq!(retired["phase"], "pr_merged");
    assert_eq!(retired["evidence"]["luthor_merge_evidence"], "none");
    assert_eq!(
        retired["evidence"]["pr"]["verified_head_commit_sha"],
        case.original
    );
    assert_eq!(retired["evidence"]["pr"]["head_commit_sha"], head);
    assert_eq!(
        retired["evidence"]["base_merge_lineage"]["merges"],
        json!([head])
    );
    assert_no_merge_writes(&case);
}

fn merged_head(case: &Case) -> String {
    git_out(
        &case.f.config.mappings[0]
            .checkout
            .parent()
            .unwrap()
            .join("remote.git"),
        &["rev-parse", "refs/pull/42/head"],
    )
}

#[test]
fn delivery_is_not_repeated_on_a_later_cycle() {
    let case = external_merge(Advance::BaseMerge, |_| {});
    run(&case);
    let (report, _) = run(&case);
    assert!(report.pr_merge.delivered.is_empty() && report.pr_merge.waiting.is_empty());
    assert_eq!(repairs::phase_of_12(&case.config), "pr_merged");
}

#[test]
fn merged_pr_whose_head_changed_by_a_foreign_commit_holds_only_that_task() {
    let case = external_merge(Advance::ForeignCommit, |_| {});
    assert_held_with(&case, "PR advanced by a non-base-merge commit");
}

#[test]
fn merged_pr_whose_head_changed_by_a_non_base_merge_holds_only_that_task() {
    let case = external_merge(Advance::ForeignMerge, |_| {});
    assert_held_with(&case, "PR merge parent is not on configured base");
}

#[test]
fn merged_pr_with_any_other_identity_change_holds_only_that_task() {
    for pointer in ["/user/login", "/base/ref", "/body", "/created_at"] {
        let case = external_merge(Advance::BaseMerge, |detail| {
            *detail.pointer_mut(pointer).unwrap() = json!("changed");
        });
        assert_held_with(&case, "merged PR identity differs from the verified PR");
    }
}

#[test]
fn unmerged_pr_with_an_unrecorded_head_change_holds_instead_of_failing_the_cycle() {
    let case = external_merge(Advance::BaseMerge, |detail| {
        detail["state"] = json!("open");
        detail["merged"] = json!(false);
        detail["closed_at"] = json!(null);
        detail["merged_at"] = json!(null);
    });
    assert_held_with(&case, "merge-pr-identity-changed");
}

#[test]
fn held_task_journals_its_reason_once_across_repeated_cycles() {
    let case = external_merge(Advance::ForeignCommit, |_| {});
    run(&case);
    let (report, _) = run(&case);
    assert_eq!(report.pr_merge.waiting.len(), 1);
    let errors = kinds(&case)
        .iter()
        .filter(|kind| *kind == "pr_merge_error")
        .count();
    assert_eq!(errors, 1);
}

pub(super) fn serve_tracker(f: &Fixture) {
    let closed = f.root.path().join("externally-delivered-tracker-closed");
    let issue = json!({"node_id":"ISSUE12","number":12,"html_url":"https://github.com/org/tracker/issues/12","repository_url":"https://api.github.com/repos/org/tracker","state":"open","closed_at":null});
    let mut terminal = issue.clone();
    terminal["state"] = json!("closed");
    terminal["closed_at"] = json!("2026-10-15T12:00:00Z");
    let routes = format!(
        r#"
  repos/org/tracker/issues/12) if test -f '{closed}'; then printf '%s\n' '{terminal}'; else printf '%s\n' '{issue}'; fi ;;
  --method) test "$3" = PATCH && test "$4" = repos/org/tracker/issues/12 || exit 96
    test "$6" = state=closed && test "$8" = state_reason=completed || exit 97
    touch '{closed}'; printf '%s\n' '{{}}' ;;
"#,
        closed = closed.display()
    );
    let script = fs::read_to_string(&f.gh).unwrap();
    fs::write(
        &f.gh,
        script.replace("  user)", &format!("{routes}\n  user)")),
    )
    .unwrap();
}
