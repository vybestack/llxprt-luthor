use super::{Lane, database, executable_lane};
use luthor::state::{journal, scheduling};
use luthor::supervisor;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
};

fn commit_plan(lane: &mut Lane) {
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    let proof = lane
        .f
        .store
        .begin_amended_supervision(&context, &lane.f.config, "corrected-revision")
        .unwrap();
    let path = lane
        .f
        .store
        .root()
        .join("attempts/attempt-task-a.plan.json");
    fs::write(&path, serde_json::to_vec(&proof.effective_plan).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(target_os = "macos")]
pub(super) fn identity(pid: u32) -> (String, String) {
    let boot = Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.bootsessionuuid"])
        .output()
        .unwrap();
    assert!(boot.status.success());
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    assert_eq!(
        unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        },
        size
    );
    (
        format!(
            "darwin-bootsessionuuid:{}",
            String::from_utf8(boot.stdout)
                .unwrap()
                .trim()
                .to_ascii_lowercase()
        ),
        format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
    )
}

#[cfg(target_os = "linux")]
pub(super) fn identity(pid: u32) -> (String, String) {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    (
        boot.trim().into(),
        stat.rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .into(),
    )
}

pub(super) fn ready_supervisor(lane: &mut Lane) -> std::process::Child {
    commit_plan(lane);
    ready_committed_supervisor(lane)
}

pub(super) fn ready_committed_supervisor(lane: &mut Lane) -> std::process::Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_luthor"))
        .arg("__supervise")
        .arg(lane.f.store.root())
        .arg("attempt-task-a")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "READY\n");
    let registration = fs::read_to_string(
        lane.f
            .store
            .root()
            .join("attempts/attempt-task-a.child.json"),
    )
    .unwrap();
    journal::record_evidence(
        &mut lane.f.store,
        "task-a",
        Some("attempt-task-a"),
        "child_registered",
        registration.trim(),
    )
    .unwrap();
    let (boot, start) = identity(child.id());
    let process = serde_json::json!({"pid":child.id(),"boot_identity":boot,"start_identity":start})
        .to_string();
    journal::record_evidence(
        &mut lane.f.store,
        "task-a",
        Some("attempt-task-a"),
        "supervisor_ready",
        &process,
    )
    .unwrap();
    journal::record_intent(
        &mut lane.f.store,
        "gate-attempt-task-a",
        "task-a",
        Some("attempt-task-a"),
        "gate_release",
        &process,
    )
    .unwrap();
    child
}

#[test]
fn amended_ready_gate_rechecks_audit_seal_dispatch_and_plan_before_worker_exec() {
    for sql in [
        "DELETE FROM evidence WHERE kind='initial_branch_removed'",
        "DELETE FROM intents WHERE kind='initial_branch_removal_seal'",
        "UPDATE intents SET detail=json_set(detail,'$.amendment_sequence',99999) WHERE kind='supervisor_dispatch'",
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) SELECT task_id,attempt_id,kind,payload FROM evidence WHERE kind='initial_branch_removed'",
        "UPDATE intents SET detail=json_set(detail,'$.audit_payload','changed') WHERE kind='initial_branch_removal_seal'",
        "UPDATE evidence SET attempt_id='foreign' WHERE kind='child_registered'",
        "UPDATE intents SET detail=json_set(detail,'$.effective_plan.args[0]','forged') WHERE kind='supervisor_dispatch'",
    ] {
        let mut lane = executable_lane();
        let mut child = ready_supervisor(&mut lane);
        database(&lane).execute_batch(sql).unwrap();
        child.stdin.take().unwrap().write_all(b"R").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success(), "{sql}");
        assert!(
            fs::read(
                lane.f
                    .store
                    .root()
                    .join("attempts/attempt-task-a.stdout.log")
            )
            .unwrap()
            .is_empty(),
            "{sql}"
        );
        assert!(
            !lane
                .f
                .store
                .root()
                .join("attempts/attempt-task-a.receipt.json")
                .exists()
        );
        assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
    }
}

#[test]
fn amended_ready_eof_leaves_dispatch_held_without_worker_or_receipt() {
    let mut lane = executable_lane();
    let mut child = ready_supervisor(&mut lane);
    drop(child.stdin.take());
    assert!(!child.wait().unwrap().success());
    assert!(
        fs::read(
            lane.f
                .store
                .root()
                .join("attempts/attempt-task-a.stdout.log")
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        !lane
            .f
            .store
            .root()
            .join("attempts/attempt-task-a.receipt.json")
            .exists()
    );
    assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
    assert!(matches!(
        supervisor::reconcile_attempt(&mut lane.f.store, "task-a", "attempt-task-a").unwrap(),
        supervisor::Reconciliation::Held { .. }
    ));
}

#[test]
fn amended_raw_worker_plan_and_dispatch_without_registration_cannot_exec() {
    for committed in [false, true] {
        let mut lane = executable_lane();
        if committed {
            commit_plan(&mut lane);
        } else {
            let mut plan = lane.plan.clone();
            plan.args.drain(2..4);
            let path = lane
                .f
                .store
                .root()
                .join("attempts/attempt-task-a.plan.json");
            fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_luthor"))
            .arg("__worker_gate")
            .arg(
                lane.f
                    .store
                    .root()
                    .join("attempts/attempt-task-a.plan.json"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"R").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(scheduling::reservation_count(&lane.f.store).unwrap(), 1);
    }
}

#[test]
fn amended_conflicting_artifact_is_not_overwritten_or_dispatched() {
    let mut lane = executable_lane();
    let context = lane
        .f
        .store
        .never_dispatched_context("task-a", "attempt-task-a")
        .unwrap();
    let path = lane
        .f
        .store
        .root()
        .join("attempts/attempt-task-a.plan.json");
    fs::write(&path, "forged").unwrap();
    assert!(
        supervisor::execute_amended_with_binary(
            &mut lane.f.store,
            &context,
            &lane.f.config,
            "corrected-revision",
            Path::new(env!("CARGO_BIN_EXE_luthor"))
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "forged");
    assert_eq!(super::dispatch_count(&lane), 0);
}
