//! Opt-in merge policy through the real execute cycle and hermetic gh adapter.
use super::*;
use crate::config::{MergeMethod, MergePolicy};

fn serve_merge(f: &Fixture) -> String {
    repairs::seed_finished_issue12(f);
    let store = StateStore::open(&f.config.state_root, 2).unwrap();
    let worktree = crate::state::worktree_records::worktree_record(&store, "task-12")
        .unwrap()
        .unwrap()
        .identity
        .unwrap();
    let mut saved = crate::state::pr_repair::saved_pr(&store.connection, "task-12").unwrap();
    saved.head_commit_sha = worktree.head.clone();
    saved.head_branch = worktree.branch.clone();
    store
        .connection
        .execute(
            "UPDATE evidence SET payload=?1 WHERE task_id='task-12' AND kind='verified_open_pr'",
            [serde_json::to_string(&saved).unwrap()],
        )
        .unwrap();
    drop(store);
    let mut detail = repairs::pr_detail(true);
    detail["head"]["sha"] = json!(worktree.head);
    detail["head"]["ref"] = json!(worktree.branch);
    detail["mergeable"] = json!(true);
    detail["mergeable_state"] = json!("clean");
    let mut terminal = detail.clone();
    terminal["state"] = json!("closed");
    terminal["merged"] = json!(true);
    terminal["merged_at"] = json!("2026-10-14T12:00:00Z");
    terminal["merge_commit_sha"] = json!("c".repeat(40));
    let health = json!({"data":{"repository":{"pullRequest":{"databaseId":4242,"headRefOid":saved.head_commit_sha,"mergeable":"MERGEABLE",
        "commits":{"nodes":[{"commit":{"oid":saved.head_commit_sha,"statusCheckRollup":{"contexts":{"nodes":[
            {"__typename":"StatusContext","context":"CI","state":"SUCCESS","isRequired":false}
        ],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}]}}}}});
    let merged = f.root.path().join("merged");
    let closed = f.root.path().join("tracker-closed");
    let selection = crate::state::task_records::selection_evidence(
        &StateStore::open(&f.config.state_root, 2).unwrap(),
        "task-12",
    )
    .unwrap()
    .unwrap();
    let mut issue = json!({"node_id":selection.candidate.issue_node_id,"number":12,"html_url":selection.candidate.issue_url,"repository_url":format!("https://api.github.com/repos/{}", selection.candidate.repository),"state":"open","closed_at":null});
    let open_issue = issue.to_string();
    issue["state"] = json!("closed");
    issue["closed_at"] = json!("2026-10-14T12:00:00Z");
    let closed_issue = issue.to_string();
    let routes = format!(
        r#"
  repos/{tracker}/issues/12) if test -f '{closed}'; then printf '%s\n' '{closed_issue}'; else printf '%s\n' '{open_issue}'; fi ;;
  repos/org/code/pulls/42) if test -f '{merged}'; then printf '%s\n' '{terminal}'; else printf '%s\n' '{detail}'; fi ;;
  --method) test "$3" = PUT && test "$4" = repos/org/code/pulls/42/merge || exit 96
    test "$6" = sha={head} && test "$8" = merge_method=squash || exit 97
    touch '{merged}'; printf '%s\n' '{{"merged":true,"sha":"{commit}"}}' ;;
"#,
        tracker = selection.candidate.repository,
        closed = closed.display(),
        merged = merged.display(),
        head = saved.head_commit_sha,
        commit = "c".repeat(40)
    );
    let script = fs::read_to_string(&f.gh).unwrap();
    let script = script.replace("  graphql) printf", &format!("  graphql) case \"$*\" in *headRefOid*) printf '%s\\n' '{health}'; exit 0 ;; esac; printf"));
    let script = script.replace("  user)", &format!("{routes}\n  user)"));
    let script = script.replace(
        "case \"$2\" in",
        &format!(
            "if test \"$1:$2\" = issue:close; then touch '{}'; exit 0; fi\ncase \"$2\" in",
            closed.display()
        ),
    );
    fs::write(&f.gh, script).unwrap();
    saved.head_commit_sha
}

#[test]
fn execute_cycle_merges_only_opted_in_verified_head_and_reports_durable_evidence() {
    let f = Fixture::new();
    let head = serve_merge(&f);
    let mut config = f.config.clone();
    config.mappings[0].merge = MergePolicy::Enabled {
        method: MergeMethod::Squash,
        required_checks: crate::config::RequiredChecks::Rule(crate::config::CheckRule::All {}),
        allow_draft: false,
    };
    let mut launcher = RecordingLauncher::default();
    let report = f.cycle(&config, &mut launcher).unwrap().unwrap();
    assert_eq!(report.pr_merge.merged, ["task-12"]);
    assert!(report.pr_merge.waiting.is_empty());
    repairs::assert_only_331_dispatched(&launcher, &report);
    let store = StateStore::open(&config.state_root, 2).unwrap();
    let payload: String = store
        .connection
        .query_row(
            "SELECT payload FROM evidence WHERE task_id='task-12' AND kind='pr_merged'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let evidence: crate::pr_merge::MergeEvidence = serde_json::from_str(&payload).unwrap();
    assert_eq!(evidence.merged_head, head);
    assert_eq!(evidence.merge_commit, "c".repeat(40));
    assert_eq!(evidence.authorization.checks.summary[0].name, "CI");
    let calls = fs::read_to_string(&f.calls).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.contains("--method PUT"))
            .count(),
        1
    );
    assert!(!calls.contains("pr merge") && !calls.contains("--admin"));
}
