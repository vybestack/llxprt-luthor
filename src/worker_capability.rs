//! Explicit worker attestation; never infer session formats from build names.
use crate::worker_build::WorkerBuild;
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStateVersions {
    pub session_state_versions: Vec<u64>,
}

/// The probe receives no session identifier, prompt, or template arguments.
/// Its only accepted response is one successful, bounded JSON document.
pub(crate) fn attest(
    build: &WorkerBuild,
    cwd: &Path,
    version: u64,
) -> io::Result<SessionStateVersions> {
    build.verify()?;
    let bytes = probe(&build.path, cwd)?;
    build.verify()?;
    let capability: SessionStateVersions = serde_json::from_slice(&bytes)?;
    if !capability.session_state_versions.contains(&version) {
        return Err(io::Error::other(
            "worker did not attest compatibility with the recorded session state version",
        ));
    }
    Ok(capability)
}

#[cfg(unix)]
fn probe(executable: &Path, cwd: &Path) -> io::Result<Vec<u8>> {
    use std::os::fd::AsRawFd;
    let mut child = Command::new(executable)
        .arg("--session-state-versions")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing probe stdout"))?;
        let fd = stdout.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 4096];
            match stdout.read(&mut chunk) {
                Ok(0) => {
                    if let Some(status) = child.try_wait()? {
                        if !status.success() {
                            return Err(io::Error::other(format!(
                                "session compatibility probe failed: {status}"
                            )));
                        }
                        return Ok(bytes);
                    }
                }
                Ok(count) => bytes.extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
            if bytes.len() > 64 * 1024 {
                return Err(io::Error::other(
                    "session compatibility probe output exceeds 64 KiB",
                ));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("session compatibility probe timed out"));
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        if child.try_wait()?.is_none() {
            child.kill()?;
        }
        child.wait()?;
    }
    result
}

#[cfg(not(unix))]
fn probe(_: &Path, _: &Path) -> io::Result<Vec<u8>> {
    Err(io::Error::other("session compatibility probe unsupported"))
}
