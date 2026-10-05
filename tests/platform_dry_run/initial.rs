use super::*;

pub(crate) fn run() {
    let fixture = NativeFixture::new();
    let (request_rx, server, profile) = start_provider(fixture.dir.path(), false);
    let (config, candidate) = fixture_config(fixture.dir.path(), &fixture.binary, &profile, false);
    let mut store = claimed_store(&config, &candidate);
    let plan = prepare_initial(&mut store, "task", "installed-initial").unwrap();
    execute_with_binary(&mut store, &plan, Path::new(env!("CARGO_BIN_EXE_luthor"))).unwrap();
    assert_initial_turn(&fixture, &config, request_rx, server);
}

fn assert_initial_turn(
    fixture: &NativeFixture,
    config: &Config,
    request_rx: std::sync::mpsc::Receiver<String>,
    server: thread::JoinHandle<()>,
) {
    let config_root = &fixture.config_root;
    let dir = &fixture.dir;
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
        fs::read_dir(config_root).unwrap().next().is_some(),
        "rs did not create session data under private config root"
    );
    assert!(!dir.path().join("gh-calls").exists());
}
