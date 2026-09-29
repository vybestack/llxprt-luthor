use luthor::config::{CommandTemplate, Config, TaskValues};

fn valid() -> &'static str {
    r#"{"state_root":"/private/state","worktree_root":"/private/worktrees","capacity":2,"sources":[{"project_id":"PVT_1","repositories":["org/tracker"],"ready_marker":{"kind":"label","name":"luthor-ready"},"milestone":"0.12.0"}],"mappings":[{"tracker_repository":"org/tracker","code_repository":"org/code","checkout":"/src/code","base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/fork","allowed_pr_author":"alice"}],"initial":{"executable":"/bin/agent","args":["--issue","{task.issue_number}"]},"resume":{"executable":"/bin/agent","args":["--issue","{task.issue_number}","--attempt","{attempt.id}"]}}"#
}

#[test]
fn accepts_valid_configuration_and_exact_optional_milestone() {
    let config = Config::from_json(valid()).unwrap();
    assert_eq!(config.sources[0].milestone.as_deref(), Some("0.12.0"));
}

#[test]
fn rejects_unmapped_source_repository_and_duplicate_project_sources() {
    let unmapped = valid().replace(
        r#""repositories":["org/tracker"]"#,
        r#""repositories":["org/elsewhere"]"#,
    );
    assert!(
        Config::from_json(&unmapped)
            .unwrap_err()
            .to_string()
            .contains("no mapping")
    );
    let duplicate = valid().replace(
        r#"}],"mappings""#,
        r#"},{"project_id":"PVT_1","repositories":["org/tracker"],"ready_marker":{"kind":"label","name":"ready"},"milestone":null}],"mappings""#,
    );
    assert!(Config::from_json(&duplicate).is_err());
}

#[test]
fn rejects_duplicate_code_repository_mappings() {
    let duplicate = valid().replace(
        r#"}],"initial""#,
        r#"},{"tracker_repository":"org/other","code_repository":"org/code","checkout":"/src/other","base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/fork","allowed_pr_author":"alice"}],"initial""#,
    );
    assert!(Config::from_json(&duplicate).is_err());
}

#[test]
fn rejects_unknown_template_and_shell_expansion() {
    let json = valid().replace("{task.issue_number}", "$(curl bad)");
    assert!(
        Config::from_json(&json)
            .unwrap_err()
            .to_string()
            .contains("shell")
    );
    let json = valid().replace("{task.issue_number}", "{task.secret}");
    assert!(
        Config::from_json(&json)
            .unwrap_err()
            .to_string()
            .contains("unsupported")
    );
}

#[test]
fn rejects_embedded_credential_fields() {
    let json = valid().replace("\"capacity\":2", "\"capacity\":2,\"token\":\"secret\"");
    assert!(Config::from_json(&json).is_err());
}

#[test]
fn rejects_credential_headers_and_executable_secrets_without_echoing_values() {
    let safe = valid().replace(r#""--issue""#, r#""--session""#).replace(
        r#""--session","{task.issue_number}""#,
        r#""--session","session-1","--profile","default","Fix this task""#,
    );
    assert!(Config::from_json(&safe).is_ok());

    for (json, marker) in [
        (
            valid()
                .replace("\"--issue\"", "\"--header\"")
                .replace("{task.issue_number}", "Authorization: Bearer SECRET_MARKER"),
            "SECRET_MARKER",
        ),
        (
            valid().replace("{task.issue_number}", "Authorization=Bearer SECRET_MARKER"),
            "SECRET_MARKER",
        ),
        (
            valid().replace("{task.issue_number}", "--api-key=SECRET_MARKER"),
            "SECRET_MARKER",
        ),
        (
            valid().replace("/bin/agent", "/bin/agent-SECRET_MARKER"),
            "SECRET_MARKER",
        ),
    ] {
        let error = Config::from_json(&json).unwrap_err().to_string();
        assert!(
            !error.contains(marker),
            "error leaked credential marker: {error}"
        );
    }
}

#[test]
fn rejects_secret_flags_bad_braces_and_accepts_benign_prompt() {
    for bad in [
        "--api-key=abc",
        "--token",
        "--auth-key=abc",
        "credential=abc",
        "{task.issue_number}}",
        "{{task.issue_number}",
        "{task.issue_number",
    ] {
        assert!(
            Config::from_json(&valid().replace("{task.issue_number}", bad)).is_err(),
            "accepted {bad}"
        );
    }
    assert!(
        Config::from_json(&valid().replace("{task.issue_number}", "Please fix this issue")).is_ok()
    );
}

fn task_values() -> TaskValues {
    TaskValues {
        task_issue_number: "42".into(),
        task_repository: "org/repo".into(),
        task_issue_url: "https://example.test/issues/42".into(),
        task_id: "task-1".into(),
        attempt_id: "attempt-2".into(),
        worktree: "/tmp/worktree".into(),
    }
}

#[test]
fn renders_multiple_placeholders_as_literal_argv() {
    let template = CommandTemplate {
        executable: "/bin/agent".into(),
        args: vec!["{task.repository}#{task.issue_number}:{task.repository}".into()],
    };
    let rendered = template.render(&task_values()).unwrap();
    assert_eq!(rendered.executable, std::path::PathBuf::from("/bin/agent"));
    assert_eq!(rendered.args, ["org/repo#42:org/repo"]);
}

#[test]
fn substituted_braces_are_literal() {
    let template = CommandTemplate {
        executable: "/bin/agent".into(),
        args: vec!["{task.issue_url}".into()],
    };
    let mut values = task_values();
    values.task_issue_url = "https://example.test/{literal}".into();
    assert_eq!(
        template.render(&values).unwrap().args,
        ["https://example.test/{literal}"]
    );
}

#[test]
fn missing_template_value_fails_without_exposing_values() {
    let template = CommandTemplate {
        executable: "/bin/agent".into(),
        args: vec!["{attempt.id}".into()],
    };
    let mut values = task_values();
    values.attempt_id.clear();
    let error = template.render(&values).unwrap_err().to_string();
    assert!(error.contains("missing template value"));
    assert!(!error.contains("42"));
}

#[test]
fn milestone_can_be_omitted() {
    let json = valid().replace(",\"milestone\":\"0.12.0\"", "");
    assert!(
        Config::from_json(&json).unwrap().sources[0]
            .milestone
            .is_none()
    );
}
