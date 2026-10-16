//! A whole execute cycle reports the durable backoff, including old holds.
use super::typed_retry::{bounded, fail, reconcile};
use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

#[test]
fn execute_cycle_reports_retry_scheduled_with_the_durable_backoff_deadline() {
    let mut f = Fixture::for_author(Default::default(), "acoliver");
    bounded(&mut f, 3);
    let plan = f.repair().unwrap().remove(0);
    fail(&mut f, &plan, "model-transient-server", 5);
    reconcile(&mut f, &plan.attempt_id).unwrap();
    let gh = serve(&mut f);
    let config_path = f._dir.path().join("config.json");
    fs::write(&config_path, serde_json::to_vec(&f.config).unwrap()).unwrap();
    drop(f.store);
    let output = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .args(["daemon", "--config"])
        .arg(config_path)
        .args([
            "--config-revision",
            "rev",
            "--once",
            "--execute",
            "--repository",
            "org/tracker",
            "--issues",
            "7",
        ])
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", gh.parent().unwrap().display()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let scheduled = &report["pr_repair"]["retry_scheduled"];
    assert_eq!(scheduled.as_array().unwrap().len(), 1, "{report}");
    assert_eq!(scheduled[0]["task_id"], "task");
    assert_eq!(scheduled[0]["attempt_id"], plan.attempt_id);
    assert!(report["launched"].as_array().unwrap().is_empty());
    assert!(report["pr_repair"]["held"].as_array().unwrap().is_empty());
    let store = StateStore::open(&f.config.state_root, 1).unwrap();
    let evidence = journal::evidence_payloads(
        &store,
        "task",
        &plan.attempt_id,
        "pr_repair_retry_scheduled",
    )
    .unwrap();
    let saved: Value = serde_json::from_str(&evidence[0]).unwrap();
    assert_eq!(scheduled[0], saved);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("pr_complete")
    );
}

fn serve(f: &mut Fixture) -> std::path::PathBuf {
    let detail = f.prs.detail("org/code", 42).unwrap();
    let list = f.prs.page("org/code", 1).unwrap();
    let c = &f.projects.0;
    let project = json!({"data":{"node":{"items":{
        "nodes":[{"id":c.item_id,"content":{"__typename":"Issue","state":"OPEN","labels":{"nodes":[{"name":"ready"}],"pageInfo":{"hasNextPage":false}},"assignees":{"nodes":[],"pageInfo":{"hasNextPage":false}},"milestone":null,"id":c.issue_node_id,"number":7,
            "repository":{"id":c.tracker_repo_id,"nameWithOwner":"org/tracker"}},
            "fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],
        "pageInfo":{"hasNextPage":false,"endCursor":null}
    }}}});
    let issue = json!({"node_id":c.issue_node_id,"number":7,"repository_url":"https://api.github.com/repos/org/tracker",
        "html_url":c.issue_url,"state":"open","assignees":[{"login":"operator"}],
        "labels":[{"name":"ready"}],"milestone":null});
    let gh = f._dir.path().join("gh");
    fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
case "$2" in
  user) printf '%s\n' 'acoliver' ;;
  graphql) printf '%s\n' '{project}' ;;
  repos/org/tracker) printf '%s\n' '{{"node_id":"repo"}}' ;;
  repos/org/code) printf '%s\n' '{{"id":10,"full_name":"org/code"}}' ;;
  repos/org/tracker/issues/7\?*) printf '%s\n' '{issue}' ;;
  repos/org/code/pulls/42) printf '%s\n' '{detail}' ;;
  repos/org/code/pulls\?*) printf '%s\n' '{list}' ;;
  *) printf 'unexpected gh call: %s\n' "$*" >&2; exit 99 ;;
esac
"#,
            list = json!(list)
        ),
    )
    .unwrap();
    let script = fs::read_to_string(&gh).unwrap().replace("case \"$2\" in", r#"case "$2" in
  rate_limit) printf '%s\n' '{"resources":{"core":{"remaining":5000,"reset":9999999999},"graphql":{"remaining":5000,"reset":9999999999}}}' ;;
"#);
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    gh
}
