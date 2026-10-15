//! Actions reruns are writes: freshly verify identity and exact head first.
use super::types::{ErrorCategory, FailedJobsRerun, LookupError, error};
use serde_json::Value;
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
};

pub(crate) fn rerun<E: From<LookupError>>(
    executable: &Path,
    repository: &str,
    run: u64,
    head: &str,
    mut api: impl FnMut(&[&str]) -> Result<Value, LookupError>,
    before_post: impl FnOnce() -> Result<(), E>,
) -> Result<FailedJobsRerun, E> {
    let user = api(&["api", "user"])?;
    if user["login"].as_str() != Some("acoliver") {
        return Err(error(ErrorCategory::Permission, "identity-mismatch", None).into());
    }
    let path = format!("repos/{repository}/actions/runs/{run}");
    let evidence = api(&["api", &path])?;
    if evidence["id"].as_u64() != Some(run)
        || evidence["head_sha"].as_str() != Some(head)
        || evidence["repository"]["full_name"].as_str() != Some(repository)
    {
        return Err(error(ErrorCategory::Malformed, "invalid-failed-run", None).into());
    }
    match evidence["status"].as_str() {
        Some("queued" | "in_progress") => return Ok(FailedJobsRerun::Pending),
        Some("completed") => {}
        _ => return Err(error(ErrorCategory::Malformed, "invalid-failed-run", None).into()),
    }
    before_post()?;
    crate::github::budget::command(&["api"]);
    let status = Command::new(executable)
        .args([
            "api",
            "--method",
            "POST",
            &format!("{path}/rerun-failed-jobs"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| error(ErrorCategory::Transport, "rerun-command-failed", None))?;
    if !status.success() {
        return Err(error(ErrorCategory::Unknown, "rerun-command-failed", None).into());
    }
    Ok(FailedJobsRerun::Accepted)
}

/// Bound the excerpt in memory; the gh process drains normally rather than
/// treating a broken pipe or partial read as successful log retrieval.
pub(crate) fn logs(executable: &Path, repository: &str, job: u64) -> Result<String, LookupError> {
    crate::github::budget::command(&["api"]);
    let mut child = Command::new(executable)
        .args([
            "api",
            &format!("repos/{repository}/actions/jobs/{job}/logs"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| error(ErrorCategory::Transport, "job-logs-command-failed", None))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| error(ErrorCategory::Transport, "job-logs-command-failed", None))?;
    let mut tail = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let size = stdout
            .read(&mut buf)
            .map_err(|_| error(ErrorCategory::Transport, "job-logs-read-failed", None))?;
        if size == 0 {
            break;
        }
        tail.extend_from_slice(&buf[..size]);
        if tail.len() > 4096 {
            tail.drain(..tail.len() - 4096);
        }
    }
    let status = child
        .wait()
        .map_err(|_| error(ErrorCategory::Transport, "job-logs-command-failed", None))?;
    if !status.success() {
        return Err(error(
            ErrorCategory::Unknown,
            "job-logs-command-failed",
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&tail).into_owned())
}
