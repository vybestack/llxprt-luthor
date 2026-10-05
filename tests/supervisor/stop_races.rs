use super::*;
use luthor::state::journal;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn pause_after_natural_exit_preserves_natural_attention_path() {
    let (_dir, config, mut store, plan, _cleanup) = natural_stop_race_fixture();
    let receipt = release_natural_exit(&config);
    assert!(
        journal::stop_intent(&store, "task", "attempt-real")
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        request_stop(&mut store, "task", "attempt-real"),
        Ok(()) | Err(SupervisorError::StopUnavailable)
    ));
    assert_natural_stop_accounted(&config, store, &plan, &receipt);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn pause_intent_before_natural_exit_before_signal_preserves_attention_path() {
    let (_dir, config, mut store, plan, _cleanup) = natural_stop_race_fixture();
    let stop = luthor::supervisor::prepare_stop(&mut store, "task", "attempt-real").unwrap();
    let db = rusqlite::Connection::open_with_flags(
        config.state_root.join("state.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let intent: String = db.query_row(
        "SELECT detail FROM intents WHERE kind='stop' AND task_id='task' AND attempt_id='attempt-real'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&intent).unwrap(),
        serde_json::json!({"task_id":"task","attempt_id":"attempt-real"})
    );
    assert!(!receipt_path(&config).exists());
    let child: serde_json::Value = serde_json::from_slice(
        &fs::read(config.state_root.join("attempts/attempt-real.child.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        unsafe { libc::kill(-(child["pid"].as_i64().unwrap() as i32), 0) },
        0
    );
    let receipt = release_natural_exit(&config);
    assert!(matches!(
        stop.finish(),
        Ok(()) | Err(SupervisorError::StopUnavailable)
    ));
    assert_natural_stop_accounted(&config, store, &plan, &receipt);
}
