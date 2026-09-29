use luthor::pr_evidence::{ExpectedPr, PrIdentityEvidence, Verification, verify};

fn expected() -> ExpectedPr {
    ExpectedPr {
        issue_url: "https://github.com/tracker/repo/issues/4".into(),
        repository_id: 10,
        repository: "code/repo".into(),
        base_branch: "main".into(),
        head_repository_id: 11,
        head_repository: "worker/repo".into(),
        task_branch: "luthor/task-4".into(),
        allowed_author: "worker".into(),
        current_identity: "worker".into(),
    }
}

fn evidence() -> PrIdentityEvidence {
    PrIdentityEvidence {
        id: 9,
        repository_id: 10,
        repository: "code/repo".into(),
        base_repository: "code/repo".into(),
        base_branch: "main".into(),
        head_repository_id: 11,
        head_repository: "worker/repo".into(),
        head_branch: "luthor/task-4".into(),
        author: "worker".into(),
        open: true,
        draft: true,
        checks: vec!["failure".into()],
    }
}

#[test]
fn accepts_open_draft_with_red_checks_as_advisory() {
    assert!(matches!(
        verify(
            evidence(),
            &expected(),
            "Tracker-Issue: https://github.com/tracker/repo/issues/4"
        ),
        Verification::Matching(_)
    ));
}

#[test]
fn rejects_wrong_link_author_head_repository_target_repository_base_and_closed_pr() {
    let expected = expected();
    let body = "Tracker-Issue: https://github.com/tracker/repo/issues/4";
    assert_eq!(
        verify(
            evidence(),
            &expected,
            "Tracker-Issue: https://github.com/tracker/repo/issues/40"
        ),
        Verification::Mismatch("tracker_link")
    );
    let mut pr = evidence();
    pr.author = "attacker".into();
    assert_eq!(
        verify(pr, &expected, body),
        Verification::Mismatch("author")
    );
    let mut pr = evidence();
    pr.head_branch = "other".into();
    assert_eq!(verify(pr, &expected, body), Verification::Mismatch("head"));
    let mut pr = evidence();
    pr.head_repository_id = 99;
    assert_eq!(verify(pr, &expected, body), Verification::Mismatch("head"));
    let mut pr = evidence();
    pr.repository_id = 99;
    assert_eq!(
        verify(pr, &expected, body),
        Verification::Mismatch("target_repository")
    );
    let mut pr = evidence();
    pr.base_branch = "develop".into();
    assert_eq!(verify(pr, &expected, body), Verification::Mismatch("base"));
    let mut pr = evidence();
    pr.open = false;
    assert_eq!(
        verify(pr, &expected, body),
        Verification::Mismatch("not_open_or_missing_id")
    );
}
