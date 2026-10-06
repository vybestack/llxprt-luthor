use super::*;
use luthor::state::{journal, launches, scheduling};

#[cfg(unix)]
pub(crate) fn branch_switch_after_ready_blocks_release_and_holds_slot() {
    use std::{
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    let attempts = config.state_root.join("attempts");
    fs::DirBuilder::new().mode(0o700).create(&attempts).unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempts.join("attempt-real.plan.json"))
        .unwrap();
    serde_json::to_writer(&mut file, &plan).unwrap();
    file.sync_all().unwrap();
    let owner = luthor::WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    let proof = owner
        .protocol_evidence(store.root(), "task", "attempt-real")
        .unwrap();
    launches::begin_supervision(
        &mut store,
        "task",
        "attempt-real",
        &serde_json::to_string(&plan).unwrap(),
        &proof,
    )
    .unwrap();
    drop(owner);
    let owner = luthor::WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
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
    let mut child = command.spawn().unwrap();
    drop(owner);
    let mut ready = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "READY\n");
    git(&plan.worktree, &["switch", "-c", "foreign"]);
    record_ready_release_evidence(&mut store, &mut child, &attempts);
    child.stdin.take().unwrap().write_all(b"R").unwrap();
    assert_release_blocked(child, &marker, &attempts, &store);
}

#[cfg(unix)]
fn record_ready_release_evidence(
    store: &mut StateStore,
    child: &mut std::process::Child,
    attempts: &Path,
) {
    let registered = fs::read_to_string(attempts.join("attempt-real.child.json")).unwrap();
    journal::record_evidence(
        store,
        "task",
        Some("attempt-real"),
        "child_registered",
        registered.trim(),
    )
    .unwrap();
    let (boot, start) = test_process_identity(child.id());
    let process = serde_json::json!({"pid":child.id(),"boot_identity":boot,"start_identity":start});
    journal::record_evidence(
        store,
        "task",
        Some("attempt-real"),
        "supervisor_ready",
        &process.to_string(),
    )
    .unwrap();
    journal::record_intent(
        store,
        "gate-attempt-real",
        "task",
        Some("attempt-real"),
        "gate_release",
        &process.to_string(),
    )
    .unwrap();
}

#[cfg(unix)]
fn assert_release_blocked(
    mut child: std::process::Child,
    marker: &Path,
    attempts: &Path,
    store: &StateStore,
) {
    assert!(!child.wait().unwrap().success());
    assert!(!marker.exists());
    assert!(!attempts.join("attempt-real.receipt.json").exists());
    assert_eq!(scheduling::reservation_count(store).unwrap(), 1);
}

#[cfg(unix)]
pub(crate) fn same_binary_ready_without_release_does_not_launch_worker() {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = tempfile::tempdir().unwrap();
    let (config, mut store, plan, marker) = prepared_fake_worker(&dir);
    let attempts = config.state_root.join("attempts");
    fs::DirBuilder::new().mode(0o700).create(&attempts).unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempts.join("attempt-real.plan.json"))
        .unwrap();
    serde_json::to_writer(&mut file, &plan).unwrap();
    file.sync_all().unwrap();
    fs::File::open(&attempts).unwrap().sync_all().unwrap();
    let owner = luthor::WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    let proof = owner
        .protocol_evidence(store.root(), "task", "attempt-real")
        .unwrap();
    launches::begin_supervision(
        &mut store,
        "task",
        "attempt-real",
        &serde_json::to_string(&plan).unwrap(),
        &proof,
    )
    .unwrap();
    drop(owner);
    let owner = luthor::WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
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
    let mut child = command.spawn().unwrap();
    drop(owner);
    let mut ready = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "READY\n");
    drop(child.stdin.take());
    assert!(!child.wait().unwrap().success());
    assert!(!marker.exists());
    assert!(attempts.join("attempt-real.supervisor-error.json").exists());
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
}
