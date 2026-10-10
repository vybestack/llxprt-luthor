use super::*;
use luthor::WorktreeOwner;
use luthor::state::scheduling;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn completed_worker(dir: &tempfile::TempDir) -> (Config, StateStore) {
    let (config, mut store, plan, marker) = prepared_fake_worker(dir);
    let owner = WorktreeOwner::acquire(store.root(), &plan.task_id).unwrap();
    execute_with_binary(
        &mut store,
        &plan,
        Path::new(env!("CARGO_BIN_EXE_luthor")),
        &owner,
    )
    .unwrap();
    let receipt = receipt_path(&config);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() || !receipt.exists() {
        assert!(
            Instant::now() < deadline,
            "worker did not finish after gate release"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let exit: luthor::supervisor::ExitReceipt =
        serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert_eq!(exit.exit_code, Some(0));
    assert_eq!(fs::read(exit.stdout_path).unwrap(), b"worker stdout\n");
    assert_eq!(fs::read(exit.stderr_path).unwrap(), b"worker stderr\n");
    drop(owner);
    loop {
        match WorktreeOwner::acquire_existing(store.root(), &plan.task_id) {
            Ok(probe) => {
                drop(probe);
                break;
            }
            Err(luthor::OwnershipError::Busy) => {
                assert!(
                    Instant::now() < deadline,
                    "worker owner was not released after receipt"
                );
                thread::sleep(Duration::from_millis(20));
            }
            Err(luthor::OwnershipError::Unavailable) => {
                panic!("worktree owner became unavailable after receipt")
            }
        }
    }
    (config, store)
}

fn replace_supervisor(config: &Config, pid: u32) {
    let db = rusqlite::Connection::open(config.state_root.join("state.sqlite3")).unwrap();
    let original: String = db.query_row(
        "SELECT payload FROM evidence WHERE kind='supervisor_ready' AND attempt_id='attempt-real'",
        [], |row| row.get(0),
    ).unwrap();
    let mut record: serde_json::Value = serde_json::from_str(&original).unwrap();
    record["pid"] = pid.into();
    record["start_identity"] = "not-the-recorded-process-start".into();
    let payload = serde_json::to_string(&record).unwrap();
    assert_eq!(db.execute(
        "UPDATE evidence SET payload=?1 WHERE attempt_id='attempt-real' AND kind IN ('supervisor_ready','gate_sent')",
        [&payload],
    ).unwrap(), 2);
    assert_eq!(
        db.execute(
            "UPDATE intents SET detail=?1 WHERE attempt_id='attempt-real' AND kind='gate_release'",
            [&payload],
        )
        .unwrap(),
        1
    );
}

fn assert_held_after_restart(config: &Config, mut store: StateStore, reason: &str) {
    assert_eq!(
        reconcile_attempt(&mut store, "task", "attempt-real").unwrap(),
        Reconciliation::Held {
            reason: reason.into()
        }
    );
    assert_eq!(scheduling::reservation_count(&store).unwrap(), 1);
    drop(store);
    let mut reopened = StateStore::open(&config.state_root, 1).unwrap();
    assert_eq!(
        reconcile_attempt(&mut reopened, "task", "attempt-real").unwrap(),
        Reconciliation::Held {
            reason: reason.into()
        }
    );
    assert_eq!(scheduling::reservation_count(&reopened).unwrap(), 1);
}

#[test]
fn receipt_with_mismatched_supervisor_identity_keeps_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let (config, store) = completed_worker(&dir);
    replace_supervisor(&config, std::process::id());
    assert_held_after_restart(&config, store, "supervisor identity mismatch");
}

struct GroupCleanup(i32);
impl Drop for GroupCleanup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

#[test]
fn unavailable_supervisor_identity_with_surviving_group_keeps_reservation() {
    let dir = tempfile::tempdir().unwrap();
    let (config, store) = completed_worker(&dir);
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "sleep 30 & echo ready"])
        .stdout(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut leader = command.spawn().unwrap();
    let pid = leader.id();
    let _cleanup = GroupCleanup(i32::try_from(pid).unwrap());
    let mut ready = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(leader.stdout.take().unwrap()),
        &mut ready,
    )
    .unwrap();
    assert_eq!(ready, "ready\n");
    assert!(leader.wait().unwrap().success());
    assert_eq!(unsafe { libc::kill(-i32::try_from(pid).unwrap(), 0) }, 0);
    replace_supervisor(&config, pid);
    assert_held_after_restart(&config, store, "supervisor identity unavailable");
}
