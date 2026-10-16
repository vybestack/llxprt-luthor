//! The fake Project applies the filter before paginating and charging points.
use super::*;
use crate::{
    config::{Mapping, Source},
    eligibility,
    github::project::{GhProjectReader, ProjectReader},
};
use serde_json::{Value, json};
use std::path::Path;

fn item(number: u64, state: &str, assigned: bool) -> Value {
    json!({"id":format!("ITEM{number}"), "content":{
        "__typename":"Issue", "id":format!("ISSUE{number}"), "number":number,
        "state":state, "labels":{"nodes":[{"name":"ready"}],"pageInfo":{"hasNextPage":false}},
        "assignees":{"nodes":if assigned {vec![json!({"login":"someone"})]} else {vec![]},"pageInfo":{"hasNextPage":false}},
        "milestone":null,"repository":{"id":"REPO","nameWithOwner":"org/tracker"}
    },"fieldValues":{"nodes":[],"pageInfo":{"hasNextPage":false}}})
}

fn page(nodes: &[Value], next: Option<String>, remaining: u64) -> Value {
    json!({"data":{
        "rateLimit":{"cost":1,"remaining":remaining,"resetAt":"2026-10-17T04:35:00Z"},
        "node":{"items":{"nodes":nodes,"pageInfo":{"hasNextPage":next.is_some(),"endCursor":next}}}
    }})
}

fn serve(root: &Path, history: usize, candidates: usize) -> GhProjectReader {
    let mut nodes: Vec<_> = (1..=history)
        .map(|n| {
            item(
                n as u64,
                if n % 2 == 0 { "OPEN" } else { "CLOSED" },
                n % 2 == 0,
            )
        })
        .collect();
    nodes.extend((1..=candidates).map(|n| item(10000 + n as u64, "OPEN", false)));
    // Model server-side eligibility, not a client-side post-pagination filter.
    let filtered: Vec<_> = nodes
        .iter()
        .filter(|node| {
            node["content"]["state"] == "OPEN"
                && node["content"]["assignees"]["nodes"]
                    .as_array()
                    .unwrap()
                    .is_empty()
        })
        .cloned()
        .collect();
    let pages: Vec<_> = filtered.chunks(20).collect();
    if pages.is_empty() {
        fs::write(root.join("page0"), page(&[], None, 4999).to_string()).unwrap();
    }
    let mut script = String::from(
        "#!/bin/sh\ncd -- \"$(dirname -- \"$0\")\" || exit 1\nprintf '%s\\n' \"$*\" >> calls\ncase \"$2\" in\ngraphql)\ncase \"$*\" in\n*'filter=is:open no:assignee'*)\ncase \"$*\" in\n",
    );
    for (index, chunk) in pages.iter().enumerate() {
        let next = (index + 1 < pages.len()).then(|| format!("PAGE{}", index + 1));
        fs::write(
            root.join(format!("page{index}")),
            page(chunk, next, 4999 - index as u64).to_string(),
        )
        .unwrap();
        if index > 0 {
            script.push_str(&format!("*cursor=PAGE{index}*) cat page{index};;\n"));
        }
    }
    script.push_str("*) cat page0;;\nesac;;\n*) cat history;;\nesac;;\nrepos/org/tracker) printf '%s' '{\"node_id\":\"REPO\"}';;\n");
    fs::write(
        root.join("history"),
        page(&nodes[..nodes.len().min(20)], None, 4999).to_string(),
    )
    .unwrap();
    for n in 1..=candidates {
        let number = 10000 + n;
        let issue = json!({"node_id":format!("ISSUE{number}"),"number":number,
            "repository_url":"https://api.github.com/repos/org/tracker",
            "html_url":format!("https://github.com/org/tracker/issues/{number}"),
            "state":"open","assignees":[],"labels":[{"name":"ready"}],"milestone":null});
        script.push_str(&format!(
            "repos/org/tracker/issues/{number}?*) printf '%s' '{issue}';;\n"
        ));
    }
    script.push_str("*) exit 99;;\nesac\n");
    let gh = root.join("gh");
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    GhProjectReader::new(gh)
}

fn configuration() -> (Source, Mapping) {
    let source = serde_json::from_value(json!({"project_id":"P","repositories":["org/tracker"],
        "ready_marker":{"kind":"label","name":"ready"},"milestone":null}))
    .unwrap();
    let mapping = serde_json::from_value(json!({"tracker_repository":"org/tracker",
        "code_repository":"org/code","checkout":"/checkout","base_branch":"main","push_remote":"origin",
        "allowed_pr_head_repository":"org/code","allowed_pr_author":"acoliver"})).unwrap();
    (source, mapping)
}

#[test]
fn discovery_points_scale_with_candidates_not_closed_or_assigned_project_history() {
    let (source, mapping) = configuration();
    for history in [0, 250, 5000] {
        let dir = tempfile::tempdir().unwrap();
        let mut reader = serve(dir.path(), history, 25);
        for cycle in 0..2 {
            let _guard = CycleGuard::start(&GithubPolling::default());
            let selected = eligibility::discover(
                &mut reader,
                std::slice::from_ref(&source),
                std::slice::from_ref(&mapping),
                None,
            )
            .unwrap();
            assert_eq!(selected.candidates.len(), 25, "history={history}");
            assert!(selected.source_read_failures.is_empty());
            assert_eq!(usage().graphql, 2, "only candidate pages are read");
            assert_eq!(usage().graphql_points, 2, "history={history}");
            assert_eq!(usage().rest, if cycle == 0 { 26 } else { 25 });
        }
        let calls = fs::read_to_string(dir.path().join("calls")).unwrap();
        let pages: Vec<_> = calls
            .lines()
            .filter(|line| line.starts_with("api graphql"))
            .collect();
        assert_eq!(pages.len(), 4);
        assert!(pages.iter().all(|line| line.contains("items(first: 20")
            && line.contains("query: $filter")
            && line.contains("filter=is:open no:assignee")
            && line.contains("rateLimit { cost remaining resetAt }")));
        if history > 0 {
            let unfiltered = reader.page("P", None).unwrap();
            assert_eq!(
                unfiltered.items[0].summary.as_ref().unwrap().state,
                "closed"
            );
            assert_eq!(
                unfiltered.items[1].summary.as_ref().unwrap().assignees,
                ["someone"]
            );
            assert!(
                !fs::read_to_string(dir.path().join("calls"))
                    .unwrap()
                    .lines()
                    .last()
                    .unwrap()
                    .contains("filter=")
            );
        }
    }
}

#[test]
fn projects_containing_only_history_cost_one_point_and_need_no_rest_reads() {
    let (source, mapping) = configuration();
    for history in [0, 5000] {
        let dir = tempfile::tempdir().unwrap();
        let mut reader = serve(dir.path(), history, 0);
        let _guard = CycleGuard::start(&GithubPolling::default());
        let selected = eligibility::discover(
            &mut reader,
            std::slice::from_ref(&source),
            std::slice::from_ref(&mapping),
            None,
        )
        .unwrap();
        assert!(selected.candidates.is_empty());
        assert_eq!(usage().graphql, 1);
        assert_eq!(usage().graphql_points, 1);
        assert_eq!(usage().rest, 0);
    }
}

#[test]
fn preview_and_targeted_selection_also_use_server_filtered_candidate_pages() {
    let (source, mapping) = configuration();
    let dir = tempfile::tempdir().unwrap();
    let mut reader = serve(dir.path(), 5000, 1);
    let _guard = CycleGuard::start(&GithubPolling::default());
    let sources = std::slice::from_ref(&source);
    let mappings = std::slice::from_ref(&mapping);
    assert_eq!(
        eligibility::select(&mut reader, sources, mappings)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        eligibility::select_target(&mut reader, sources, mappings, "org/tracker", 10001)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(usage().graphql_points, 2);
    assert_eq!(
        usage().rest,
        3,
        "each selection still REST-confirms the candidate"
    );
}
