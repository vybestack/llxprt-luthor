use super::*;
use luthor::state::journal::{evidence_kinds, record_evidence, record_intent, stop_intent};
use luthor::state::launches::begin_supervision;
use luthor::state::scheduling::reservation_count;

#[cfg(unix)]
pub(crate) fn registered_shim_stays_gated_and_survives_supervisor_crash_after_release() {
    #[cfg(target_os = "linux")]
    if std::env::var_os("LUTHOR_STOP_TEST_SUBREAPER").is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("registered_shim_stays_gated_and_survives_supervisor_crash_after_release")
            .env("LUTHOR_STOP_TEST_SUBREAPER", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated subreaper test process failed: {output:?}"
        );
        return;
    }
    #[cfg(target_os = "linux")]
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    fs::write(
        &plan.executable,
        format!(
            "#!/bin/sh\necho started > '{}'\nexec /bin/sleep 30\n",
            marker.display()
        ),
    )
    .unwrap();
    let attempts = config.state_root.join("attempts");
    install_gate_plan(&mut store, &plan, &attempts);
    let mut supervisor = start_gated_supervisor(&config);
    let identity_path = attempts.join("attempt-real.child.json");
    let identity: serde_json::Value =
        serde_json::from_slice(&fs::read(&identity_path).unwrap()).unwrap();
    let pid = identity["pid"].as_i64().unwrap() as i32;
    assert_eq!(identity["group_id"].as_i64().unwrap(), i64::from(pid));
    assert_eq!(unsafe { libc::getpgid(pid) }, pid);
    #[cfg(target_os = "linux")]
    let reaper = start_worker_reaper(pid);
    assert!(!marker.exists());
    let other_owner = luthor::WorktreeOwner::acquire(&config.state_root, "other-task").unwrap();
    authorize_registered_gate(&mut store, &identity_path, &supervisor, &marker);
    release_then_crash_supervisor(&mut supervisor, &marker, pid);
    assert_worker_identity_after_crash(&identity, &identity_path, pid);
    assert_owner_busy(&config.state_root, "supervisor reaped while worker lives");
    drop(other_owner);
    assert!(matches!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held { .. }
    ));
    assert_eq!(reservation_count(&store).unwrap(), 1);
    request_stop(&mut store, "task", "attempt-real").unwrap_or_else(|error| {
        panic!(
            "request_stop failed: {error}; evidence_kinds={:?}",
            evidence_kinds(&store, "task").unwrap()
        )
    });
    #[cfg(target_os = "linux")]
    assert!(
        reaper.join().unwrap(),
        "adopted worker was not reaped within the bound"
    );
    assert_owner_acquirable(&config.state_root);
    assert!(!attempts.join("attempt-real.receipt.json").exists());
    assert!(
        stop_intent(&store, "task", "attempt-real")
            .unwrap()
            .is_some()
    );
    assert_eq!(unsafe { libc::kill(-pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert!(!attempts.join("attempt-real.receipt.json").exists());
    assert_crashed_supervisor_reservation(&config, store);
}

#[cfg(unix)]
fn assert_owner_busy(root: &Path, context: &str) {
    assert!(
        matches!(
            luthor::WorktreeOwner::acquire_existing(root, "task"),
            Err(luthor::OwnershipError::Busy)
        ),
        "production worker owner lock was not Busy: {context}"
    );
}

#[cfg(unix)]
fn assert_owner_acquirable(root: &Path) {
    let owner = luthor::WorktreeOwner::acquire_existing(root, "task")
        .expect("production worker owner lock remained Busy after worker reaping");
    drop(owner);
}

#[cfg(unix)]
fn assert_worker_identity_after_crash(
    identity: &serde_json::Value,
    identity_path: &Path,
    pid: i32,
) {
    let (boot, start) = test_process_identity(pid as u32);
    assert_eq!(identity["boot_identity"], boot);
    assert_eq!(identity["start_identity"], start);
    assert_eq!(unsafe { libc::getpgid(pid) }, pid);
    assert_eq!(
        *identity,
        serde_json::from_slice::<serde_json::Value>(&fs::read(identity_path).unwrap()).unwrap()
    );
}

#[cfg(unix)]
fn install_gate_plan(
    store: &mut StateStore,
    plan: &luthor::supervisor::LaunchPlan,
    attempts: &Path,
) {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    fs::DirBuilder::new().mode(0o700).create(attempts).unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempts.join("attempt-real.plan.json"))
        .unwrap();
    serde_json::to_writer(&mut file, plan).unwrap();
    file.sync_all().unwrap();
    fs::File::open(attempts).unwrap().sync_all().unwrap();
    let owner = luthor::WorktreeOwner::acquire(store.root(), "task").unwrap();
    begin_supervision(
        store,
        "task",
        "attempt-real",
        &serde_json::to_string(&plan).unwrap(),
        &owner
            .protocol_evidence(store.root(), "task", "attempt-real")
            .unwrap(),
    )
    .unwrap();
    drop(owner);
}
#[cfg(unix)]
fn start_gated_supervisor(config: &Config) -> std::process::Child {
    let owner = luthor::WorktreeOwner::acquire(&config.state_root, "task").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_luthor"));
    command
        .args([
            "__supervise",
            config.state_root.to_str().unwrap(),
            "attempt-real",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    owner.inherit_into(&mut command);
    let mut supervisor = command.spawn().unwrap();
    drop(owner);
    let mut ready = String::new();
    std::io::BufReader::new(supervisor.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "READY\n");
    supervisor
}
#[cfg(unix)]
fn authorize_registered_gate(
    store: &mut StateStore,
    identity_path: &Path,
    supervisor: &std::process::Child,
    marker: &Path,
) {
    record_evidence(
        store,
        "task",
        Some("attempt-real"),
        "child_registered",
        fs::read_to_string(identity_path).unwrap().trim(),
    )
    .unwrap();
    assert!(!marker.exists());
    let (boot, start) = test_process_identity(supervisor.id());
    let process =
        serde_json::json!({"pid":supervisor.id(),"boot_identity":boot,"start_identity":start});
    record_evidence(
        store,
        "task",
        Some("attempt-real"),
        "supervisor_ready",
        &process.to_string(),
    )
    .unwrap();
    record_intent(
        store,
        "gate-attempt-real",
        "task",
        Some("attempt-real"),
        "gate_release",
        &process.to_string(),
    )
    .unwrap();
    assert!(!marker.exists());
}
#[cfg(unix)]
fn release_then_crash_supervisor(supervisor: &mut std::process::Child, marker: &Path, pid: i32) {
    use std::io::Write;
    supervisor.stdin.take().unwrap().write_all(b"R").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if marker.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    if !marker.exists() {
        let supervisor_status = supervisor.try_wait();
        let _ = supervisor.kill();
        let _ = supervisor.wait();
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        assert!(
            marker.exists(),
            "fake worker marker was not created before deadline; supervisor.try_wait() = {supervisor_status:?}"
        );
    }
    assert!(marker.exists());
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
}

#[cfg(target_os = "linux")]
fn start_worker_reaper(pid: i32) -> thread::JoinHandle<bool> {
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let result = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
            if result == pid {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    })
}
