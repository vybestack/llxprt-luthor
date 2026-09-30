use luthor::config::{CommandTemplate, Config, TaskValues};

#[test]
fn documented_json_example_loads_and_round_trips() {
    let docs = include_str!("../dev-docs/config-and-state.md");
    let json = docs
        .split("```json\n")
        .nth(1)
        .and_then(|example| example.split("\n```").next())
        .expect("documentation contains a fenced JSON example");
    let config = Config::from_json(json).unwrap();
    let mapping = &config.mappings[0];
    assert_eq!(mapping.push_remote, "git@github-acoliver:example/code.git");
    assert_eq!(mapping.allowed_pr_head_repository, "example/code-fork");
    assert_eq!(mapping.allowed_pr_author, "acoliver");
    assert_eq!(config.assignment_login, "example-agent");

    let snapshot = serde_json::to_string(&config).unwrap();
    let round_tripped = Config::from_json(&snapshot).unwrap();
    assert_eq!(round_tripped.mappings[0], *mapping);

    let initial_values = TaskValues {
        task_issue_number: "7".into(),
        task_repository: "example/tracker".into(),
        task_issue_url: "https://github.com/example/tracker/issues/7".into(),
        task_id: "task-7f3a".into(),
        attempt_id: "attempt-initial".into(),
        worktree: "/example/worktrees/task-7f3a".into(),
    };
    let resume_values = TaskValues {
        attempt_id: "attempt-resume-02".into(),
        ..initial_values.clone()
    };
    let initial = config.initial.render(&initial_values).unwrap();
    let resume = config.resume.render(&resume_values).unwrap();

    for rendered in [&initial, &resume] {
        let session = rendered
            .args
            .windows(2)
            .find(|pair| pair[0] == "--session")
            .expect("documented command has a session flag");
        assert_eq!(session[1], initial_values.task_id);
        let cwd = rendered
            .args
            .windows(2)
            .find(|pair| pair[0] == "--cwd")
            .expect("documented command has a cwd flag");
        assert_eq!(cwd[1], initial_values.worktree);
    }

    let initial_prompt = initial
        .args
        .last()
        .expect("documented initial command has a prompt");
    let resume_prompt = resume
        .args
        .last()
        .expect("documented resume command has a prompt");
    assert_ne!(initial_prompt, resume_prompt);
    assert!(resume_prompt.contains(&resume_values.attempt_id));
    assert!(resume_prompt.contains(&resume_values.task_issue_url));
}

fn valid() -> &'static str {
    r#"{"state_root":"/private/state","worktree_root":"/private/worktrees","capacity":2,"assignment_login":"bot","sources":[{"project_id":"PVT_1","repositories":["org/tracker"],"ready_marker":{"kind":"label","name":"luthor-ready"},"milestone":"0.12.0"}],"mappings":[{"tracker_repository":"org/tracker","code_repository":"org/code","checkout":"/src/code","base_branch":"main","push_remote":"origin","allowed_pr_head_repository":"org/fork","allowed_pr_author":"alice"}],"initial":{"executable":"/bin/llxprt-code-rs","args":["--prompt","Work on {task.issue_url}","--cwd","{worktree}"]},"resume":{"executable":"/bin/llxprt-code-rs","args":["--session","{attempt.id}","--cwd","{worktree}","-p","Continue {task.issue_url}"]}}"#
}

#[test]
fn accepts_valid_configuration_and_exact_optional_milestone() {
    let config = Config::from_json(valid()).unwrap();
    assert_eq!(config.sources[0].milestone.as_deref(), Some("0.12.0"));
}

#[test]
fn accepts_in_repo_and_fork_pr_head_repositories() {
    let both_same = valid()
        .replace("org/code", "org/tracker")
        .replace("org/fork", "org/tracker");
    assert!(Config::from_json(&both_same).is_ok());

    let head_is_code = valid().replace("org/fork", "org/code");
    assert!(Config::from_json(&head_is_code).is_ok());

    assert!(Config::from_json(valid()).is_ok());
}

#[test]
fn rejects_invalid_pr_head_repository_syntax() {
    let invalid = valid().replace("org/fork", "invalid/repository/name");
    assert!(Config::from_json(&invalid).is_err());
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
    let json = valid().replace("{task.issue_url}", "$(curl bad)");
    assert!(
        Config::from_json(&json)
            .unwrap_err()
            .to_string()
            .contains("shell")
    );
    let sentinel = "PRIVATE_SENTINEL_SECRET_BYTES";
    let json = valid().replace("{task.issue_url}", &format!("{{{sentinel}}}"));
    let error = Config::from_json(&json).unwrap_err().to_string();
    assert!(error.contains("unsupported"));
    assert!(!error.contains(sentinel));
}

#[test]
fn rejects_embedded_credential_fields() {
    let json = valid().replace("\"capacity\":2", "\"capacity\":2,\"token\":\"secret\"");
    assert!(Config::from_json(&json).is_err());
}

#[test]
fn rejects_credential_headers_and_executable_secrets_without_echoing_values() {
    let mut value: serde_json::Value = serde_json::from_str(valid()).unwrap();
    value["initial"]["args"] = serde_json::json!(["--prompt", "PRIVATE-TOKEN: DEMO_VALUE"]);
    let error = Config::from_json(&value.to_string())
        .unwrap_err()
        .to_string();
    assert!(!error.contains("DEMO_VALUE"));
    value["initial"]["args"] =
        serde_json::json!(["--prompt", "Authorization: Bearer SECRET_MARKER"]);
    let error = Config::from_json(&value.to_string())
        .unwrap_err()
        .to_string();
    assert!(!error.contains("SECRET_MARKER"));
    let secret_executable = valid().replace("/bin/llxprt-code-rs", "/bin/agent-SECRET_MARKER");
    let error = Config::from_json(&secret_executable)
        .unwrap_err()
        .to_string();
    assert!(!error.contains("SECRET_MARKER"));
}

#[test]
fn rejects_secret_flags_bad_braces_and_accepts_benign_prompt() {
    for bad in [
        "credential=abc",
        "{task.issue_url}}",
        "{{task.issue_url}",
        "{task.issue_url",
    ] {
        assert!(
            Config::from_json(&valid().replace("{task.issue_url}", bad)).is_err(),
            "accepted {bad}"
        );
    }
    assert!(
        Config::from_json(&valid().replace("{task.issue_url}", "Please fix this issue")).is_ok()
    );
    for prompt in [
        "Fix {task.issue_url} using API",
        "Tracker-Issue: {task.issue_url}",
    ] {
        assert!(Config::from_json(&valid().replace("Work on {task.issue_url}", prompt)).is_ok());
    }
    for prompt in ["PRIVATE-TOKEN:", "X-API-KEY:", "password:"] {
        assert!(Config::from_json(&valid().replace("Work on {task.issue_url}", prompt)).is_err());
    }
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

#[test]
fn rejects_header_env_and_unknown_flags_without_echoing_values() {
    for args in [
        vec!["--header", "PRIVATE-TOKEN: DEMO_VALUE"],
        vec!["-H", "PRIVATE-TOKEN: DEMO_VALUE"],
        vec!["--header=PRIVATE-TOKEN: DEMO_VALUE"],
        vec!["--env", "PRIVATE_TOKEN=DEMO_VALUE"],
        vec!["-e", "TOKEN=DEMO_VALUE"],
        vec!["--env=TOKEN=DEMO_VALUE"],
        vec!["--unrecognized", "DEMO_VALUE"],
    ] {
        let mut value: serde_json::Value = serde_json::from_str(valid()).unwrap();
        value["initial"]["args"] = serde_json::json!(args);
        let error = Config::from_json(&value.to_string())
            .unwrap_err()
            .to_string();
        assert!(!error.contains("DEMO_VALUE"), "{error}");
    }
}

#[test]
fn accepts_verified_rs_arguments_and_renders_task_placeholders() {
    let config = Config::from_json(valid()).unwrap();
    let rendered = config.initial.render(&task_values()).unwrap();
    assert_eq!(
        rendered.args,
        [
            "--prompt",
            "Work on https://example.test/issues/42",
            "--cwd",
            "/tmp/worktree"
        ]
    );
}

#[test]
fn native_tool_budget_rejects_unsupported_values_and_accepts_unlimited() {
    for value in ["-1", "1", "512", "0", "513", "1024", "no", "{attempt.id}"] {
        for inline in [false, true] {
            let mut config = Config::from_json(valid()).unwrap();
            if inline {
                config.resume.args.push(format!("--max-tool-calls={value}"));
            } else {
                config
                    .resume
                    .args
                    .extend(["--max-tool-calls".into(), value.into()]);
            }
            assert_eq!(
                config.validate().is_ok(),
                ["-1", "1", "512"].contains(&value),
                "{value}, inline={inline}"
            );
        }
    }
    let mut config = Config::from_json(valid()).unwrap();
    config
        .resume
        .args
        .extend(["--max-tool-calls=512".into(), "--max-tool-calls=-1".into()]);
    assert!(config.validate().is_err());
}
