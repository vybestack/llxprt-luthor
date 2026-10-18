//! PID reuse must not obstruct terminal queue history, but live identities do.
use crate::capacity_fixture::Fixture;
use luthor::state::StateError;
use serde_json::{Value, json};
use std::{
    os::unix::process::CommandExt,
    process::{Child, Command},
};

struct Process(Child);

impl Process {
    fn new() -> Self {
        Self(
            Command::new("/bin/sleep")
                .arg("300")
                .process_group(0)
                .spawn()
                .unwrap(),
        )
    }

    fn registration(&self) -> Value {
        let (boot, start) = identity(self.0.id());
        json!({"pid": self.0.id(), "group_id": self.0.id(), "boot_identity": boot, "start_identity": start})
    }
}

impl Process {
    fn stop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn identity(pid: u32) -> (String, String) {
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
    assert_eq!(info.pbi_pid, pid);
    assert_ne!(info.pbi_start_tvsec, 0);
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
pub(crate) fn identity(pid: u32) -> (String, String) {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    let stat = luthor::platform::observe_linux_process(pid).unwrap();
    (boot.trim().into(), stat.start_time_ticks)
}

fn register(f: &Fixture, kind: &str, registration: &Value) {
    f.evidence(
        "task",
        Some("attempt-task"),
        kind,
        &registration.to_string(),
    );
}

#[test]
fn terminal_reused_pid_and_recycled_group_do_not_block_resize_or_change_history() {
    let process = Process::new();
    for kind in ["child_registered", "supervisor_ready", "tracked_descendant"] {
        let f = Fixture::new(2);
        f.completed("task", 1);
        f.db()
            .execute("UPDATE tasks SET state='pr_merged'", [])
            .unwrap();
        let mut registration = process.registration();
        registration["start_identity"] = "previous-process-start".into();
        register(&f, kind, &registration);
        let before = f.history();
        let audit = f.change(2, 3, 0).unwrap();
        assert!(audit.running_attempts.is_empty());
        assert_eq!(f.history(), before);
        assert_eq!(f.audit_count(), 1);
        assert!(unsafe { libc::kill(process.0.id() as i32, 0) } == 0);
    }
}

#[test]
fn terminal_matching_pid_and_group_still_block_resize_without_mutation() {
    let process = Process::new();
    for kind in ["child_registered", "supervisor_ready", "tracked_descendant"] {
        let f = Fixture::new(2);
        f.completed("task", 1);
        register(&f, kind, &process.registration());
        let before = f.history();
        assert!(matches!(
            f.change(2, 3, 0),
            Err(StateError::CapacityChangeRefused(
                "registered process may be active; reconcile first"
            ))
        ));
        assert_eq!(f.history(), before);
        assert_eq!(f.audit_count(), 0);
    }
}

#[test]
fn registrations_from_a_previous_boot_do_not_block_resize() {
    let process = Process::new();
    let f = Fixture::new(2);
    f.completed("task", 1);
    let mut registration = process.registration();
    let boot = registration["boot_identity"].as_str().unwrap();
    let other = "11111111-2222-3333-4444-555555555555";
    assert!(!boot.ends_with(other));
    registration["boot_identity"] = if boot.starts_with("darwin-bootsessionuuid:") {
        format!("darwin-bootsessionuuid:{other}")
    } else {
        other.into()
    }
    .into();
    register(&f, "child_registered", &registration);
    let before = f.history();
    f.change(2, 3, 0).unwrap();
    assert_eq!(f.history(), before);
}

#[test]
fn surviving_group_member_blocks_resize_after_registered_leader_exits() {
    let mut leader = Process::new();
    let member = Process(
        Command::new("/bin/sleep")
            .arg("300")
            .process_group(leader.0.id() as i32)
            .spawn()
            .unwrap(),
    );
    let registration = leader.registration();
    leader.stop();
    assert_eq!(
        unsafe { libc::getpgid(member.0.id() as i32) },
        leader.0.id() as i32
    );
    for kind in ["child_registered", "supervisor_ready", "tracked_descendant"] {
        let f = Fixture::new(2);
        f.completed("task", 1);
        register(&f, kind, &registration);
        let before = f.history();
        assert!(matches!(
            f.change(2, 3, 0),
            Err(StateError::CapacityChangeRefused(
                "registered process may be active; reconcile first"
            ))
        ));
        assert_eq!(f.history(), before);
        assert_eq!(f.audit_count(), 0);
    }
}
