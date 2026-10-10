use luthor::config::{CommandTemplate, Config, ConfigError, Marker, TaskValues};

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

mod validation_characterization {
    use super::*;

    fn assert_invalid(config: &Config, expected: &str) {
        match config.validate().unwrap_err() {
            ConfigError::Invalid(message) => assert_eq!(message, expected),
            other => panic!("expected ConfigError::Invalid, got {other}"),
        }
    }

    fn invalid_after(change: impl FnOnce(&mut Config), expected: &str) {
        let mut config = Config::from_json(valid()).unwrap();
        change(&mut config);
        assert_invalid(&config, expected);
    }

    #[test]
    fn capacity_collections_and_logins_keep_exact_errors() {
        let required = "capacity, sources and mappings must be non-empty";
        invalid_after(|c| c.capacity = 0, required);
        invalid_after(|c| c.sources.clear(), required);
        invalid_after(|c| c.mappings.clear(), required);
        for login in ["", " ", "bad/login", "bot@example", "böt"] {
            invalid_after(
                |c| c.assignment_login = login.into(),
                "invalid assignment_login",
            );
            invalid_after(
                |c| c.mappings[0].allowed_pr_author = login.into(),
                "invalid allowed_pr_author",
            );
        }
        let mut config = Config::from_json(valid()).unwrap();
        config.assignment_login = "Bot_1-2".into();
        config.mappings[0].allowed_pr_author = "Author_1-2".into();
        config.validate().unwrap();
    }

    #[test]
    fn sources_keep_required_fields_and_marker_errors() {
        let required = "source project_id and repositories are required";
        for id in ["", " "] {
            invalid_after(|c| c.sources[0].project_id = id.into(), required);
        }
        invalid_after(|c| c.sources[0].repositories.clear(), required);
        for empty in ["", " "] {
            invalid_after(
                |c| c.sources[0].ready_marker = Marker::Label { name: empty.into() },
                "empty label marker",
            );
            for (name, value) in [(empty, "Ready"), ("Status", empty)] {
                invalid_after(
                    |c| {
                        c.sources[0].ready_marker = Marker::ProjectField {
                            name: name.into(),
                            value: value.into(),
                        }
                    },
                    "project marker name and value are required",
                );
            }
            invalid_after(
                |c| c.sources[0].milestone = Some(empty.into()),
                "milestone cannot be empty",
            );
        }
        let mut config = Config::from_json(valid()).unwrap();
        config.sources[0].ready_marker = Marker::ProjectField {
            name: "Status".into(),
            value: "Ready".into(),
        };
        Config::from_json(&serde_json::to_string(&config).unwrap()).unwrap();
        invalid_after(
            |c| c.sources.push(c.sources[0].clone()),
            "duplicate source project_id",
        );
        invalid_after(
            |c| c.sources[0].repositories[0] = "org/unmapped".into(),
            "source repository has no mapping",
        );
    }

    #[test]
    fn mappings_keep_exact_repository_path_and_remote_errors() {
        invalid_after(
            |c| c.mappings.push(c.mappings[0].clone()),
            "duplicate mapping",
        );
        invalid_after(
            |c| c.mappings[0].checkout = "".into(),
            "mapping checkout and base_branch are required",
        );
        for branch in ["", " "] {
            invalid_after(
                |c| c.mappings[0].base_branch = branch.into(),
                "mapping checkout and base_branch are required",
            );
        }
        for repository in [
            "",
            "org",
            "/repo",
            "org/",
            "org/repo/extra",
            "org/re po",
            "org/répö",
        ] {
            invalid_after(
                |c| c.sources[0].repositories[0] = repository.into(),
                "invalid repository name",
            );
            invalid_after(
                |c| c.mappings[0].tracker_repository = repository.into(),
                "source repository has no mapping",
            );
            invalid_after(
                |c| c.mappings[0].code_repository = repository.into(),
                "invalid repository name",
            );
            invalid_after(
                |c| c.mappings[0].allowed_pr_head_repository = repository.into(),
                "invalid repository name",
            );
        }
        for (remotes, expected) in [
            (
                &[
                    "",
                    "bad remote",
                    "-origin",
                    "org/../repo",
                    "ssh://host/repo?query",
                ][..],
                "invalid push_remote",
            ),
            (
                &["ftp://host/repo", "ssh://user@host/repo", "https://host"][..],
                "invalid push_remote URL",
            ),
        ] {
            for remote in remotes {
                invalid_after(|c| c.mappings[0].push_remote = (*remote).into(), expected);
            }
        }
    }

    #[test]
    fn config_first_error_order_is_stable() {
        let mut c = Config::from_json(valid()).unwrap();
        c.capacity = 0;
        c.assignment_login = "bad/login".into();
        c.sources[0].project_id.clear();
        c.sources[0].repositories = vec!["bad".into()];
        c.sources[0].ready_marker = Marker::Label { name: "".into() };
        c.sources[0].milestone = Some("".into());
        c.mappings[0].code_repository = "bad".into();
        c.mappings[0].checkout = "".into();
        c.mappings[0].push_remote = "-bad".into();
        c.mappings[0].allowed_pr_head_repository = "bad".into();
        c.mappings[0].allowed_pr_author.clear();
        c.initial.executable = "".into();
        c.resume.args = vec!["--unknown".into()];
        assert_invalid(&c, "capacity, sources and mappings must be non-empty");
        c.capacity = 1;
        assert_invalid(&c, "invalid assignment_login");
        c.assignment_login = "bot".into();
        assert_invalid(&c, "source project_id and repositories are required");
        c.sources[0].project_id = "PVT_1".into();
        assert_invalid(&c, "invalid repository name");
        c.sources[0].repositories = vec!["org/unmapped".into()];
        assert_invalid(&c, "source repository has no mapping");
        c.sources[0].repositories = vec!["org/tracker".into()];
        assert_invalid(&c, "empty label marker");
        c.sources[0].ready_marker = Marker::Label {
            name: "Ready".into(),
        };
        assert_invalid(&c, "milestone cannot be empty");
        c.sources[0].milestone = None;
        assert_invalid(&c, "invalid repository name");
        c.mappings[0].code_repository = "org/code".into();
        assert_invalid(&c, "mapping checkout and base_branch are required");
        c.mappings[0].checkout = "/src/code".into();
        assert_invalid(&c, "invalid push_remote");
        c.mappings[0].push_remote = "origin".into();
        assert_invalid(&c, "invalid repository name");
        c.mappings[0].allowed_pr_head_repository = "org/fork".into();
        assert_invalid(&c, "invalid allowed_pr_author");
        c.mappings[0].allowed_pr_author = "alice".into();
        assert_invalid(&c, "command executable is required");
        c.initial.executable = "/bin/llxprt-code-rs".into();
        assert_invalid(&c, "unrecognized worker argument option");
    }

    fn with_worker_args(resume: bool, args: &[&str]) -> Config {
        let mut config = Config::from_json(valid()).unwrap();
        let command = if resume {
            &mut config.resume
        } else {
            &mut config.initial
        };
        command.args = args.iter().map(|arg| (*arg).into()).collect();
        config
    }

    #[test]
    fn initial_and_resume_parser_keep_exact_errors_and_precedence() {
        let cases: &[(&[&str], &str)] = &[
            (&["--prompt"], "worker option value is missing"),
            (&["--prompt", "--help"], "worker option value is missing"),
            (
                &["--prompt", "--unknown"],
                "unrecognized worker argument option",
            ),
            (
                &["--prompt", "--header"],
                "forbidden worker argument option",
            ),
            (
                &["--prompt", "--env=value"],
                "forbidden worker argument option",
            ),
            (
                &["--allow-shell=yes"],
                "worker option does not accept a value",
            ),
            (&["--help=yes"], "worker option does not accept a value"),
            (&[""], "invalid worker argument"),
            (&["--help", ""], "invalid worker argument"),
            (&["--prompt", "-1"], "unrecognized worker argument option"),
            (
                &["--prompt", "--help=yes"],
                "worker option value is missing",
            ),
        ];
        for resume in [false, true] {
            for (args, expected) in cases {
                assert_invalid(&with_worker_args(resume, args), expected);
            }
            for args in [
                vec!["--prompt=literal", "--cwd", "{worktree}"],
                vec!["--prompt", "literal", "--cwd={worktree}"],
                vec!["--prompt=", "--prompt", ""],
                vec!["--allow-shell", "--help", "--version"],
                vec!["--max-tool-calls", "-1", "--prompt", "literal"],
            ] {
                let config = with_worker_args(resume, &args);
                config.validate().unwrap();
                let command = if resume {
                    &config.resume
                } else {
                    &config.initial
                };
                let expected: Vec<_> = args
                    .iter()
                    .map(|arg| arg.replace("{worktree}", &task_values().worktree))
                    .collect();
                assert_eq!(command.render(&task_values()).unwrap().args, expected);
            }
        }
    }

    #[test]
    fn localoauth_is_a_no_value_flag_and_oauth_login_stays_rejected() {
        for resume in [false, true] {
            let args = ["--localoauth", "--prompt", "literal"];
            with_worker_args(resume, &args).validate().unwrap();
            assert_invalid(
                &with_worker_args(resume, &["--localoauth=x"]),
                "worker option does not accept a value",
            );
            assert_invalid(
                &with_worker_args(resume, &["--oauth-login"]),
                "unrecognized worker argument option",
            );
        }
    }

    #[test]
    fn initial_and_resume_budgets_keep_exact_errors_and_run_first() {
        let error = "--max-tool-calls requires exactly one value: -1 or 1..512";
        for resume in [false, true] {
            for value in ["-1", "1", "512", "0", "513", "1024", "no", "{attempt.id}"] {
                for args in [
                    vec!["--max-tool-calls".into(), value.into()],
                    vec![format!("--max-tool-calls={value}")],
                ] {
                    let refs: Vec<_> = args.iter().map(String::as_str).collect();
                    let config = with_worker_args(resume, &refs);
                    if ["-1", "1", "512"].contains(&value) {
                        config.validate().unwrap();
                    } else {
                        assert_invalid(&config, error);
                    }
                }
            }
            for args in [
                vec!["--max-tool-calls"],
                vec!["--max-tool-calls", "--help"],
                vec!["--unknown", "--max-tool-calls=0"],
                vec!["--max-tool-calls=1", "--max-tool-calls=-1"],
                vec!["--max-tool-calls", "512", "--max-tool-calls=1"],
            ] {
                let mut config = with_worker_args(resume, &args);
                let command = if resume {
                    &mut config.resume
                } else {
                    &mut config.initial
                };
                command.executable = "".into();
                assert_invalid(&config, error);
            }
        }
    }

    #[test]
    fn duplicate_checks_keep_per_item_order_before_fields() {
        let mut c = Config::from_json(valid()).unwrap();
        c.sources.push(c.sources[0].clone());
        c.sources[1].repositories[0] = "bad".into();
        assert_invalid(&c, "duplicate source project_id");
        c.sources[1].project_id.clear();
        assert_invalid(&c, "source project_id and repositories are required");
        c.sources.pop();
        c.mappings.push(c.mappings[0].clone());
        c.mappings[1].checkout = "".into();
        assert_invalid(&c, "duplicate mapping");
        c.mappings[1].code_repository = "bad".into();
        assert_invalid(&c, "invalid repository name");
        c.mappings[1].code_repository = "org/code".into();
        c.mappings[1].tracker_repository = "org/other".into();
        assert_invalid(&c, "duplicate code repository mapping");
        c.mappings[1].tracker_repository = "bad".into();
        assert_invalid(&c, "invalid repository name");
        // Earlier items finish validation before later items' duplicate checks.
        c.sources[0].milestone = Some("".into());
        assert_invalid(&c, "milestone cannot be empty");
    }

    #[test]
    fn initial_and_resume_credentials_stay_redacted_and_ordered() {
        for resume in [false, true] {
            for arg in [
                "PRIVATE-TOKEN: ISSUE12_SENTINEL",
                "API_KEY=ISSUE12_SENTINEL",
            ] {
                let config = with_worker_args(resume, &["--prompt", arg]);
                assert_invalid(&config, "credential-bearing command argument is forbidden");
                assert!(
                    !config
                        .validate()
                        .unwrap_err()
                        .to_string()
                        .contains("ISSUE12_SENTINEL")
                );
            }
            let mut config = with_worker_args(resume, &["--unknown"]);
            let command = if resume {
                &mut config.resume
            } else {
                &mut config.initial
            };
            command.executable = "/bin/agent-SECRET-ISSUE12_SENTINEL".into();
            assert_invalid(
                &config,
                "credential-bearing command executable is forbidden",
            );
            let command = if resume {
                &mut config.resume
            } else {
                &mut config.initial
            };
            command.executable = "".into();
            assert_invalid(&config, "command executable is required");
        }
    }
}
