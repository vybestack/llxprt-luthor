use super::{
    CliError,
    files::{attempts_dir, log_paths, private_file},
};
use crate::cli_identifiers::valid_attempt;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
const SILENCE_WARNING_THRESHOLD_SECONDS: u64 = 300;

pub(crate) enum OutputAge {
    LogOnly,
    AttemptStart(Option<i64>),
}
pub(crate) fn log_observation(
    output: &mut Value,
    root: &Path,
    attempt: Option<&str>,
    fallback: OutputAge,
) {
    let timestamp = attempt.and_then(|id| output_time(root, id));
    let (bytes, bytes_unavailable) = attempt
        .map(|id| observed_log_bytes(root, id))
        .unwrap_or((None, Some("no reserved attempt")));
    let current = now();
    let (start_age, unavailable) = match fallback {
        OutputAge::LogOnly => (None, "no verified output log data"),
        OutputAge::AttemptStart(start) => (
            start
                .and_then(|epoch| u64::try_from(epoch).ok())
                .filter(|epoch| *epoch <= current)
                .map(|epoch| current - epoch),
            "no verified output log data or valid attempt start time",
        ),
    };
    let (status, age, reason) = match (attempt, timestamp, start_age) {
        (None, _, _) => ("unavailable", None, Some("no reserved attempt")),
        (Some(_), Some(timestamp), _) => {
            ("available", Some(current.saturating_sub(timestamp)), None)
        }
        (Some(_), None, Some(age)) => ("no_output_yet", Some(age), None),
        (Some(_), None, None) => ("unavailable", None, Some(unavailable)),
    };
    output["last_output_age_seconds"] = json!(age);
    output["output_age_unavailable_reason"] = json!(reason);
    output["output_log_status"] = json!(status);
    output["observed_stdout_bytes"] = json!(bytes.map(|bytes| bytes[0]));
    output["observed_stderr_bytes"] = json!(bytes.map(|bytes| bytes[1]));
    output["observed_bytes_unavailable_reason"] = json!(bytes_unavailable);
    output["byte_counts_are_observational"] = json!(true);
    output["observed_at_utc"] = json!(bytes.and_then(|_| utc_now()));
    output["output_silence_warning"] =
        json!(age.is_some_and(|age| age >= SILENCE_WARNING_THRESHOLD_SECONDS));
    output["silence_warning_threshold_seconds"] = json!(SILENCE_WARNING_THRESHOLD_SECONDS);
}

fn observed_log_bytes(root: &Path, attempt: &str) -> (Option<[u64; 2]>, Option<&'static str>) {
    if !valid_attempt(attempt) {
        return (None, Some("invalid attempt id"));
    }
    let result = (|| {
        let dir = attempts_dir(root)?;
        let paths = log_paths(&dir, attempt, None)?;
        let (_, stdout) = private_file(&paths[0])?;
        let (_, stderr) = private_file(&paths[1])?;
        Ok::<_, CliError>([stdout.len(), stderr.len()])
    })();
    match result {
        Ok(bytes) => (Some(bytes), None),
        Err(_) => (None, Some("active attempt log files are missing or unsafe")),
    }
}

fn utc_now() -> Option<String> {
    let seconds = i64::try_from(now()).ok()?;
    let conn = Connection::open_in_memory().ok()?;
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', ?1, 'unixepoch')",
        [seconds],
        |r| r.get(0),
    )
    .ok()
}

fn output_time(root: &Path, attempt: &str) -> Option<u64> {
    if !valid_attempt(attempt) {
        return None;
    }
    let dir = attempts_dir(root).ok()?;
    let paths = log_paths(&dir, attempt, None).ok()?;
    paths
        .iter()
        .filter_map(|path| {
            let (_, meta) = private_file(path).ok()?;
            if meta.len() == 0 {
                return None;
            }
            meta.modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        })
        .max()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
