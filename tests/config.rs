use luthor::config::Config;

fn valid() -> &'static str {
    r#"{"state_root":"/private/state","worktree_root":"/private/worktrees","capacity":2,"sources":[{"project_id":"PVT_1","repositories":["org/tracker"],"ready_marker":{"kind":"label","name":"luthor-ready"},"milestone":"0.12.0"}],"mappings":[{"tracker_repository":"org/tracker","code_repository":"org/code","checkout":"/src/code","base_branch":"main"}],"initial":{"executable":"/bin/agent","args":["--issue","{task.issue_number}"]},"resume":{"executable":"/bin/agent","args":["--issue","{task.issue_number}","--attempt","{attempt.id}"]}}"#
}

#[test]
fn accepts_valid_configuration_and_exact_optional_milestone() {
    let config = Config::from_json(valid()).unwrap();
    assert_eq!(config.sources[0].milestone.as_deref(), Some("0.12.0"));
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
fn milestone_can_be_omitted() {
    let json = valid().replace(",\"milestone\":\"0.12.0\"", "");
    assert!(
        Config::from_json(&json).unwrap().sources[0]
            .milestone
            .is_none()
    );
}
