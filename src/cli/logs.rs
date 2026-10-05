use super::{
    CliError,
    files::{attempts_dir, log_paths, private_file},
    history::receipt_summary,
};
use crate::{cli_identifiers::valid_attempt, supervisor::ExitReceipt};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use std::{io::Read, path::Path};

fn receipt(
    conn: &Connection,
    dir: &Path,
    task: &str,
    attempt: &str,
) -> Result<Option<ExitReceipt>, CliError> {
    let path = dir.join(format!("{attempt}.receipt.json"));
    let recorded: Option<String> = conn.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='attempt_exit' ORDER BY sequence DESC LIMIT 1",
        [task,attempt],|r|r.get(0)
    ).optional().map_err(|_|CliError::Database)?;
    if !path.exists() && recorded.is_none() {
        return Ok(None);
    }
    let (mut file, _) = private_file(&path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| CliError::UnsafeLog)?;
    let parsed: ExitReceipt = serde_json::from_slice(&bytes).map_err(|_| CliError::UnsafeLog)?;
    if parsed.attempt_id != attempt
        || recorded
            .as_ref()
            .is_some_and(|s| serde_json::from_str::<ExitReceipt>(s).ok().as_ref() != Some(&parsed))
    {
        return Err(CliError::UnsafeLog);
    }
    Ok(Some(parsed))
}

pub(crate) fn logs(
    conn: &Connection,
    root: &Path,
    task: &str,
    selected: Option<&str>,
) -> Result<Value, CliError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1)",
            [task],
            |r| r.get(0),
        )
        .map_err(|_| CliError::Database)?;
    if !exists {
        return Err(CliError::TaskNotFound);
    }
    if selected.is_some_and(|id| !valid_attempt(id)) {
        return Err(CliError::Arguments);
    }
    let mut stmt = conn
        .prepare("SELECT id FROM attempts WHERE task_id=?1 ORDER BY created_at,rowid")
        .map_err(|_| CliError::Database)?;
    let ids = stmt
        .query_map([task], |r| r.get::<_, String>(0))
        .map_err(|_| CliError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| CliError::Database)?;
    let attempt = match selected {
        Some(id) if ids.iter().any(|v| v == id) => id.to_owned(),
        Some(_) => return Err(CliError::AttemptNotFound),
        None => ids.last().cloned().ok_or(CliError::AttemptNotFound)?,
    };
    if !valid_attempt(&attempt) {
        return Err(CliError::UnsafeLog);
    }
    let dir = attempts_dir(root)?;
    let receipt = receipt(conn, &dir, task, &attempt)?;
    let paths = log_paths(&dir, &attempt, receipt.as_ref())?;
    let mut output = serde_json::Map::new();
    for (stream, path) in ["stdout", "stderr"].into_iter().zip(paths) {
        let (mut file, meta) = private_file(&path)?;
        if receipt.as_ref().is_some_and(|r| {
            meta.len()
                != if stream == "stdout" {
                    r.stdout_bytes
                } else {
                    r.stderr_bytes
                }
        }) {
            return Err(CliError::UnsafeLog);
        }
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|_| CliError::UnsafeLog)?;
        output.insert(stream.into(), json!(contents));
    }
    Ok(json!({"task_id":task,"attempt_id":attempt,"logs":output,
        "receipt":receipt.as_ref().map(receipt_summary)}))
}
