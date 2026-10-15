#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::{
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
pub(crate) fn observed_process_identity(pid: u32) -> Option<(String, String)> {
    let boot = Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.bootsessionuuid"])
        .output()
        .ok()?;
    if !boot.status.success() {
        return None;
    }
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    (unsafe {
        libc::proc_pidinfo(
            i32::try_from(pid).ok()?,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    } == size
        && info.pbi_pid == pid
        && info.pbi_start_tvsec != 0)
        .then(|| {
            (
                format!(
                    "darwin-bootsessionuuid:{}",
                    String::from_utf8(boot.stdout)
                        .expect("sysctl UUID UTF-8")
                        .trim()
                        .to_ascii_lowercase()
                ),
                format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
            )
        })
}

#[cfg(target_os = "linux")]
pub(crate) fn observed_process_identity(pid: u32) -> Option<(String, String)> {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let start = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    Some((boot.trim().into(), start.into()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn test_process_identity(pid: u32) -> (String, String) {
    observed_process_identity(pid).expect("fixture process identity")
}

#[cfg(unix)]
pub(crate) fn wait_for_process_and_group_absence(pid: libc::pid_t) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let process_result = unsafe { libc::kill(pid, 0) };
        let process_absent = process_result == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        let group_result = unsafe { libc::kill(-pid, 0) };
        let group_absent = group_result == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if process_absent && group_absent {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "supervisor {pid} or its process group remains"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

use luthor::github::{
    project::{Issue, Page, ProjectItem, ProjectReadError, ProjectReader},
    pull_request::{LookupError, PullRequestReader},
};

pub(crate) struct NoProjects;
impl ProjectReader for NoProjects {
    fn page(&mut self, _: &str, _: Option<&str>) -> Result<Page<ProjectItem>, ProjectReadError> {
        panic!("historical hold must precede project reads")
    }
    fn issue(&mut self, _: &ProjectItem) -> Result<Issue, ProjectReadError> {
        panic!("historical hold must precede issue reads")
    }
}

pub(crate) struct NoPrs;
impl PullRequestReader for NoPrs {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        panic!("historical hold must precede identity reads")
    }
    fn page(&mut self, _: &str, _: u32) -> Result<Vec<serde_json::Value>, LookupError> {
        panic!("historical hold must precede PR reads")
    }
    fn detail(&mut self, _: &str, _: u64) -> Result<serde_json::Value, LookupError> {
        panic!("historical hold must precede PR detail reads")
    }
    fn repository_identity(&mut self, _: &str) -> Result<u64, LookupError> {
        panic!("historical hold must precede repository reads")
    }
}

/// A historical refusal must never reach either worker-launch boundary.
pub(crate) struct NoLaunch;
impl luthor::coordinator::SupervisorLauncher for NoLaunch {
    fn launch(
        &mut self,
        _: &mut luthor::state::StateStore,
        _: &luthor::supervisor::LaunchPlan,
        _: &luthor::WorktreeOwner,
    ) -> Result<(), luthor::supervisor::SupervisorError> {
        panic!("historical hold must precede launch")
    }
}
impl luthor::coordinator::AmendedSupervisorLauncher for NoLaunch {
    fn launch_amended(
        &mut self,
        _: &mut luthor::state::StateStore,
        _: &luthor::state::NeverDispatchedContext,
        _: &luthor::config::Config,
        _: &str,
        _: &luthor::WorktreeOwner,
    ) -> Result<(), luthor::supervisor::SupervisorError> {
        panic!("historical hold must precede amended launch")
    }
}
