use luthor::platform::{ProcessStatError, parse_linux_process_stat};

fn stat_fixture(name: &str, state: char, start: &str) -> String {
    let mut fields = vec![state.to_string()];
    fields.extend((0..18).map(|n| n.to_string()));
    fields.push(start.to_owned());
    format!("42 ({name}) {}\n", fields.join(" "))
}

#[test]
fn linux_process_observation_parses_start_ticks_after_parenthesized_command() {
    let stat = stat_fixture("worker ) with spaces", 'S', "987654");
    let observed = parse_linux_process_stat(&stat).unwrap();
    assert_eq!(observed.state, 'S');
    assert_eq!(observed.start_time_ticks, "987654");
}

#[test]
fn linux_process_observation_rejects_missing_identity_fields() {
    assert_eq!(
        parse_linux_process_stat("42 (worker) S"),
        Err(ProcessStatError::Malformed)
    );
    assert_eq!(
        parse_linux_process_stat("worker S 1 2 3"),
        Err(ProcessStatError::Malformed)
    );
}

fn bounded_sanitized_output(path: &std::path::Path) -> String {
    use std::fs;

    let bytes = fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let mut sanitized = String::new();
    let mut redact_next = false;
    for word in text.split_whitespace() {
        if redact_next {
            sanitized.push_str("[REDACTED]");
            redact_next = false;
        } else if word.eq_ignore_ascii_case("authorization:") || word.eq_ignore_ascii_case("bearer")
        {
            sanitized.push_str(word);
            sanitized.push(' ');
            sanitized.push_str("[REDACTED]");
            redact_next = true;
        } else {
            sanitized.push_str(word);
        }
        sanitized.push(' ');
    }
    if bytes.len() > 4096 {
        sanitized.push_str("[truncated]");
    }
    sanitized
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires an installed LLxprt rs binary; set LUTHOR_RS_BINARY"]
fn installed_rs_initial_turn_uses_private_config_and_loopback_provider() {
    use luthor::{
        config::{CommandTemplate, Config, Mapping, Marker, Source},
        eligibility::Candidate,
        state::StateStore,
        supervisor::{execute_with_binary, prepare_initial},
        worktree::ensure_worktree,
    };
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        path::Path,
        process::Command,
        thread,
        time::{Duration, Instant},
    };
    use tempfile::tempdir;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let binary = std::env::var_os("LUTHOR_RS_BINARY").expect("LUTHOR_RS_BINARY is required");
    let binary = Path::new(&binary);
    assert!(
        binary.is_absolute() && binary.is_file(),
        "LUTHOR_RS_BINARY must be an absolute file"
    );
    let dir = tempdir().unwrap();
    let config_root = dir.path().join("config-root");
    fs::create_dir_all(&config_root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut chunk = [0; 4096];
                    loop {
                        let n = stream.read(&mut chunk).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&chunk[..n]);
                        if let Some(split) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&bytes[..split]);
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            if bytes.len() >= split + 4 + length {
                                break;
                            }
                        }
                    }
                    request_tx
                        .send(String::from_utf8_lossy(&bytes).into_owned())
                        .unwrap();
                    let body = r#"{"id":"chatcmpl-test","object":"chat.completion","created":0,"model":"loopback","choices":[{"index":0,"message":{"role":"assistant","content":"Hello."},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                    stream.flush().unwrap();
                    return;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(error) => panic!("loopback provider did not receive a request: {error}"),
            }
        }
    });
    let profile = dir.path().join("loopback-profile.json");
    fs::write(
        &profile,
        serde_json::json!({
            "provider":"openai", "model":"loopback",
            "ephemeralSettings":{"base-url":format!("http://{address}"),"auth-key":"test"}
        })
        .to_string(),
    )
    .unwrap();
    let checkout = dir.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    git(&checkout, &["init", "-b", "main"]);
    git(&checkout, &["config", "user.name", "Fixture"]);
    git(&checkout, &["config", "user.email", "fixture@example.org"]);
    git(
        &checkout,
        &["remote", "add", "origin", "git@github.com:org/code.git"],
    );
    fs::write(checkout.join("README"), "fixture").unwrap();
    git(&checkout, &["add", "README"]);
    git(&checkout, &["commit", "-m", "fixture"]);
    let mapping = Mapping {
        tracker_repository: "org/tracker".into(),
        code_repository: "org/code".into(),
        checkout,
        base_branch: "main".into(),
        push_remote: "origin".into(),
        allowed_pr_head_repository: "org/code".into(),
        allowed_pr_author: "operator".into(),
    };
    let source = Source {
        project_id: "project".into(),
        repositories: vec!["org/tracker".into()],
        ready_marker: Marker::Label {
            name: "ready".into(),
        },
        milestone: None,
    };
    let config = Config {
        state_root: dir.path().join("state"),
        worktree_root: dir.path().join("worktrees"),
        capacity: 1,
        assignment_login: "operator".into(),
        sources: vec![source.clone()],
        mappings: vec![mapping.clone()],
        initial: CommandTemplate {
            executable: "/usr/bin/env".into(),
            args: vec![
                format!("LLXPRT_CONFIG_HOME={}", config_root.display()),
                binary.display().to_string(),
                "--profile-load".into(),
                profile.display().to_string(),
                "--session".into(),
                "{task.id}".into(),
                "--cwd".into(),
                "{worktree}".into(),
                "-p".into(),
                "Respond with a short greeting".into(),
            ],
        },
        resume: CommandTemplate {
            executable: "/usr/bin/true".into(),
            args: vec![],
        },
    };
    let candidate = Candidate {
        project_id: "project".into(),
        item_id: "item".into(),
        repository: "org/tracker".into(),
        issue_node_id: "issue".into(),
        issue_number: 7,
        issue_url: "https://github.com/org/tracker/issues/7".into(),
        tracker_repo_id: "repo".into(),
        milestone_id: None,
        milestone_title: None,
        observed_at_unix_secs: 1,
        observed_state: "open".into(),
        observed_assignees: vec![],
        observed_labels: vec!["ready".into()],
        observed_project_fields: vec![],
        marker: source.ready_marker.clone(),
        source,
        mapping,
    };
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    store
        .create_task("task", &candidate, "rev", &config)
        .unwrap();
    store
        .record_claim_intent("task", "operator", "org/tracker", 7)
        .unwrap();
    store
        .record_evidence("task", None, "claim_verified", "operator")
        .unwrap();
    store.set_task_phase("task", "claimed").unwrap();
    ensure_worktree(
        &mut store,
        "task",
        &config.worktree_root,
        &candidate.mapping,
    )
    .unwrap();
    let plan = prepare_initial(&mut store, "task", "installed-initial").unwrap();
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let receipt_path = config
        .state_root
        .join("attempts/installed-initial.receipt.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !receipt_path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        receipt_path.exists(),
        "real rs initial turn produced no exit receipt"
    );
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt_path).unwrap()).unwrap();
    assert_eq!(
        receipt.exit_code,
        Some(0),
        "installed rs initial turn failed; stdout={}, stderr={}",
        bounded_sanitized_output(&receipt.stdout_path),
        bounded_sanitized_output(&receipt.stderr_path),
    );
    let stdout = fs::metadata(&receipt.stdout_path).unwrap();
    let stderr = fs::metadata(&receipt.stderr_path).unwrap();
    assert_eq!(stdout.len(), receipt.stdout_bytes);
    assert_eq!(stderr.len(), receipt.stderr_bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(stdout.permissions().mode() & 0o777, 0o600);
        assert_eq!(stderr.permissions().mode() & 0o777, 0o600);
    }
    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("provider request");
    assert!(
        request.contains("/v1/chat/completions") || request.contains("/chat/completions"),
        "{request}"
    );
    assert!(
        request.contains("Respond with a short greeting"),
        "{request}"
    );
    server.join().unwrap();
    assert!(
        fs::read_dir(&config_root).unwrap().next().is_some(),
        "rs did not create session data under private config root"
    );
    assert!(!dir.path().join("gh-calls").exists());
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires an installed LLxprt rs binary; set LUTHOR_RS_BINARY"]
fn installed_rs_stop_uses_private_supervisor_and_reconciles() {
    use luthor::{
        config::{CommandTemplate, Config, Mapping, Marker, Source},
        eligibility::Candidate,
        state::StateStore,
        supervisor::{execute_with_binary, prepare_initial, prepare_resume},
        worktree::ensure_worktree,
    };
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        path::Path,
        process::Command,
        thread,
        time::{Duration, Instant},
    };
    use tempfile::tempdir;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let binary = std::env::var_os("LUTHOR_RS_BINARY").expect("LUTHOR_RS_BINARY is required");
    let binary = Path::new(&binary);
    assert!(
        binary.is_absolute() && binary.is_file(),
        "LUTHOR_RS_BINARY must be an absolute file"
    );
    let dir = tempdir().unwrap();
    let config_root = dir.path().join("config-root");
    fs::create_dir_all(&config_root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        for _ in 0..2 {
            loop {
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(20))
                        }
                        Err(error) => panic!(
                            "loopback provider did not receive a request before deadline: {error}"
                        ),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 4096];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) if bytes.is_empty() => break,
                        Ok(0) => break,
                        Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            panic!("loopback provider request read timed out")
                        }
                        Err(error) => panic!("loopback provider request read failed: {error}"),
                    }
                    if let Some(split) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..split]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= split + 4 + length {
                            break;
                        }
                    }
                }
                if bytes.is_empty() {
                    continue;
                }
                let split = bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .unwrap_or_else(|| panic!("loopback provider received nonempty malformed request without header terminator ({} bytes)", bytes.len()));
                let headers = String::from_utf8_lossy(&bytes[..split]);
                if !headers
                    .lines()
                    .next()
                    .is_some_and(|line| line.starts_with("POST "))
                {
                    panic!(
                        "loopback provider received malformed request line: {}",
                        headers.lines().next().unwrap_or("<empty>")
                    );
                }
                let request_line = headers.lines().next().unwrap();
                let body_start = split + 4;
                let request_body = String::from_utf8_lossy(&bytes[body_start..])
                    .chars()
                    .take(8192)
                    .collect::<String>();
                request_tx
                    .send(format!("{request_line}\n{request_body}"))
                    .unwrap();
                let body = r#"{"id":"chatcmpl-test","object":"chat.completion","created":0,"model":"loopback","choices":[{"index":0,"message":{"role":"assistant","content":"Hello again."},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#;
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                stream.flush().unwrap();
                break;
            }
        }
    });
    let profile = dir.path().join("loopback-profile.json");
    fs::write(
        &profile,
        serde_json::json!({
            "provider":"openai", "model":"loopback",
            "ephemeralSettings":{"base-url":format!("http://{address}"),"auth-key":"test"}
        })
        .to_string(),
    )
    .unwrap();
    let checkout = dir.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    git(&checkout, &["init", "-b", "main"]);
    git(&checkout, &["config", "user.name", "Fixture"]);
    git(&checkout, &["config", "user.email", "fixture@example.org"]);
    git(
        &checkout,
        &["remote", "add", "origin", "git@github.com:org/code.git"],
    );
    fs::write(checkout.join("README"), "fixture").unwrap();
    git(&checkout, &["add", "README"]);
    git(&checkout, &["commit", "-m", "fixture"]);
    let mapping = Mapping {
        tracker_repository: "org/tracker".into(),
        code_repository: "org/code".into(),
        checkout,
        base_branch: "main".into(),
        push_remote: "origin".into(),
        allowed_pr_head_repository: "org/code".into(),
        allowed_pr_author: "operator".into(),
    };
    let source = Source {
        project_id: "project".into(),
        repositories: vec!["org/tracker".into()],
        ready_marker: Marker::Label {
            name: "ready".into(),
        },
        milestone: None,
    };
    let config = Config {
        state_root: dir.path().join("state"),
        worktree_root: dir.path().join("worktrees"),
        capacity: 1,
        assignment_login: "operator".into(),
        sources: vec![source.clone()],
        mappings: vec![mapping.clone()],
        initial: CommandTemplate {
            executable: "/usr/bin/env".into(),
            args: vec![
                format!("LLXPRT_CONFIG_HOME={}", config_root.display()),
                binary.display().to_string(),
                "--profile-load".into(),
                profile.display().to_string(),
                "--session".into(),
                "{task.id}".into(),
                "--cwd".into(),
                "{worktree}".into(),
                "-p".into(),
                "Respond with a short greeting".into(),
            ],
        },
        resume: CommandTemplate {
            executable: "/usr/bin/env".into(),
            args: vec![
                format!("LLXPRT_CONFIG_HOME={}", config_root.display()),
                binary.display().to_string(),
                "--profile-load".into(),
                profile.display().to_string(),
                "--session".into(),
                "{task.id}".into(),
                "--cwd".into(),
                "{worktree}".into(),
                "-p".into(),
                "Distinct second turn after stop for {attempt.id}".into(),
            ],
        },
    };
    let candidate = Candidate {
        project_id: "project".into(),
        item_id: "item".into(),
        repository: "org/tracker".into(),
        issue_node_id: "issue".into(),
        issue_number: 7,
        issue_url: "https://github.com/org/tracker/issues/7".into(),
        tracker_repo_id: "repo".into(),
        milestone_id: None,
        milestone_title: None,
        observed_at_unix_secs: 1,
        observed_state: "open".into(),
        observed_assignees: vec![],
        observed_labels: vec!["ready".into()],
        observed_project_fields: vec![],
        marker: source.ready_marker.clone(),
        source,
        mapping,
    };
    let mut store = StateStore::open(&config.state_root, 1).unwrap();
    store
        .create_task("task", &candidate, "rev", &config)
        .unwrap();
    store
        .record_claim_intent("task", "operator", "org/tracker", 7)
        .unwrap();
    store
        .record_evidence("task", None, "claim_verified", "operator")
        .unwrap();
    store.set_task_phase("task", "claimed").unwrap();
    ensure_worktree(
        &mut store,
        "task",
        &config.worktree_root,
        &candidate.mapping,
    )
    .unwrap();
    let plan = prepare_initial(&mut store, "task", "installed-stop").unwrap();
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    let request = request_rx.recv_timeout(Duration::from_secs(20));
    if request.is_err() {
        let receipt_path = config
            .state_root
            .join("attempts/installed-stop.receipt.json");
        let intent = store.launch_intent("installed-stop").unwrap();
        eprintln!(
            "STOP diagnostic: fixture={}, receipt_exists={}, launch_intent={}",
            dir.path().display(),
            receipt_path.exists(),
            intent.as_deref().map(|_| "present").unwrap_or("absent")
        );
        if let Ok(bytes) = fs::read(&receipt_path)
            && let Ok(receipt) = serde_json::from_slice::<luthor::supervisor::ExitReceipt>(&bytes)
        {
            eprintln!(
                "STOP receipt: exit_code={:?}, signal={:?}, stop_signals={:?}, stdout_bytes={}, stderr_bytes={}",
                receipt.exit_code,
                receipt.signal,
                receipt.stop_signals,
                receipt.stdout_bytes,
                receipt.stderr_bytes
            );
            for (label, path) in [
                ("stdout", &receipt.stdout_path),
                ("stderr", &receipt.stderr_path),
            ] {
                let text = fs::read_to_string(path).unwrap_or_default();
                let safe_lines = text
                    .lines()
                    .map(|line| {
                        if line.to_ascii_lowercase().contains("token")
                            || line.to_ascii_lowercase().contains("authorization")
                        {
                            "[redacted sensitive log line]"
                        } else {
                            line
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                eprintln!("STOP {label} log:\n{safe_lines}");
            }
        }
    }
    let request = request.expect("provider request");
    assert!(
        request.contains("/v1/chat/completions") || request.contains("/chat/completions"),
        "{request}"
    );
    luthor::supervisor::request_stop(&mut store, "task", "installed-stop").unwrap();
    let receipt_path = config
        .state_root
        .join("attempts/installed-stop.receipt.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !receipt_path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        receipt_path.exists(),
        "real rs stop produced no durable receipt"
    );
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    assert!(
        !receipt.stop_signals.is_empty(),
        "supervisor sent no stop signals: {receipt:?}"
    );
    assert!(matches!(
        luthor::supervisor::reconcile_attempt(&mut store, "task", "installed-stop").unwrap(),
        luthor::supervisor::Reconciliation::Completed { .. }
    ));
    assert_eq!(store.reservation_count().unwrap(), 0);
    store
        .record_pause_pr_lookup(
            "task",
            "installed-stop",
            &luthor::state::PausePrEvidence {
                observed_at_unix_secs: 2,
                repository: "org/code".into(),
                status: luthor::state::PausePrStatus::Absent,
            },
        )
        .unwrap();
    assert_eq!(store.task_phase("task").unwrap().as_deref(), Some("paused"));
    drop(store);
    let mut reopened = StateStore::open(&config.state_root, 1).unwrap();
    let resume = prepare_resume(&mut reopened, "task", "installed-resume").unwrap();
    assert_eq!(resume.session_id, plan.session_id);
    assert_eq!(resume.worktree, plan.worktree);
    assert!(
        resume
            .args
            .join(" ")
            .contains("Distinct second turn after stop for installed-resume")
    );
    assert_eq!(reopened.reservation_count().unwrap(), 1);
    execute_with_binary(
        &mut reopened,
        &resume,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
    )
    .unwrap();
    let resume_receipt_path = config
        .state_root
        .join("attempts/installed-resume.receipt.json");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !resume_receipt_path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        resume_receipt_path.exists(),
        "real rs resume produced no exit receipt"
    );
    let resume_receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(&resume_receipt_path).unwrap()).unwrap();
    let stream_tail = |path: &Path| {
        let contents = fs::read_to_string(path).unwrap_or_default();
        let tail = contents.chars().rev().take(3000).collect::<String>();
        let tail = tail.chars().rev().collect::<String>();
        tail.lines()
            .map(|line| {
                if line.to_ascii_lowercase().contains("authorization")
                    || line.to_ascii_lowercase().contains("api-key")
                {
                    "[redacted authentication line]"
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        resume_receipt.exit_code,
        Some(0),
        "rs resume stdout tail:\n{}\nrs resume stderr tail:\n{}",
        stream_tail(&resume_receipt.stdout_path),
        stream_tail(&resume_receipt.stderr_path)
    );
    let resumed_request = request_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("resume provider request");
    assert!(
        resumed_request.contains("Distinct second turn after stop for installed-resume"),
        "{resumed_request}"
    );
    assert!(
        resumed_request.contains(&plan.session_id),
        "session id missing: {resumed_request}"
    );
    assert!(
        resumed_request.contains(&plan.worktree.display().to_string()),
        "worktree missing: {resumed_request}"
    );
    for (path, expected) in [
        (&resume_receipt.stdout_path, resume_receipt.stdout_bytes),
        (&resume_receipt.stderr_path, resume_receipt.stderr_bytes),
    ] {
        assert_eq!(fs::metadata(path).unwrap().len(), expected);
    }
    assert!(matches!(
        luthor::supervisor::reconcile_attempt(&mut reopened, "task", "installed-resume").unwrap(),
        luthor::supervisor::Reconciliation::Completed { .. }
    ));
    assert_eq!(reopened.reservation_count().unwrap(), 0);
    let stdout = fs::metadata(&receipt.stdout_path).unwrap();
    let stderr = fs::metadata(&receipt.stderr_path).unwrap();
    assert_eq!(stdout.len(), receipt.stdout_bytes);
    assert_eq!(stderr.len(), receipt.stderr_bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(stdout.permissions().mode() & 0o777, 0o600);
        assert_eq!(stderr.permissions().mode() & 0o777, 0o600);
    }
    server.join().unwrap();
    assert!(
        fs::read_dir(&config_root).unwrap().next().is_some(),
        "rs did not create session data under private config root"
    );
    assert!(!dir.path().join("gh-calls").exists());
}
