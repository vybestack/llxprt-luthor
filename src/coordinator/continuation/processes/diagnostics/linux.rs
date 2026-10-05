use std::{
    fs::File,
    io::{self, Read},
    os::unix::ffi::OsStrExt,
    path::Path,
};

const EMPTY_STATUS: &str = "Uid=- Gid=- TracerPid=- NoNewPrivs=- Seccomp=-";

pub fn read_leaf(root: &Path, leaf: &str) -> io::Result<Vec<u8>> {
    if leaf == "exe" {
        let target = std::fs::read_link(root.join(leaf))?;
        return target
            .file_name()
            .map(|name| name.as_bytes().to_vec())
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL));
    }
    let limit = match leaf {
        "comm" => 64,
        "stat" => 4096,
        "status" => 16384,
        _ => 0,
    };
    let mut bytes = Vec::new();
    File::open(root.join(leaf))?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::from_raw_os_error(libc::EFBIG));
    }
    Ok(bytes)
}

pub fn collect(pid: u32, mut read: impl FnMut(&str) -> io::Result<Vec<u8>>) -> String {
    let before = read("stat")
        .map_err(errno)
        .and_then(|bytes| starttime(pid, &bytes));
    let comm = read("comm")
        .map_err(errno)
        .map(|bytes| name(bytes.strip_suffix(b"\n").unwrap_or(&bytes), 15));
    let status = read("status")
        .map_err(errno)
        .and_then(|bytes| parse_status(&bytes));
    let exe = read("exe").map_err(errno).map(|bytes| name(&bytes, 64));
    let after = read("stat")
        .map_err(errno)
        .and_then(|bytes| starttime(pid, &bytes));
    // Bookend observations by start time, not PID alone. Never attribute mixed
    // metadata to a process that exited or was replaced while it was read.
    let stable = matches!((&before, &after), (Ok(a), Ok(b)) if a == b);
    let identity_errno = if stable { 0 } else { libc::ESTALE };
    format!(
        " comm={} starttime={} {} exe={} stat_errno={} comm_errno={} status_errno={} exe_errno={} stat_after_errno={} identity_errno={}",
        value(&comm, stable, "-"),
        value(&before, stable, "-"),
        value(&status, stable, EMPTY_STATUS),
        value(&exe, stable, "-"),
        failure(&before),
        failure(&comm),
        failure(&status),
        failure(&exe),
        failure(&after),
        identity_errno,
    )
}

fn errno(error: io::Error) -> i32 {
    error.raw_os_error().unwrap_or(libc::EIO)
}

fn failure<T>(result: &Result<T, i32>) -> i32 {
    result.as_ref().err().copied().unwrap_or(0)
}

fn value<T: std::fmt::Display>(result: &Result<T, i32>, stable: bool, missing: &str) -> String {
    match (stable, result) {
        (true, Ok(value)) => value.to_string(),
        _ => missing.into(),
    }
}

fn name(bytes: &[u8], max: usize) -> String {
    if bytes.is_empty()
        || bytes.len() > max
        || bytes == b"."
        || bytes == b".."
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.+-".contains(byte))
    {
        return "redacted".into();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn starttime(pid: u32, bytes: &[u8]) -> Result<u64, i32> {
    let text = std::str::from_utf8(bytes).map_err(|_| libc::EINVAL)?;
    let (prefix, fields) = text.rsplit_once(')').ok_or(libc::EINVAL)?;
    let (observed, _) = prefix.split_once(" (").ok_or(libc::EINVAL)?;
    if observed.parse::<u32>() != Ok(pid) {
        return Err(libc::EINVAL);
    }
    let mut fields = fields.split_whitespace();
    let state = fields.next().ok_or(libc::EINVAL)?;
    if state.len() != 1 || !state.as_bytes()[0].is_ascii_alphabetic() {
        return Err(libc::EINVAL);
    }
    let ticks = fields.nth(18).ok_or(libc::EINVAL)?;
    if !ticks.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(libc::EINVAL);
    }
    ticks.parse().map_err(|_| libc::EINVAL)
}

fn parse_status(bytes: &[u8]) -> Result<String, i32> {
    let text = std::str::from_utf8(bytes).map_err(|_| libc::EINVAL)?;
    let mut values = Vec::new();
    for (key, count) in [
        ("Uid", 4),
        ("Gid", 4),
        ("TracerPid", 1),
        ("NoNewPrivs", 1),
        ("Seccomp", 1),
    ] {
        values.push(format!("{key}={}", status_field(text, key, count)?));
    }
    Ok(values.join(" "))
}

fn status_field(text: &str, key: &str, count: usize) -> Result<String, i32> {
    let mut lines = text
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| *name == key);
    let Some((_, raw)) = lines.next() else {
        return Ok("-".into());
    };
    if lines.next().is_some() {
        return Err(libc::EINVAL);
    }
    let fields = raw
        .split_whitespace()
        .take(count + 1)
        .map(|value| {
            if !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(libc::EINVAL);
            }
            value
                .parse::<u32>()
                .map(|number| number.to_string())
                .map_err(|_| libc::EINVAL)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if fields.len() != count {
        return Err(libc::EINVAL);
    }
    Ok(fields.join(","))
}
