#[cfg(test)]
mod conflict_tests;
use super::ports::{ContinuationProcessInspector, ProcessInspectionError as Error};
use crate::state::NeverDispatchedContext;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub struct OsContinuationProcessInspector;

impl ContinuationProcessInspector for OsContinuationProcessInspector {
    fn inspect(&mut self, context: &NeverDispatchedContext) -> Result<(), Error> {
        inspect_os(context)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn inspect_os(context: &NeverDispatchedContext) -> Result<(), Error> {
    let output = Command::new("/bin/ps")
        .args(["-ww", "-axo", "pid=,uid=,stat=,args="])
        .env("LC_ALL", "C")
        .output()
        .map_err(|_| Error::Unavailable)?;
    if !output.status.success() {
        return Err(Error::Unavailable);
    }
    let listing = std::str::from_utf8(&output.stdout).map_err(|_| Error::Unavailable)?;
    let rows = listing
        .lines()
        .map(parse_row)
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        return Err(Error::Unavailable);
    }
    let owner = unsafe { libc::geteuid() };
    for row in rows {
        assess(context, owner, row, current_directory)?;
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn inspect_os(_: &NeverDispatchedContext) -> Result<(), Error> {
    Err(Error::Unavailable)
}

struct ProcessRow<'a> {
    pid: u32,
    uid: u32,
    inert: bool,
    args: &'a str,
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
) -> Result<(), Error> {
    let worker = context
        .plan()
        .executable
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(Error::Unavailable)?;
    let scope = Scope {
        task: context.task_id(),
        attempt: context.attempt_id(),
        worktree: &context.plan().worktree,
        worker,
    };
    assess_scope(&scope, owner, row, cwd)
}

fn assess_scope(
    scope: &Scope<'_>,
    owner: u32,
    row: ProcessRow<'_>,
    mut cwd: impl FnMut(u32) -> Result<PathBuf, Error>,
) -> Result<(), Error> {
    if row.pid == std::process::id() || row.inert {
        return Ok(());
    }
    // Include workers outside the worktree and supervisors referencing the
    // saved namespace. Unknown same-user processes require a cwd observation.
    let references = [
        scope.task,
        scope.attempt,
        scope.worktree.to_str().ok_or(Error::Unavailable)?,
    ];
    if references.iter().any(|value| row.args.contains(value)) {
        return Err(Error::Conflict);
    }
    if row.args.contains(scope.worker) {
        return Err(Error::Unavailable);
    }
    if row.uid == owner {
        let path = cwd(row.pid)?;
        if !path.is_absolute() || path.as_os_str().as_encoded_bytes().ends_with(b" (deleted)") {
            return Err(Error::Unavailable);
        }
        if path.starts_with(scope.worktree) {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn current_directory(pid: u32) -> Result<PathBuf, Error> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).map_err(|_| Error::Unavailable)
}

#[cfg(target_os = "macos")]
fn current_directory(pid: u32) -> Result<PathBuf, Error> {
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
