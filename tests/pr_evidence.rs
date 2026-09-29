use luthor::{
    github::pull_request::PullRequestEvidence,
    pr_evidence::{ExpectedPr, PrIdentityEvidence, Verification, VerifiedOpenPr, verify},
};

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

fn pull_request(body: &str) -> PullRequestEvidence {
    let pr = evidence();
    PullRequestEvidence {
        id: pr.id,
        number: 4,
        url: "https://github.com/code/repo/pull/4".into(),
        repository_id: pr.repository_id,
        repository: pr.repository,
        base_repository_id: pr.repository_id,
        base_repository: "code/repo".into(),
        base_branch: pr.base_branch,
        head_repository_id: pr.head_repository_id,
        head_repository: pr.head_repository,
        head_branch: pr.head_branch,
        author: pr.author,
        draft: pr.draft,
        tracker_issue_url: "https://github.com/tracker/repo/issues/4".into(),
        body: body.into(),
        checks: pr.checks,
        created_at: "2026-09-29T00:00:00Z".into(),
        commit_sha: "abc123".into(),
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

#[test]
fn creates_typed_proof_for_open_draft_with_red_checks() {
    let proof = VerifiedOpenPr::from_matching(
        pull_request("Tracker-Issue: https://github.com/tracker/repo/issues/4"),
        &expected(),
        "worker",
        "attempt-1",
        42,
    );
    assert!(proof.is_ok());
}

#[test]
fn denies_forged_tracker_body_and_head() {
    let expected = expected();
    assert!(
        VerifiedOpenPr::from_matching(
            pull_request("Tracker-Issue: https://github.com/tracker/repo/issues/40"),
            &expected,
            "worker",
            "attempt-1",
            42,
        )
        .is_err()
    );
    let mut forged_head = pull_request("Tracker-Issue: https://github.com/tracker/repo/issues/4");
    forged_head.head_branch = "other".into();
    assert!(
        VerifiedOpenPr::from_matching(forged_head, &expected, "worker", "attempt-1", 42).is_err()
    );
}
