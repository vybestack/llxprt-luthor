use super::error::SupervisorError;
use std::{
    io::Read,
    os::fd::AsRawFd,
    process::ChildStdout,
    time::{Duration, Instant},
};

pub(crate) fn wait_for_ready(reader: &mut ChildStdout) -> Result<(), SupervisorError> {
    let fd = reader.as_raw_fd();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ready = [0u8; 6];
    for byte in &mut ready {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(SupervisorError::ReadyTimeout);
        }
        let timeout_ms = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        let polled = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if polled == 0 {
            return Err(SupervisorError::ReadyTimeout);
        }
        if polled < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if reader.read_exact(std::slice::from_mut(byte)).is_err() {
            return Err(SupervisorError::ExecutionUnavailable);
        }
    }
    if ready != *b"READY\n" {
        return Err(SupervisorError::ExecutionUnavailable);
    }
    Ok(())
}
