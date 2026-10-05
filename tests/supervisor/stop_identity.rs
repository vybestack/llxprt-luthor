use super::*;
use luthor::state::{journal, launches, scheduling};

#[cfg(unix)]
pub(crate) fn spoofed_child_identity_does_not_signal_live_group() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    launches::begin_supervision(
        &mut store,
        "task",
        "attempt-real",
        &serde_json::to_string(&plan).unwrap(),
    )
    .unwrap();
    let mut dead = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let (boot, start) = test_process_identity(dead.id());
    dead.kill().unwrap();
    dead.wait().unwrap();
    let supervisor =
        serde_json::json!({"pid":dead.id(),"boot_identity":boot,"start_identity":start});
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "supervisor_ready",
        &supervisor.to_string(),
    )
    .unwrap();
    journal::record_intent(
        &mut store,
        "gate-attempt-real",
        "task",
        Some("attempt-real"),
        "gate_release",
        &supervisor.to_string(),
    )
    .unwrap();
    let mut live = Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let (boot, _) = test_process_identity(live.id());
    let spoof = serde_json::json!({"pid":live.id(),"boot_identity":boot,"start_identity":"recycled","group_id":live.id()});
    let attempts = config.state_root.join("attempts");
    fs::create_dir_all(&attempts).unwrap();
    fs::write(attempts.join("attempt-real.child.json"), spoof.to_string()).unwrap();
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "child_registered",
        &spoof.to_string(),
    )
    .unwrap();
    assert!(matches!(
        request_stop(&mut store, "task", "attempt-real"),
        Err(SupervisorError::StopUnavailable)
    ));
    assert!(live.try_wait().unwrap().is_none());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"independent_stop_decision".into())
    );
    live.kill().unwrap();
    live.wait().unwrap();
}

#[cfg(unix)]
pub(crate) fn matching_supervisor_without_socket_does_not_fallback_to_group_signal() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, _) = prepared_fake_worker(&dir);
    launches::begin_supervision(
        &mut store,
        "task",
        "attempt-real",
        &serde_json::to_string(&plan).unwrap(),
    )
    .unwrap();
    let (boot, start) = test_process_identity(std::process::id());
    let supervisor =
        serde_json::json!({"pid":std::process::id(),"boot_identity":boot,"start_identity":start});
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "supervisor_ready",
        &supervisor.to_string(),
    )
    .unwrap();
    journal::record_intent(
        &mut store,
        "gate-attempt-real",
        "task",
        Some("attempt-real"),
        "gate_release",
        &supervisor.to_string(),
    )
    .unwrap();
    let mut live = Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let (boot, start) = test_process_identity(live.id());
    let child = serde_json::json!({"pid":live.id(),"boot_identity":boot,"start_identity":start,"group_id":live.id()});
    let attempts = config.state_root.join("attempts");
    fs::create_dir_all(&attempts).unwrap();
    fs::write(attempts.join("attempt-real.child.json"), child.to_string()).unwrap();
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "child_registered",
        &child.to_string(),
    )
    .unwrap();
    assert!(matches!(
        request_stop(&mut store, "task", "attempt-real"),
        Err(SupervisorError::StopUnavailable)
    ));
    assert!(live.try_wait().unwrap().is_none());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .contains(&"independent_stop_decision".into())
    );
    live.kill().unwrap();
    live.wait().unwrap();
}
