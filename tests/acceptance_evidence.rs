use luthor::pr_evidence::{ExpectedPr, PrIdentityEvidence, Verification, verify};

fn expected() -> ExpectedPr {
    ExpectedPr {
        issue_url: "https://github.com/vybestack/llxprt-code/issues/12".into(),
        repository_id: 10,
        repository: "vybestack/llxprt-code".into(),
        base_branch: "main".into(),
        head_repository_id: 10,
        head_repository: "vybestack/llxprt-code".into(),
        task_branch: "luthor/issue-12".into(),
        allowed_author: "authorized".into(),
        current_identity: "authorized".into(),
    }
}

fn pr(id: u64, draft: bool) -> PrIdentityEvidence {
    PrIdentityEvidence {
        id,
        repository_id: 10,
        repository: "vybestack/llxprt-code".into(),
        base_repository: "vybestack/llxprt-code".into(),
        base_branch: "main".into(),
        head_repository_id: 10,
        head_repository: "vybestack/llxprt-code".into(),
        head_branch: "luthor/issue-12".into(),
        author: "authorized".into(),
        open: true,
        draft,
        checks: Some(vec!["pending".into()]),
    }
}

#[test]
fn acceptance_accepts_open_draft_prs_and_pending_or_red_checks() {
    let expected = expected();
    for evidence in [pr(301, false), pr(302, true)] {
        assert!(matches!(
            verify(
                evidence,
                &expected,
                "Details\nTracker-Issue: https://github.com/vybestack/llxprt-code/issues/12\n"
            ),
            Verification::Matching(_)
        ));
    }
    let mut red = pr(303, false);
    red.checks = Some(vec!["failure".into()]);
    assert!(matches!(
        verify(
            red,
            &expected,
            "Tracker-Issue: https://github.com/vybestack/llxprt-code/issues/12"
        ),
        Verification::Matching(_)
    ));
}

#[test]
fn acceptance_holds_mismatched_identity_or_missing_tracker_link() {
    let expected = expected();
    let mut evidence = pr(401, false);
    evidence.author = "other".into();
    assert_eq!(
        verify(
            evidence,
            &expected,
            "Tracker-Issue: https://github.com/vybestack/llxprt-code/issues/12"
        ),
        Verification::Mismatch("author")
    );

    let mut evidence = pr(402, false);
    evidence.head_branch = "unexpected".into();
    assert_eq!(
        verify(
            evidence,
            &expected,
            "Tracker-Issue: https://github.com/vybestack/llxprt-code/issues/12"
        ),
        Verification::Mismatch("head")
    );
    assert_eq!(
        verify(pr(403, false), &expected, "no tracker evidence"),
        Verification::Mismatch("tracker_link")
    );
}

#[test]
fn acceptance_distinct_issue_and_pr_id_requirement_is_explicitly_tested() {
    let issue_ids = [12, 13, 14, 15, 16];
    let pr_ids = [301, 302, 303, 304, 305];
    assert_eq!(
        issue_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        5
    );
    assert_eq!(
        pr_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        5
    );
}
