use super::*;
use crate::pr_merge::{MergeAuthorization, MergeChecks, MergeEvidence, TrackerTarget};

fn detail(f: &Fixture, change: impl FnOnce(&mut serde_json::Value)) {
    let file = f.dir.path().join("detail");
    let mut value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    change(&mut value);
    fs::write(file, value.to_string()).unwrap();
}

fn route(f: &Fixture, route: &str) {
    let script = f.dir.path().join("gh");
    let source = fs::read_to_string(&script).unwrap();
    fs::write(
        script,
        source.replace("case \"$1:$2\" in", &format!("case \"$1:$2\" in\n{route}")),
    )
    .unwrap();
}

#[test]
fn behind_update_is_explicit_merge_pinned_to_observed_head_and_never_rebase() {
    let mut f = Fixture::new("acoliver", false, false);
    detail(&f, |value| {
        value["mergeable_state"] = json!("behind");
        value["node_id"] = json!("PR_node");
    });
    route(
        &f,
        "api:graphql) printf '%s\\n' '{\"data\":{\"updatePullRequestBranch\":{\"pullRequest\":{\"id\":\"PR_node\"}}}}' ;;",
    );
    f.reader.update_branch(&f.saved).unwrap();
    let calls = f.calls();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.contains("api graphql"))
            .count(),
        1
    );
    assert!(calls.contains("updateMethod:MERGE"));
    assert!(calls.contains("expectedHeadOid:$head"));
    assert!(calls.contains(&format!("head={}", f.saved.head_commit_sha)));
    assert!(!calls.contains("REBASE") && !calls.contains("force") && !calls.contains("--admin"));
}

#[test]
fn branch_update_revalidates_account_and_head_and_surfaces_graphql_failure() {
    for (login, race, response, writes) in [
        ("other", false, "{}", 0),
        ("acoliver", true, "{}", 0),
        (
            "acoliver",
            false,
            "{\"errors\":[{\"message\":\"conflict\"}]}",
            1,
        ),
        (
            "acoliver",
            false,
            "{\"data\":{\"updatePullRequestBranch\":null}}",
            1,
        ),
    ] {
        let mut f = Fixture::new(login, false, race);
        detail(&f, |value| {
            value["mergeable_state"] = json!("behind");
            value["node_id"] = json!("PR_node");
        });
        route(&f, &format!("api:graphql) printf '%s\\n' '{response}' ;;"));
        assert!(f.reader.update_branch(&f.saved).is_err());
        assert_eq!(
            f.calls()
                .lines()
                .filter(|line| line.contains("api graphql"))
                .count(),
            writes
        );
    }
}

fn tracker(f: &mut Fixture) -> TrackerTarget {
    f.saved.tracker_issue_url = "https://github.com/org/tracker/issues/40".into();
    let target = TrackerTarget {
        repository: "org/tracker".into(),
        number: 40,
        node_id: "ISSUE_node".into(),
        url: f.saved.tracker_issue_url.clone(),
    };
    detail(f, |value| {
        value["body"] = json!(format!(
            "Tracker-Issue: {}\nFixes org/tracker#40",
            target.url
        ));
        value["state"] = json!("closed");
        value["merged"] = json!(true);
        value["merged_at"] = json!("2026-10-14T12:00:00Z");
        value["merge_commit_sha"] = json!("c".repeat(40));
    });
    let issue = json!({"node_id":target.node_id,"number":40,"html_url":target.url,"repository_url":"https://api.github.com/repos/org/tracker","state":"open","closed_at":null});
    fs::write(f.dir.path().join("issue"), issue.to_string()).unwrap();
    route(
        f,
        &format!(
            "api:repos/org/tracker/issues/40) cat '{}/issue' ;;\nissue:close) exit 0 ;;",
            f.dir.path().display()
        ),
    );
    target
}

fn evidence(f: &Fixture) -> MergeEvidence {
    let checks: MergeChecks = serde_json::from_value(json!({"health":{"id":42,"head":f.saved.head_commit_sha,"conflicting":false,
        "failed_required_checks":[],"required_checks_pending":false,"failed_jobs":[],"required_jobs":[],"required_checks_source":"configured_all"},"summary":[]})).unwrap();
    MergeEvidence {
        authorization: MergeAuthorization {
            pr: f.saved.clone(),
            policy: crate::config::MergePolicy::Enabled {
                method: MergeMethod::Squash,
                required_checks: crate::config::RequiredChecks::Rule(
                    crate::config::CheckRule::All {},
                ),
                allow_draft: false,
            },
            checks,
            observed_at: 1,
        },
        merge_commit: "c".repeat(40),
        merged_head: f.saved.head_commit_sha.clone(),
        observed_at: 2,
    }
}

#[test]
fn tracker_close_targets_saved_tracker_not_code_repo_and_comment_names_pr_and_head() {
    let mut f = Fixture::new("acoliver", false, false);
    let target = tracker(&mut f);
    let merge = evidence(&f);
    assert!(f.reader.tracker_issue(&target).unwrap().closed_at.is_none());
    f.reader.close_tracker(&merge, &target).unwrap();
    let calls = f.calls();
    assert!(calls.contains("issue close 40 --repo org/tracker --reason completed --comment"));
    assert!(calls.contains(&merge.authorization.pr.url));
    assert!(calls.contains(&merge.merged_head));
    assert!(calls.contains(&merge.merge_commit));
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("issue close"))
            .count(),
        1
    );
}

#[test]
fn tracker_close_refuses_changed_pr_or_issue_identity_and_upstream_write_failure() {
    for change in ["head", "commit", "unmerged", "issue", "write"] {
        let mut f = Fixture::new("acoliver", false, false);
        let target = tracker(&mut f);
        let merge = evidence(&f);
        match change {
            "head" => detail(&f, |v| v["head"]["sha"] = json!("b".repeat(40))),
            "commit" => detail(&f, |v| v["merge_commit_sha"] = json!("b".repeat(40))),
            "unmerged" => detail(&f, |v| v["merged"] = json!(false)),
            "issue" => fs::write(f.dir.path().join("issue"), "{\"node_id\":\"other\"}").unwrap(),
            "write" => route(&f, "issue:close) exit 1 ;;"),
            _ => unreachable!(),
        }
        assert!(f.reader.close_tracker(&merge, &target).is_err(), "{change}");
        assert_eq!(
            f.calls()
                .lines()
                .filter(|line| line.starts_with("issue close"))
                .count(),
            usize::from(change == "write")
        );
    }
}

#[test]
fn tracker_reads_reject_malformed_terminal_states_and_pull_requests() {
    for patch in [
        json!({"state":"closed","closed_at":null}),
        json!({"state":"open","closed_at":"unexpected"}),
        json!({"pull_request":{}}),
    ] {
        let mut f = Fixture::new("acoliver", false, false);
        let target = tracker(&mut f);
        let file = f.dir.path().join("issue");
        let mut issue: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        issue
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        fs::write(file, issue.to_string()).unwrap();
        assert!(f.reader.tracker_issue(&target).is_err());
        assert!(!f.calls().contains("issue close"));
    }
}
