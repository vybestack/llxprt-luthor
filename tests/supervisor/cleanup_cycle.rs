use super::cleanup_cycle_fixture::{complete, config};
use super::*;
use luthor::state::{journal, task_records};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

#[test]
fn cleanup_mixed_historical_config_and_fixture_outputs_does_not_block_daemon_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let mut historical = config.clone();
    historical.worktree_root = dir.path().join("old-worktrees");
    historical.resume.args.push("old-selection-option".into());
    let mut store = StateStore::open(&config.state_root, config.capacity).unwrap();
    complete(&mut store, &historical, "historical", 7);
    let mut old_selection = config.clone();
    old_selection
        .resume
        .args
        .push("old-selection-option".into());
    complete(&mut store, &old_selection, "historical-selection", 6);
    let mut terminal = complete(&mut store, &config, "cleanable", 8);
    terminal["merge_commit_sha"] = json!("c".repeat(40));
    let nested = config.worktree_root.join("cleanable/tmp/f/checkout");
    fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-b", "fixture"]);
    drop(store);
    let gh = gh_fixture(dir.path(), &terminal);
    let config_path = dir.path().join("config.json");
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(["daemon", "--config"])
        .arg(&config_path)
        .args([
            "--config-revision",
            "rev",
            "--once",
            "--execute",
            "--repository",
            "org/tracker",
            "--issues",
            "9",
        ])
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", gh.parent().unwrap().display()),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_cleanup_report(&report);
    assert_cycle_state(&config, &historical, dir.path(), &report);
}

fn assert_cycle_state(config: &Config, historical: &Config, directory: &Path, report: &Value) {
    let store = StateStore::open(&config.state_root, config.capacity).unwrap();
    assert_eq!(
        task_records::task_phase(&store, "historical")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    assert!(historical.worktree_root.join("historical/README").exists());
    assert!(
        config
            .worktree_root
            .join("historical-selection/README")
            .exists()
    );
    assert_eq!(
        task_records::task_phase(&store, "historical-selection")
            .unwrap()
            .as_deref(),
        Some("pr_complete")
    );
    assert!(!config.worktree_root.join("cleanable").exists());
    let task = report["launched"][0]["task_id"].as_str().unwrap();
    assert_eq!(
        task_records::selection_evidence(&store, task)
            .unwrap()
            .unwrap()
            .candidate
            .issue_number,
        9
    );
    let calls = fs::read_to_string(directory.join("calls")).unwrap();
    assert!(calls.contains("POST repos/org/tracker/issues/9/assignees"));
    // Historical terminal probes are independent of locally mismatched workers.
    assert_eq!(
        calls
            .matches("api repos/org/code/pulls/7 --include")
            .count(),
        1
    );
    assert_eq!(
        calls
            .matches("api repos/org/code/pulls/6 --include")
            .count(),
        1
    );
    assert!(!calls.contains("check-runs") && !calls.contains("headRefOid"));
    assert!(!calls.contains("/merge") && !calls.contains("rerun-failed-jobs"));
    assert!(
        journal::evidence_kinds(&store, "historical")
            .unwrap()
            .contains(&"cleanup_failed".into())
    );
    crate::supervisor_support::wait_for_worktree_owner_release(store.root(), task);
}

fn assert_cleanup_report(report: &Value) {
    assert_eq!(
        report["cleanup"]["cleaned"],
        json!(["cleanable"]),
        "{report}"
    );
    assert_eq!(
        report["cleanup"]["skipped"],
        json!([{
            "task_id":"historical", "reason":"cleanup configuration differs from task ownership"
        }, {
            "task_id":"historical-selection", "reason":"cleanup configuration differs from task ownership"
        }])
    );
    assert_eq!(report["pr_repair"]["skipped"], report["cleanup"]["skipped"]);
    assert!(report["pr_repair"]["held"].as_array().unwrap().is_empty());
    assert!(report["cleanup"]["retained"].as_array().unwrap().is_empty());
    assert_eq!(report["launched"].as_array().unwrap().len(), 1);
}

fn gh_fixture(root: &Path, terminal: &Value) -> std::path::PathBuf {
    let project = json!({"data":{"node":{"items":{
        "nodes":[{"id":"item-9","content":{"__typename":"Issue","state":"OPEN","labels":{"nodes":[{"name":"ready"}],"pageInfo":{"hasNextPage":false}},"assignees":{"nodes":[],"pageInfo":{"hasNextPage":false}},"milestone":null,"id":"issue-9","number":9,
            "repository":{"id":"repo","nameWithOwner":"org/tracker"}},
            "fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],
        "pageInfo":{"hasNextPage":false,"endCursor":null}
    }}}});
    let mut issue = json!({"node_id":"issue-9","number":9,"repository_url":"https://api.github.com/repos/org/tracker",
        "html_url":"https://github.com/org/tracker/issues/9","state":"open","assignees":[],
        "labels":[{"name":"ready"}],"milestone":null});
    let unassigned = issue.to_string();
    issue["assignees"] = json!([{"login":"operator"}]);
    let gh = root.join("gh");
    fs::write(&gh, format!(r#"#!/bin/sh
printf '%s\n' "$*" >> '{root}/calls'
case "$2" in
  rate_limit) printf '%s\n' '{{"resources":{{"core":{{"remaining":5000,"reset":9999999999}},"graphql":{{"remaining":5000,"reset":9999999999}}}}}}' ;;
  user) printf '%s\n' 'acoliver' ;;
  graphql) printf '%s\n' '{project}' ;;
  repos/org/tracker) printf '%s\n' '{{"node_id":"repo"}}' ;;
  repos/org/code) printf '%s\n' '{{"id":10,"full_name":"org/code"}}' ;;
  repos/org/code/commits/*/check-runs\?*) printf '%s\n' '{{"total_count":0,"check_runs":[]}}' ;;
  repos/org/code/commits/*/status) printf '%s\n' '{{"statuses":[]}}' ;;
  repos/org/code/pulls/8) printf '%s\n' '{terminal}' ;;
  repos/org/code/pulls\?*) printf '%s\n' '[]' ;;
  repos/org/tracker/issues/9\?*) if test -f '{root}/assigned'; then printf '%s\n' '{issue}'; else printf '%s\n' '{unassigned}'; fi ;;
  -X) test "$3" = POST && test "$4" = repos/org/tracker/issues/9/assignees || exit 99
    touch '{root}/assigned'; printf '%s\n' '{{}}' ;;
  *) exit 99 ;;
esac
"#, root=root.display())).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    gh
}
