use super::*;
use luthor::state::{journal, scheduling, task_records};

#[cfg(unix)]
pub(crate) fn live_tracked_descendant_holds_valid_receipt_reconciliation() {
    let (_dir, _config, mut store) = dispatched_fixture(0);
    let (boot_identity, start_identity) = test_process_identity(std::process::id());
    let tracked = serde_json::json!({
        "pid": std::process::id(),
        "boot_identity": boot_identity,
        "start_identity": start_identity,
    });
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "tracked_descendant",
        &tracked.to_string(),
    )
    .unwrap();

    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "tracked descendant is alive"
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn escaped_tracked_descendant_holds_valid_receipt_until_reaped() {
    let (_dir, config, mut store) = dispatched_fixture(7);
    let receipt: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt_path(&config)).unwrap()).unwrap();
    assert_eq!(receipt.exit_code, Some(7));
    assert_eq!(unsafe { libc::kill(-(receipt.child_pid as i32), 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );

    let mut worker = escaped_worker();
    let pid = worker.child.id();
    let (boot_identity, start_identity) = worker.identity.clone();
    let tracked = serde_json::json!({
        "pid": pid,
        "boot_identity": boot_identity,
        "start_identity": start_identity,
    });
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "tracked_descendant",
        &tracked.to_string(),
    )
    .unwrap();
    assert_eq!(
        observed_process_identity(pid).as_ref(),
        Some(&(boot_identity.clone(), start_identity.clone()))
    );
    assert!(worker.child.try_wait().unwrap().is_none());
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "tracked descendant is alive"
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_ne!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("completed")
    );
    assert!(matches!(
        scheduling::ensure_dispatch_capacity(&store),
        Err(StateError::Capacity { .. })
    ));
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );

    assert_eq!(unsafe { libc::getpgid(pid as i32) }, pid as i32);
    assert_eq!(
        observed_process_identity(pid),
        Some((boot_identity, start_identity))
    );
    assert_eq!(unsafe { libc::kill(-(pid as i32), libc::SIGKILL) }, 0);
    let status = worker.child.wait().unwrap();
    assert!(!status.success());
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Completed {
            exit_code: Some(7),
            signal: None
        }
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 0);
}

#[cfg(unix)]
pub(crate) fn malformed_tracked_identity_holds_valid_receipt_reconciliation() {
    let (_dir, _config, mut store) = dispatched_fixture(0);
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "tracked_descendant",
        r#"{"pid":0,"boot_identity":"boot","start_identity":"start"}"#,
    )
    .unwrap();

    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "invalid tracked descendant identity"
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

pub(crate) fn malformed_tracked_descendant_evidence_holds_missing_receipt_attempt() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    fs::remove_file(receipt_path(&config)).unwrap();
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "tracked_descendant",
        "{malformed",
    )
    .unwrap();

    assert!(matches!(
        inspect_recovery_quiescence(&store, "task", "attempt-real").unwrap(),
        RecoveryInspection::Held("invalid tracked descendant identity")
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason == "invalid tracked descendant identity"
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

#[cfg(unix)]
pub(crate) fn live_tracked_descendant_prevents_missing_receipt_absence_reconciliation() {
    let (_dir, config, mut store) = dispatched_fixture(0);
    fs::remove_file(receipt_path(&config)).unwrap();
    let (boot_identity, start_identity) = test_process_identity(std::process::id());
    let tracked = serde_json::json!({
        "pid": std::process::id(),
        "boot_identity": boot_identity,
        "start_identity": start_identity,
    });
    journal::record_evidence(
        &mut store,
        "task",
        Some("attempt-real"),
        "tracked_descendant",
        &tracked.to_string(),
    )
    .unwrap();

    assert!(matches!(
        inspect_recovery_quiescence(&store, "task", "attempt-real").unwrap(),
        RecoveryInspection::Held("registered processes may still be live")
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { reason } if reason != "receipt missing; registered processes absent; operator recovery required"
    ));
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    assert_eq!(
        task_records::task_phase(&store, "task").unwrap().as_deref(),
        Some("held")
    );
    assert!(
        !journal::evidence_kinds(&store, "task")
            .unwrap()
            .iter()
            .any(|kind| kind == "attempt_exit")
    );
}

struct EscapedWorker {
    child: std::process::Child,
    identity: (String, String),
}

impl Drop for EscapedWorker {
    fn drop(&mut self) {
        let pid = self.child.id() as i32;
        if unsafe { libc::getpgid(pid) } == pid
            && observed_process_identity(self.child.id()).as_ref() == Some(&self.identity)
        {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        let _ = self.child.wait();
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn escaped_worker() -> EscapedWorker {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new("/bin/sleep");
    command
        .arg("10")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    let pid = child.id();
    let (boot_identity, start_identity) = test_process_identity(pid);
    let identity = (boot_identity.clone(), start_identity.clone());
    let worker = EscapedWorker { child, identity };
    assert_eq!(unsafe { libc::getpgid(pid as i32) }, pid as i32);
    worker
}
