use super::*;

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

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) struct FixtureGroupGuard(pub(crate) std::path::PathBuf);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for FixtureGroupGuard {
    fn drop(&mut self) {
        let Ok(bytes) = fs::read(&self.0) else { return };
        let Ok(child) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return;
        };
        let Some(pid) = child["pid"].as_u64().and_then(|id| i32::try_from(id).ok()) else {
            return;
        };
        if pid > 0
            && child["group_id"].as_i64() == Some(i64::from(pid))
            && unsafe { libc::getpgid(pid) } == pid
            && observed_process_identity(pid as u32).as_ref()
                == Some(&(
                    child["boot_identity"].as_str().unwrap_or_default().into(),
                    child["start_identity"].as_str().unwrap_or_default().into(),
                ))
        {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }
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
