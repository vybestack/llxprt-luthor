use super::support::*;

pub(crate) fn repository_identity_reads_immutable_id_and_rejects_malformed_responses() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    let reader = GhPullRequestReader::new(gh.clone());
    for (body, expected) in [
        (r#"{"id":1234,"full_name":"code/project"}"#, Ok(1234)),
        (
            r#"{"id":1234,"full_name":"code/renamed"}"#,
            Err("invalid-repository"),
        ),
        (
            r#"{"id":"1234","full_name":"code/project"}"#,
            Err("invalid-repository"),
        ),
        (
            r#"{"id":0,"full_name":"code/project"}"#,
            Err("invalid-repository"),
        ),
        (r#"{"full_name":"code/project"}"#, Err("invalid-repository")),
    ] {
        std::fs::write(&gh, format!("#!/bin/sh\nprintf '%s' '{}'\n", body)).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        match (reader.repository_identity(REPO), expected) {
            (Ok(id), Ok(want)) => assert_eq!(id, want),
            (Err(error), Err(code)) => {
                assert_eq!(error.category, ErrorCategory::Malformed);
                assert_eq!(error.code, code);
                assert!(!error.to_string().contains("renamed"));
            }
            (actual, expected) => panic!("unexpected result {actual:?}, expected {expected:?}"),
        }
    }

    std::fs::write(&gh, "#!/bin/sh\nprintf '%s' '{\"status\":403}'\nexit 1\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        reader.repository_identity(REPO).unwrap_err(),
        LookupError {
            category: ErrorCategory::Permission,
            code: "command-failed",
            status: Some(403),
        }
    );

    assert_eq!(
        reader
            .repository_identity("org/repo?per_page=1")
            .unwrap_err()
            .category,
        ErrorCategory::Malformed
    );
    assert_eq!(
        reader
            .repository_identity("org/repo/extra")
            .unwrap_err()
            .category,
        ErrorCategory::Malformed
    );
}

pub(crate) fn gh_check_collection_preserves_results_and_failures_for_linked_draft_prs() {
    use std::os::unix::fs::PermissionsExt;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    let body = format!("Tracker-Issue: {ISSUE}");
    let list_json = serde_json::to_string(&json!([item(9, &body)])).unwrap();
    let mut pr_detail = detail(9, &body);
    pr_detail["head"]["sha"] = json!(SHA);
    let detail_json = serde_json::to_string(&pr_detail).unwrap();

    let scenarios = [
        (
            "failed and pending checks",
            "printf '%s' '{\"total_count\":2,\"check_runs\":[{\"name\":\"unit\",\"status\":\"completed\",\"conclusion\":\"failure\"},{\"name\":\"build\",\"status\":\"in_progress\",\"conclusion\":null}]}'",
            Some(vec![
                "unit:completed:failure".into(),
                "build:in_progress:pending".into(),
            ]),
        ),
        (
            "empty result",
            "printf '%s' '{\"total_count\":0,\"check_runs\":[]}'",
            Some(vec![]),
        ),
        (
            "forbidden request",
            "printf '%s' '{\"status\":403}'; exit 1",
            None,
        ),
        ("malformed response", "printf '%s' 'not-json'", None),
        (
            "incomplete pagination",
            "case \"$2\" in *'page=1') printf '%s' '{\"total_count\":101,\"check_runs\":[{\"name\":\"unit\",\"status\":\"completed\",\"conclusion\":\"success\"}]}' ;; *) exit 2 ;; esac",
            None,
        ),
    ];

    for (label, checks, expected) in scenarios {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\ncase \"$2\" in\n  *'pulls?state='*) printf '%s' '{}' ;;\n  *'/pulls/9') printf '%s' '{}' ;;\n  *'/commits/{SHA}/status') printf '%s' '{{\"statuses\":[]}}' ;;\n  *'/check-runs?per_page=100&page=1'*) {} ;;  *'/check-runs?per_page=100&page=2'*) printf '%s' '{{\"total_count\":101,\"check_runs\":[]}}' ;;\n  *) exit 2 ;;\nesac\n",
                list_json, detail_json, checks
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut reader = GhPullRequestReader::new(gh);
        let LookupResult::OpenPreexisting(evidence) = lookup(&mut reader, REPO, ISSUE).unwrap()
        else {
            panic!("{label}: expected linked open PR")
        };
        assert!(evidence.draft, "{label}: draft open PR must still match");
        assert_eq!(evidence.checks, expected, "{label}");
    }
}
pub(crate) fn legacy_status_failures_are_advisory_for_linked_open_prs() {
    use std::os::unix::fs::PermissionsExt;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    let body = format!("Tracker-Issue: {ISSUE}");
    let list_json = serde_json::to_string(&json!([item(9, &body)])).unwrap();
    let mut pr_detail = detail(9, &body);
    pr_detail["head"]["sha"] = json!(SHA);
    let detail_json = serde_json::to_string(&pr_detail).unwrap();

    for (status_reply, expected) in [
        (
            "printf '%s' '{\"statuses\":[{\"context\":\"legacy/build\",\"state\":\"failure\"}]}'",
            Some(vec!["legacy/build:failure".to_string()]),
        ),
        ("printf '%s' 'not-json'", None),
        ("printf '%s' '{\"status\":403}'; exit 1", None),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\ncase \"$2\" in\n  *'pulls?state='*) printf '%s' '{}' ;;\n  *'/pulls/9') printf '%s' '{}' ;;\n  *'/commits/{SHA}/status') {status_reply} ;;\n  *'/check-runs?per_page=100&page=1'*) printf '%s' '{{\"total_count\":0,\"check_runs\":[]}}' ;;\n  *) exit 2 ;;\nesac\n",
                list_json, detail_json
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut reader = GhPullRequestReader::new(gh);
        let LookupResult::OpenPreexisting(evidence) = lookup(&mut reader, REPO, ISSUE).unwrap()
        else {
            panic!("expected linked open PR")
        };
        assert_eq!(evidence.checks, expected);
    }
}

pub(crate) fn gh_check_collection_reads_full_page_and_bounded_second_page() {
    use std::os::unix::fs::PermissionsExt;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    let body = format!("Tracker-Issue: {ISSUE}");
    let list_json = serde_json::to_string(&json!([item(9, &body)])).unwrap();
    let mut pr_detail = detail(9, &body);
    pr_detail["head"]["sha"] = json!(SHA);
    let detail_json = serde_json::to_string(&pr_detail).unwrap();
    let first_page =
        vec![json!({"name":"check-{index}","status":"completed","conclusion":"success"}); 100];
    let first_page = first_page
        .into_iter()
        .enumerate()
        .map(|(index, _)| json!({"name":format!("check-{index}"),"status":"completed","conclusion":"success"}))
        .collect::<Vec<_>>();
    let page_one_json =
        serde_json::to_string(&json!({"total_count":101,"check_runs":first_page})).unwrap();
    let page_two_json = serde_json::to_string(&json!({"total_count":101,"check_runs":[{"name":"last","status":"completed","conclusion":"failure"}]})).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\ncase \"$2\" in\n  *'pulls?state='*) printf '%s' '{}' ;;\n  *'/pulls/9') printf '%s' '{}' ;;\n  *'/commits/{SHA}/status') printf '%s' '{{\"statuses\":[]}}' ;;\n  *'/check-runs?per_page=100&page=1'*) printf '%s' '{}' ;;  *'/check-runs?per_page=100&page=2'*) printf '%s' '{}' ;;\n  *) exit 2 ;;\nesac\n",
            list_json, detail_json, page_one_json, page_two_json
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut reader = GhPullRequestReader::new(gh);
    let LookupResult::OpenPreexisting(evidence) = lookup(&mut reader, REPO, ISSUE).unwrap() else {
        panic!("expected linked open PR")
    };
    let checks = evidence.checks.unwrap();
    assert_eq!(checks.len(), 101);
    assert_eq!(checks[0], "check-0:completed:success");
    assert_eq!(checks[99], "check-99:completed:success");
    assert_eq!(checks[100], "last:completed:failure");
}
pub(crate) fn gh_detail_preserves_exact_body_for_independent_verification() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    let body = format!("Summary text\nTracker-Issue: {ISSUE}\n\nMore detail.\n");
    let list_body = format!("Tracker-Issue: {ISSUE}");
    let detail_json = serde_json::to_string(&detail(9, &body)).unwrap();
    let list_json = serde_json::to_string(&json!([item(9, &list_body)])).unwrap();
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\ncase \"$2\" in\n  *'pulls?state='*) printf '%s' '{}' ;;\n  *'/pulls/9') printf '%s' '{}' ;;\n  *) exit 2 ;;\nesac\n",
            list_json, detail_json
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut reader = GhPullRequestReader::new(gh);
    let LookupResult::OpenPreexisting(evidence) = lookup(&mut reader, REPO, ISSUE).unwrap() else {
        panic!("expected preexisting PR")
    };
    assert_eq!(evidence.body, body);
    assert_eq!(evidence.tracker_issue_url, ISSUE);
}
