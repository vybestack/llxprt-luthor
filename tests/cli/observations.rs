use super::support::Fixture;
use serde_json::{Value, json};

pub(crate) fn operator_observations_are_typed_current_and_redacted_in_status_and_show() {
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "attempt", "session");
    let secret = "private-token-in-untrusted-response";
    f.evidence("task", None, "held_reason", "old hold");
    for (kind, category, status, phase) in pr_cases() {
        f.db()
            .execute("UPDATE tasks SET state=?1 WHERE id='task'", [phase])
            .unwrap();
        let payload = json!({"observed_at_unix_secs":1700000000,
            "repository":format!("org/code/{secret}"),"status":status,"secret":secret});
        f.evidence("task", Some("attempt"), kind, &payload.to_string());
        for (view, summary) in [
            (f.run(&["status"]).unwrap()["tasks"][0].clone(), "status"),
            (f.run(&["show", "task"]).unwrap(), "show"),
        ] {
            assert_pr_view(&view, summary, kind, category, &status, phase, secret);
        }
    }
    let source = json!({"task_id":"task","status":"held",
        "reasons":["source_read_failed"],"issue_state":secret,
        "assignees":[secret],"marker_present":null,"project_membership":null,"worktree":null});
    f.evidence("task", None, "source_observation", &source.to_string());
    for view in [
        f.run(&["status"]).unwrap()["tasks"][0].clone(),
        f.run(&["show", "task"]).unwrap(),
    ] {
        assert_eq!(view["last_observed_source"]["stage"], "source_observation");
        assert_eq!(view["last_observed_source"]["category"], "source_read");
        assert_eq!(view["last_observed_source"]["code"], "source_read_failed");
        assert_eq!(view["last_observation"], view["last_observed_source"]);
        assert_eq!(view["last_observed_pr"]["stage"], "exit_pr_lookup");
        assert_eq!(view["reason"], "source reconciliation held");
        assert!(!view.to_string().contains(secret));
    }
}

pub(crate) fn malformed_observations_fail_closed_without_exposing_payload() {
    let f = Fixture::new();
    f.task("task");
    f.attempt("task", "attempt", "session");
    let secret = "private-token-in-untrusted-response";
    for (kind, attempt, payload) in [
        ("pause_pr_lookup", Some("attempt"), json!({"status":{"status":"error","category":"Transport","code":secret,"http_status":500},"observed_at_unix_secs":1700000000,"repository":"org/code"}).to_string()),
        ("exit_pr_lookup", Some("attempt"), format!("{{bad:{secret}")),
        ("source_observation", None, json!({"task_id":"task","status":"held","reasons":[secret]}).to_string()),
    ] {
        f.evidence("task", attempt, kind, &payload);
        for view in [f.run(&["status"]).unwrap()["tasks"][0].clone(),
            f.run(&["show", "task"]).unwrap()] {
            assert_eq!(view["last_observation"]["category"], "malformed");
            assert_eq!(view["reason"], "observation malformed");
            assert!(!view.to_string().contains(secret));
        }
    }
}

fn pr_cases() -> [(&'static str, &'static str, Value, &'static str); 7] {
    [
        (
            "pause_pr_lookup",
            "absent",
            json!({"status":"absent"}),
            "paused",
        ),
        (
            "pause_pr_lookup",
            "ambiguous",
            json!({"status":"ambiguous"}),
            "held",
        ),
        (
            "pause_pr_lookup",
            "error",
            json!({"status":"error","category":"RateLimit","code":"command-failed","http_status":429}),
            "held",
        ),
        (
            "pause_pr_lookup",
            "error",
            json!({"status":"error","category":"Malformed","code":"invalid-page","http_status":null}),
            "held",
        ),
        (
            "exit_pr_lookup",
            "absent",
            json!({"status":"absent"}),
            "attention",
        ),
        (
            "exit_pr_lookup",
            "ambiguous",
            json!({"status":"ambiguous"}),
            "held",
        ),
        (
            "exit_pr_lookup",
            "error",
            json!({"status":"error","category":"Transport","code":"transport-error","http_status":null}),
            "held",
        ),
    ]
}

fn assert_pr_view(
    view: &Value,
    summary: &str,
    kind: &str,
    category: &str,
    status: &Value,
    phase: &str,
    secret: &str,
) {
    assert_eq!(view["last_observed_pr"]["stage"], kind, "{summary}");
    assert_eq!(view["last_observed_pr"]["category"], category, "{summary}");
    assert_eq!(view["last_observed_pr"]["attempt_id"], "attempt");
    assert_eq!(
        view["last_observed_pr"]["observed_at_utc"],
        "2023-11-14T22:13:20Z"
    );
    assert_eq!(view["last_observation"], view["last_observed_pr"]);
    assert_eq!(view["phase"], phase);
    assert!(!view.to_string().contains(secret));
    assert!(!view.to_string().contains("secret-prompt"));
    if category == "error" {
        assert_eq!(
            view["last_observed_pr"]["code"],
            status["code"].as_str().unwrap()
        );
    }
    if phase != "held" {
        assert_eq!(view["reason"], Value::Null);
    } else {
        assert_ne!(view["reason"], "old hold");
    }
    if summary == "show" {
        assert_eq!(
            view["evidence"].as_array().unwrap().last().unwrap()["detail"],
            view["last_observed_pr"]
        );
    }
}
