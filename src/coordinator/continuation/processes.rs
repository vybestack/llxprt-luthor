#[cfg(test)]
mod collector_tests;
#[cfg(test)]
mod conflict_tests;
#[cfg(test)]
mod diagnostic_tests;
mod diagnostics;
use super::ports::{ContinuationProcessInspector, ProcessInspectionError as Error};
use crate::state::NeverDispatchedContext;
use diagnostics::Probe;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub struct OsContinuationProcessInspector;

impl ContinuationProcessInspector for OsContinuationProcessInspector {
    fn inspect(&mut self, context: &NeverDispatchedContext) -> Result<(), Error> {
        let destination = std::env::var_os("LUTHOR_PROCESS_DIAGNOSTIC_FILE");
        let probe = Probe::new(destination.is_some());
        let result = inspect_os(context, &probe);
        if result.is_err()
            && let (Some(destination), Some(record)) = (destination, probe.take())
        {
            // Opt-in fixture evidence must not affect the inspection result.
            let _ = std::fs::write(destination, record.render());
        }
        result
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn inspect_os(context: &NeverDispatchedContext, probe: &Probe) -> Result<(), Error> {
    let child = Command::new("/bin/ps")
        .args(["-ww", "-axo", "pid=,uid=,stat=,args="])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            probe.failed("ps_spawn", None, error.raw_os_error());
            Error::Unavailable
        })?;
    let collector_pid = child.id();
    let output = child.wait_with_output().map_err(|error| {
        probe.failed("ps_wait", Some(collector_pid), error.raw_os_error());
        Error::Unavailable
    })?;
    if !output.status.success() {
        probe.status(output.status.code());
        probe.row(Some(collector_pid), None, None);
        return Err(Error::Unavailable);
    }
    let listing = std::str::from_utf8(&output.stdout).map_err(|_| {
        probe.failed("ps_utf8", Some(collector_pid), None);
        Error::Unavailable
    })?;
    let rows = parse_listing_observed(listing, collector_pid, probe)?;
    let owner = unsafe { libc::geteuid() };
    for row in rows {
        assess(
            context,
            owner,
            row,
            |pid| current_directory_observed(pid, probe),
            probe,
        )?;
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn inspect_os(_: &NeverDispatchedContext, probe: &Probe) -> Result<(), Error> {
    probe.failed("unsupported_os", None, None);
    Err(Error::Unavailable)
}

struct ProcessRow<'a> {
    pid: u32,
    uid: u32,
    state: Option<char>,
    inert: bool,
    args: &'a str,
}

#[cfg(test)]
fn parse_listing(listing: &str, collector_pid: u32) -> Result<Vec<ProcessRow<'_>>, Error> {
    parse_listing_observed(listing, collector_pid, &Probe::new(false))
}

fn parse_listing_observed<'a>(
    listing: &'a str,
    collector_pid: u32,
    probe: &Probe,
) -> Result<Vec<ProcessRow<'a>>, Error> {
    let mut rows = listing
        .lines()
        .map(|line| parse_row(line).inspect_err(|_| probe.parse_failure(line)))
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        probe.failed("row_parser", None, None);
        return Err(Error::Unavailable);
    }
    // The owned collector has been reaped, so its cwd is no longer observable.
    rows.retain(|row| row.pid != collector_pid);
    Ok(rows)
}

fn parse_row(line: &str) -> Result<ProcessRow<'_>, Error> {
    let mut rest = line;
    let mut next = || {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace).ok_or(Error::Unavailable)?;
        let value = &rest[..end];
        rest = &rest[end..];
        Ok(value)
    };
    let pid = next()?.parse().map_err(|_| Error::Unavailable)?;
    let uid = next()?.parse().map_err(|_| Error::Unavailable)?;
    let state = next()?;
    let args = rest.trim_start();
    if pid == 0 || state.is_empty() || args.is_empty() {
        return Err(Error::Unavailable);
    }
    Ok(ProcessRow {
        pid,
        uid,
        state: state.chars().next(),
        inert: state.starts_with('Z'),
        args,
    })
}

struct Scope<'a> {
    task: &'a str,
    attempt: &'a str,
    worktree: &'a Path,
    worker: &'a str,
}

fn assess(
    context: &NeverDispatchedContext,
    owner: u32,
    row: ProcessRow<'_>,
    cwd: impl FnMut(u32) -> Result<PathBuf, Error>,
    probe: &Probe,
) -> Result<(), Error> {
    let worker = context
        .plan()
        .executable
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            observe_row(probe, "argv_match", &row);
            Error::Unavailable
        })?;
    let scope = Scope {
        task: context.task_id(),
        attempt: context.attempt_id(),
        worktree: &context.plan().worktree,
        worker,
    };
    assess_scope_observed(&scope, owner, row, cwd, probe)
}

#[cfg(test)]
fn assess_scope(
    scope: &Scope<'_>,
    owner: u32,
    row: ProcessRow<'_>,
    cwd: impl FnMut(u32) -> Result<PathBuf, Error>,
) -> Result<(), Error> {
    assess_scope_observed(scope, owner, row, cwd, &Probe::new(false))
}

fn assess_scope_observed(
    scope: &Scope<'_>,
    owner: u32,
    row: ProcessRow<'_>,
    mut cwd: impl FnMut(u32) -> Result<PathBuf, Error>,
    probe: &Probe,
) -> Result<(), Error> {
    if row.pid == std::process::id() || row.inert {
        return Ok(());
    }
    // Include workers outside the worktree and supervisors referencing the
    // saved namespace. Unknown same-user processes require a cwd observation.
    let references = [
        scope.task,
        scope.attempt,
        scope.worktree.to_str().ok_or_else(|| {
            observe_row(probe, "argv_match", &row);
            Error::Unavailable
        })?,
    ];
    if references.iter().any(|value| row.args.contains(value)) {
        observe_row(probe, "argv_match", &row);
        return Err(Error::Conflict);
    }
    if row.args.contains(scope.worker) {
        observe_row(probe, "argv_match", &row);
        return Err(Error::Unavailable);
    }
    if row.uid == owner {
        let path = cwd(row.pid).inspect_err(|_| observe_row(probe, "proc_cwd", &row))?;
        if !path.is_absolute() || path.as_os_str().as_encoded_bytes().ends_with(b" (deleted)") {
            observe_row(probe, "proc_cwd", &row);
            return Err(Error::Unavailable);
        }
        if path.starts_with(scope.worktree) {
            observe_row(probe, "proc_cwd", &row);
            return Err(Error::Conflict);
        }
    }
    Ok(())
}

fn observe_row(probe: &Probe, stage: &'static str, row: &ProcessRow<'_>) {
    probe.failed(stage, Some(row.pid), None);
    probe.row(Some(row.pid), Some(row.uid), row.state);
}

#[cfg(test)]
fn current_directory(pid: u32) -> Result<PathBuf, Error> {
    current_directory_observed(pid, &Probe::new(false))
}

#[cfg(target_os = "linux")]
fn current_directory_observed(pid: u32, probe: &Probe) -> Result<PathBuf, Error> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).map_err(|error| {
        probe.failed("proc_cwd", Some(pid), error.raw_os_error());
        Error::Unavailable
    })
}

#[cfg(target_os = "macos")]
fn current_directory_observed(pid: u32, probe: &Probe) -> Result<PathBuf, Error> {
    mac_directory(pid, probe).inspect_err(|_| probe.failed("proc_cwd", Some(pid), None))
}

#[cfg(target_os = "macos")]
fn mac_directory(pid: u32, probe: &Probe) -> Result<PathBuf, Error> {
    use std::os::unix::ffi::OsStrExt;
    let pid = i32::try_from(pid).map_err(|_| Error::Unavailable)?;
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    if read != size {
        let errno = std::io::Error::last_os_error().raw_os_error();
        probe.failed("proc_cwd", Some(pid as u32), errno);
        return Err(Error::Unavailable);
    }
    let bytes: Vec<_> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|byte| *byte as u8)
        .collect();
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Error::Unavailable)?;
    let path = Path::new(std::ffi::OsStr::from_bytes(&bytes[..end]));
    if !path.is_absolute() {
        return Err(Error::Unavailable);
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_listing_rejects_unassessable_rows() {
        for row in ["", "pid uid state args", "0 1 S worker", "12 1 S ", "12 1"] {
            assert!(parse_row(row).is_err(), "{row}");
        }
        let row = parse_row(" 12  501 S /path with spaces worker --session task").unwrap();
        assert_eq!(row.pid, 12);
        assert_eq!(row.uid, 501);
        assert_eq!(row.args, "/path with spaces worker --session task");
        assert!(!row.inert);
        assert!(parse_row("12 501 Z defunct").unwrap().inert);
    }
}
