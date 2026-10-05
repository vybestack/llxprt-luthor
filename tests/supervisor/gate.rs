use super::*;

#[cfg(unix)]
pub(crate) fn gate_eof_never_spawns_and_does_not_write_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("launched");
    let executable = dir.path().join("worker");
    fs::write(
        &executable,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let plan = luthor::supervisor::LaunchPlan {
        task_id: "task".into(),
        attempt_id: "held".into(),
        session_id: "s".into(),
        worktree: dir.path().into(),
        expected_worktree: direct_snapshot(dir.path()),
        executable,
        args: vec![],
        config_revision: "rev".into(),
        session_environment: luthor::supervisor::SessionEnvironment {
            home: dir.path().into(),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_state_home: None,
            llxprt_config_home: None,
        },
    };
    assert!(matches!(
        run_gated_child_with_binary(
            &plan,
            Cursor::new(Vec::<u8>::new()),
            dir.path(),
            Path::new(env!("CARGO_BIN_EXE_luthor"))
        ),
        Err(SupervisorError::GateClosed)
    ));
    assert!(!marker.exists());
    assert!(!dir.path().join("held.receipt.json").exists());
}

#[cfg(unix)]
pub(crate) fn released_gate_captures_durable_logs_and_receipts_real_exit() {
    for code in [0, 7] {
        let dir = tempfile::tempdir().unwrap();
        let attempt = format!("exit-{code}");
        let plan = fake_plan(dir.path(), &attempt, code);
        let status = run_gated_child_with_binary(
            &plan,
            Cursor::new(b"R"),
            dir.path(),
            Path::new(env!("CARGO_BIN_EXE_luthor")),
        )
        .unwrap();
        assert_eq!(
            status.code(),
            Some(code),
            "{}",
            fs::read_to_string(dir.path().join(format!("{attempt}.stderr.log"))).unwrap()
        );
        let stdout_path = dir.path().join(format!("{attempt}.stdout.log"));
        let stderr_path = dir.path().join(format!("{attempt}.stderr.log"));
        assert_eq!(
            fs::read(&stdout_path).unwrap(),
            format!("out-{attempt}\n").as_bytes()
        );
        assert_eq!(
            fs::read(&stderr_path).unwrap(),
            format!("err-{attempt}\n").as_bytes()
        );
        let receipt_path = dir.path().join(format!("{attempt}.receipt.json"));
        let bytes = fs::read(&receipt_path).unwrap();
        let receipt: luthor::supervisor::ExitReceipt = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipt.exit_code, Some(code));
        assert_eq!(receipt.signal, None);
        assert_eq!(
            receipt.stdout_bytes,
            fs::metadata(stdout_path).unwrap().len()
        );
        assert_eq!(
            receipt.stderr_bytes,
            fs::metadata(stderr_path).unwrap().len()
        );
        assert!(
            receipt.child_pid > 0
                && !receipt.boot_identity.is_empty()
                && !receipt.child_start_identity.is_empty()
        );
        assert_eq!(
            receipt.stdout_path,
            dir.path().join(format!("{attempt}.stdout.log"))
        );
    }
}
