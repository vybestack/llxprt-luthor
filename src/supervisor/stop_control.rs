use super::{binding, error::SupervisorError, processes::identity};
use crate::model::LaunchPlan;
use rusqlite::{Connection, OpenFlags, params};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::Child,
    thread,
    time::{Duration, Instant},
};
#[cfg(unix)]
fn stop_child_matches(pid: i32, boot: &str, start: &str) -> bool {
    identity(pid as u32).ok().as_ref() == Some(&(boot.to_owned(), start.to_owned()))
        && unsafe { libc::getpgid(pid) } == pid
}

#[cfg(unix)]
pub(crate) fn handle_stop(
    stream: &mut UnixStream,
    plan: &LaunchPlan,
    store_root: &Path,
    child: &mut Child,
    boot: &str,
    start: &str,
    signals: &mut Vec<i32>,
) -> Result<(), SupervisorError> {
    let pid = i32::try_from(child.id()).map_err(|_| SupervisorError::StopUnavailable)?;
    if !authorized_stop(stream, plan, store_root)?
        || child.try_wait()?.is_some()
        || !stop_child_matches(pid, boot, start)
    {
        stream.write_all(b"N")?;
        return Ok(());
    }
    // Each escalation targets only the still-matching dedicated process group.
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGKILL] {
        if child.try_wait()?.is_some() {
            stream.write_all(b"Y")?;
            return Ok(());
        }
        if !stop_child_matches(pid, boot, start) {
            stream.write_all(b"N")?;
            return Ok(());
        }
        if unsafe { libc::kill(-pid, signal) } != 0 {
            stream.write_all(b"N")?;
            return Ok(());
        }
        signals.push(signal);
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline {
            if child.try_wait()?.is_some() {
                stream.write_all(b"Y")?;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    if child.try_wait()?.is_some() {
        stream.write_all(b"Y")?;
    } else {
        stream.write_all(b"N")?;
    }
    Ok(())
}

fn authorized_stop(
    stream: &mut UnixStream,
    plan: &LaunchPlan,
    store_root: &Path,
) -> Result<bool, SupervisorError> {
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let mut request = [0u8; 512];
    let size = stream.read(&mut request).unwrap_or(0);
    let expected = format!("{}\n{}\n", plan.task_id, plan.attempt_id);
    let connection = Connection::open_with_flags(
        store_root
            .parent()
            .ok_or(SupervisorError::Conflict)?
            .join("state.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let persisted: Vec<String> = connection
        .prepare("SELECT detail FROM intents WHERE kind='stop' AND task_id=?1 AND attempt_id=?2")?
        .query_map(params![plan.task_id, plan.attempt_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let intended =
        serde_json::json!({"task_id":plan.task_id,"attempt_id":plan.attempt_id}).to_string();
    Ok(
        binding::verify_observed_plan(&connection, &store_root.join(".."), plan).is_ok()
            && request[..size] == *expected.as_bytes()
            && persisted == [intended],
    )
}
