use serde_json::{Value, json};

use super::{
    evidence::parse_evidence,
    types::{ErrorCategory, LookupError, PullRequestEvidence},
};

fn detail() -> Value {
    json!({
        "id":99, "number":7, "state":"open",
        "html_url":"https://github.com/org/repo/pull/7",
        "base":{"ref":"main","repo":{"id":10,"full_name":"org/repo"}},
        "head":{"ref":"work","sha":"short-sha","repo":{"id":20,"full_name":"fork/repo"}},
        "user":{"login":"worker"}, "draft":true, "created_at":"observed-date",
        "body":"Tracker-Issue: https://github.com/org/tracker/issues/7\nFixes #7",
        "luthor_checks":["build:completed:failure", "legacy:pending"]
    })
}

#[test]
fn evidence_retains_every_identity_and_advisory_field() {
    assert_eq!(
        parse_evidence(&detail(), "org/repo").unwrap(),
        PullRequestEvidence {
            id: 99,
            number: 7,
            url: "https://github.com/org/repo/pull/7".into(),
            repository_id: 10,
            repository: "org/repo".into(),
            base_repository_id: 10,
            base_repository: "org/repo".into(),
            base_branch: "main".into(),
            head_repository_id: 20,
            head_repository: "fork/repo".into(),
            head_branch: "work".into(),
            author: "worker".into(),
            draft: true,
            tracker_issue_url: "https://github.com/org/tracker/issues/7".into(),
            body: "Tracker-Issue: https://github.com/org/tracker/issues/7\nFixes #7".into(),
            checks: Some(vec![
                "build:completed:failure".into(),
                "legacy:pending".into()
            ]),
            created_at: "observed-date".into(),
            commit_sha: "short-sha".into(),
        }
    );
}

#[test]
fn invalid_evidence_fields_preserve_exact_error_contract() {
    for (pointer, value) in [
        ("/id", json!(0)),
        ("/id", json!("99")),
        ("/number", json!(0)),
        ("/number", json!(-1)),
        ("/state", json!("closed")),
        ("/html_url", json!("https://github.com/org/repo/pull/07")),
        ("/html_url", json!("https://github.com/org/other/pull/7")),
        ("/base/repo/id", json!(0)),
        ("/head/repo/id", json!(0)),
        ("/base/repo/full_name", json!("org/other")),
        ("/base/ref", json!("")),
        ("/head/ref", json!("")),
        ("/head/repo/full_name", json!("")),
        ("/user/login", json!("")),
        ("/draft", json!("true")),
        ("/created_at", json!("")),
        ("/head/sha", json!("")),
        ("/body", Value::Null),
        ("/base", Value::Null),
        ("/head", Value::Null),
    ] {
        let mut response = detail();
        *response.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            parse_evidence(&response, "org/repo"),
            Err(LookupError {
                category: ErrorCategory::Malformed,
                code: "invalid-pr-details",
                status: None,
            }),
            "{pointer}"
        );
    }
    for field in [
        "id",
        "number",
        "state",
        "html_url",
        "base",
        "head",
        "body",
        "user",
        "draft",
        "created_at",
    ] {
        let mut response = detail();
        response.as_object_mut().unwrap().remove(field);
        assert_eq!(
            parse_evidence(&response, "org/repo").unwrap_err().code,
            "invalid-pr-details",
            "{field}"
        );
    }
}

#[test]
fn evidence_keeps_optional_checks_and_first_tracker_line_semantics() {
    let mut response = detail();
    response["body"] = json!("Tracker-Issue: first\nTracker-Issue: second");
    response["luthor_checks"] = json!(["failure", 42, null, "pending"]);
    let parsed = parse_evidence(&response, "org/repo").unwrap();
    assert_eq!(parsed.tracker_issue_url, "first");
    assert_eq!(
        parsed.checks,
        Some(vec!["failure".into(), "pending".into()])
    );
    response["body"] = json!("");
    response.as_object_mut().unwrap().remove("luthor_checks");
    let parsed = parse_evidence(&response, "org/repo").unwrap();
    assert_eq!(parsed.tracker_issue_url, "");
    assert_eq!(parsed.checks, None);
    response["luthor_checks"] = json!([]);
    assert_eq!(
        parse_evidence(&response, "org/repo").unwrap().checks,
        Some(vec![])
    );
    response["luthor_checks"] = json!({});
    assert_eq!(parse_evidence(&response, "org/repo").unwrap().checks, None);
}
