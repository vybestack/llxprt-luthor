use crate::supervisor::{ExitReceipt, LaunchPlan};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

const SILENCE_WARNING_THRESHOLD_SECONDS: u64 = 300;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CliError {
    #[error("invalid command arguments")]
    Arguments,
    #[error("state database unavailable")]
    Database,
    #[error("task not found")]
    TaskNotFound,
    #[error("attempt not found")]
    AttemptNotFound,
    #[error("recorded log path is unsafe or unavailable")]
    UnsafeLog,
    #[error("output serialization failed")]
    Serialization,
}

fn database(root: &Path) -> Result<Connection, CliError> {
    Connection::open_with_flags(root.join("state.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| CliError::Database)
}

pub fn execute(root: &Path, args: &[String]) -> Result<String, CliError> {
    let command = args.first().ok_or(CliError::Arguments)?;
    let conn = database(root)?;
    let output = match command.as_str() {
        "status" if args.len() == 1 => status(&conn, root)?,
        "show" if args.len() == 2 => show(&conn, root, &args[1])?,
        "logs" if args.len() == 2 || args.len() == 4 && args[2] == "--attempt" => {
            logs(&conn, root, &args[1], args.get(3).map(String::as_str))?
        }
        _ => return Err(CliError::Arguments),
    };
    serde_json::to_string(&output).map_err(|_| CliError::Serialization)
}

fn status(conn: &Connection, root: &Path) -> Result<Value, CliError> {
    let capacity: i64 = conn
        .query_row(
            "SELECT value FROM state_meta WHERE key='capacity'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| CliError::Database)?;
    let reserved: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM reservations WHERE status='reserved'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| CliError::Database)?;
    let mut stmt = conn.prepare("SELECT t.id,t.state,t.repository,t.issue_number,
        EXISTS(SELECT 1 FROM reservations r WHERE r.task_id=t.id AND r.status='reserved'),
        (SELECT payload FROM evidence e WHERE e.task_id=t.id AND e.kind='held_reason' ORDER BY sequence DESC LIMIT 1),
        (SELECT a.id FROM attempts a JOIN reservations r ON r.attempt_id=a.id
         WHERE a.task_id=t.id AND r.status='reserved' ORDER BY a.rowid DESC LIMIT 1),
        (SELECT a.id FROM attempts a WHERE a.task_id=t.id ORDER BY a.rowid DESC LIMIT 1),
        (SELECT a.outcome FROM attempts a WHERE a.task_id=t.id ORDER BY a.rowid DESC LIMIT 1),
        (SELECT a.lifecycle FROM attempts a WHERE a.task_id=t.id ORDER BY a.rowid DESC LIMIT 1),
        (SELECT e.payload FROM evidence e WHERE e.task_id=t.id AND e.kind='pause_pr_lookup' ORDER BY e.sequence DESC LIMIT 1),
        (SELECT CAST(strftime('%s', a.created_at) AS INTEGER) FROM attempts a
         JOIN reservations r ON r.attempt_id=a.id
         WHERE a.task_id=t.id AND r.status='reserved' ORDER BY a.rowid DESC LIMIT 1)
        FROM tasks t ORDER BY t.created_at,t.id").map_err(|_| CliError::Database)?;
    let tasks = stmt
        .query_map([], |r| {
            let active_attempt = r.get::<_, Option<String>>(6)?;
            let output = active_attempt.as_deref().and_then(|id| output_time(root, id));
            let current = now();
            let start = r.get::<_, Option<i64>>(11)?
                .and_then(|epoch| u64::try_from(epoch).ok())
                .filter(|epoch| *epoch <= current);
            let (output_log_status, age, unavailable_reason) = match (active_attempt, output) {
                (None, _) => ("unavailable", None, Some("no reserved attempt")),
                (Some(_), Some(timestamp)) => (
                    "available", Some(current.saturating_sub(timestamp)), None,
                ),
                (Some(_), None) => match start {
                    Some(epoch) => ("no_output_yet", Some(current - epoch), None),
                    None => ("unavailable", None, Some("no verified output log data or valid attempt start time")),
                },
            };
            Ok(json!({
                "task_id":r.get::<_,String>(0)?, "phase":r.get::<_,String>(1)?,
                "repository":r.get::<_,String>(2)?, "issue_number":r.get::<_,i64>(3)?,
                "reserved_slot":r.get::<_,bool>(4)?, "reason":r.get::<_,Option<String>>(5)?,
                "last_output_age_seconds":age,
                "output_age_unavailable_reason":unavailable_reason,
                "output_log_status":output_log_status,
                "output_silence_warning":age.is_some_and(|seconds| seconds >= SILENCE_WARNING_THRESHOLD_SECONDS),
                "silence_warning_threshold_seconds":SILENCE_WARNING_THRESHOLD_SECONDS,
                "latest_attempt_id":r.get::<_,Option<String>>(7)?,
                "latest_attempt_outcome":r.get::<_,Option<String>>(8)?,
                "latest_attempt_outcome_unavailable_reason":if r.get::<_,Option<String>>(8)?.is_none(){Some("attempt has no verified exit outcome")}else{None},
                "latest_attempt_lifecycle":r.get::<_,Option<String>>(9)?,
                "pr_state":"unavailable",
                "pr_unavailable_reason":"status does not perform a fresh exhaustive PR read",
                "last_observed_pr":r.get::<_,Option<String>>(10)?
            }))
        })
        .map_err(|_| CliError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| CliError::Database)?;
    Ok(
        json!({"tasks":tasks,"capacity":{"reserved":reserved,"limit":capacity},
        "reserved_slot_count":reserved,"latest_telemetry":null,
        "telemetry_unavailable_reason":"no telemetry evidence recorded"}),
    )
}

fn events(conn: &Connection, table: &str, task: &str) -> Result<Vec<Value>, CliError> {
    // The schema stores CURRENT_TIMESTAMP as UTC `YYYY-MM-DD HH:MM:SS`.
    let sql = match table {
        "evidence" => {
            "SELECT attempt_id,kind,payload,created_at,CAST(strftime('%s',created_at) AS INTEGER) FROM evidence WHERE task_id=?1 ORDER BY sequence"
        }
        "intents" => {
            "SELECT attempt_id,kind,detail,created_at,CAST(strftime('%s',created_at) AS INTEGER) FROM intents WHERE task_id=?1 ORDER BY sequence"
        }
        _ => unreachable!(),
    };
    let mut stmt = conn.prepare(sql).map_err(|_| CliError::Database)?;
    stmt.query_map([task], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })
    .map_err(|_| CliError::Database)?
    .map(|row| {
        let (attempt_id, kind, payload, created_at, created_at_unix_secs) =
            row.map_err(|_| CliError::Database)?;
        // Launch and dispatch details contain executable arguments and prompts.
        let detail = match (table, kind.as_str()) {
            ("evidence", "held_reason" | "claim_verified") => Some(json!(payload)),
            ("evidence", "attempt_exit") => serde_json::from_str::<ExitReceipt>(&payload)
                .ok()
                .map(|receipt| receipt_summary(&receipt)),
            _ => None,
        };
        Ok(
            json!({"attempt_id":attempt_id,"kind":kind,"created_at":created_at,
                "created_at_unix_secs":created_at_unix_secs,"detail":detail}),
        )
    })
    .collect()
}

fn receipt_summary(receipt: &ExitReceipt) -> Value {
    json!({"attempt_id":receipt.attempt_id,"child_pid":receipt.child_pid,
        "exit_code":receipt.exit_code,"signal":receipt.signal,
        "stdout_path":receipt.stdout_path,"stdout_bytes":receipt.stdout_bytes,
        "stderr_path":receipt.stderr_path,"stderr_bytes":receipt.stderr_bytes,
        "stop_signals":receipt.stop_signals})
}

fn show(conn: &Connection, root: &Path, task: &str) -> Result<Value, CliError> {
    let row: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT state,repository,issue_number FROM tasks WHERE id=?1",
            [task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| CliError::Database)?;
    let (phase, repository, number) = row.ok_or(CliError::TaskNotFound)?;
    let evidence = events(conn, "evidence", task)?;
    let intents = events(conn, "intents", task)?;
    let selection: Option<String> = conn.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind='selection' ORDER BY sequence LIMIT 1",[task],|r|r.get(0)
    ).optional().map_err(|_| CliError::Database)?;
    let candidate = selection
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.get("candidate").cloned());
    let worktree: Option<String> = conn.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind='worktree_created' ORDER BY sequence DESC LIMIT 1",[task],|r|r.get(0)
    ).optional().map_err(|_| CliError::Database)?;
    let mut stmt = conn
        .prepare(
            "SELECT a.id,a.lifecycle,a.outcome,a.created_at,
        CAST(strftime('%s',a.created_at) AS INTEGER),
        (SELECT status FROM reservations r WHERE r.attempt_id=a.id)
        FROM attempts a WHERE a.task_id=?1 ORDER BY a.created_at,a.rowid",
        )
        .map_err(|_| CliError::Database)?;
    let attempts = stmt.query_map([task], |r| Ok(json!({
        "id":r.get::<_,String>(0)?, "lifecycle":r.get::<_,String>(1)?,
        "outcome":r.get::<_,Option<String>>(2)?, "created_at":r.get::<_,String>(3)?,
        "created_at_unix_secs":r.get::<_,Option<i64>>(4)?, "reservation":r.get::<_,Option<String>>(5)?
    }))).map_err(|_| CliError::Database)?
        .collect::<Result<Vec<_>,_>>().map_err(|_| CliError::Database)?;
    let reserved: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM reservations WHERE task_id=?1 AND status='reserved')",
            [task],
            |r| r.get(0),
        )
        .map_err(|_| CliError::Database)?;
    let mut session = None;
    for attempt in &attempts {
        let id = attempt["id"].as_str().ok_or(CliError::Database)?;
        let detail: Option<String> = conn.query_row(
            "SELECT detail FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='launch' ORDER BY sequence DESC LIMIT 1",
            [task,id],|r|r.get(0)
        ).optional().map_err(|_| CliError::Database)?;
        if let Some(detail) = detail {
            let plan: LaunchPlan = serde_json::from_str(&detail).map_err(|_| CliError::Database)?;
            if plan.task_id != task || plan.attempt_id != id {
                return Err(CliError::Database);
            }
            session = Some(plan.session_id);
        }
    }
    let active_attempt = attempts
        .iter()
        .rev()
        .find(|a| a["reservation"] == "reserved");
    let (last_output_age_seconds, output_age_unavailable_reason) = match active_attempt {
        None => (None, Some("no reserved attempt")),
        Some(attempt) => {
            let id = attempt["id"].as_str().ok_or(CliError::Database)?;
            match output_time(root, id) {
                Some(timestamp) => (Some(now().saturating_sub(timestamp)), None),
                None => (None, Some("no verified output log data")),
            }
        }
    };
    let reason = evidence
        .iter()
        .rev()
        .find(|e| e["kind"] == "held_reason")
        .and_then(|e| e["detail"].as_str());
    let latest_attempt = attempts.last();
    let latest_attempt_id = latest_attempt.and_then(|a| a["id"].as_str());
    let latest_attempt_outcome = latest_attempt.and_then(|a| a["outcome"].as_str());
    let output_silence_warning =
        last_output_age_seconds.is_some_and(|age| age >= SILENCE_WARNING_THRESHOLD_SECONDS);
    let last_observed_pr = evidence
        .iter()
        .rev()
        .find(|e| e["kind"] == "pause_pr_lookup")
        .map(|event| json!({"proof":event["detail"],"observed_at":event["created_at"]}));
    Ok(
        json!({"task":{"id":task,"repository":repository,"issue_number":number,
        "issue_url":candidate.as_ref().and_then(|v|v.get("issue_url")),
        "source":candidate.as_ref().and_then(|v|v.get("source")),
        "mapping":candidate.as_ref().and_then(|v|v.get("mapping"))},
        "phase":phase,"reason":reason,"reserved_slot":reserved,"attempts":attempts,
        "intents":intents,"evidence":evidence,"session":session,
        "worktree":worktree.and_then(|v|serde_json::from_str::<Value>(&v).ok()),
        "last_output_age_seconds":last_output_age_seconds,
        "output_age_unavailable_reason":output_age_unavailable_reason,
        "output_log_status":if last_output_age_seconds.is_some(){"available"}else{"unavailable"},
        "output_silence_warning":output_silence_warning,
        "silence_warning_threshold_seconds":SILENCE_WARNING_THRESHOLD_SECONDS,
        "latest_attempt_id":latest_attempt_id,
        "latest_attempt_outcome":latest_attempt_outcome,
        "latest_attempt_outcome_unavailable_reason":if latest_attempt_outcome.is_none(){Some("attempt has no verified exit outcome")}else{None},
        "pr_state":"unavailable",
        "pr_unavailable_reason":"show does not perform a fresh exhaustive PR read",
        "last_observed_pr":last_observed_pr,
        "last_observed_pr_unavailable_reason":if last_observed_pr.is_none(){Some("no verified stored PR proof")}else{None}}),
    )
}

fn valid_attempt(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn attempts_dir(root: &Path) -> Result<PathBuf, CliError> {
    let root = fs::canonicalize(root).map_err(|_| CliError::UnsafeLog)?;
    let dir = root.join("attempts");
    if fs::symlink_metadata(&dir)
        .map_err(|_| CliError::UnsafeLog)?
        .file_type()
        .is_symlink()
    {
        return Err(CliError::UnsafeLog);
    }
    let dir = fs::canonicalize(dir).map_err(|_| CliError::UnsafeLog)?;
    if dir.parent() != Some(root.as_path()) {
        return Err(CliError::UnsafeLog);
    }
    Ok(dir)
}

fn private_file(path: &Path) -> Result<(File, fs::Metadata), CliError> {
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let before = fs::symlink_metadata(path).map_err(|_| CliError::UnsafeLog)?;
    if !before.file_type().is_file() {
        return Err(CliError::UnsafeLog);
    }
    #[cfg(unix)]
    if before.permissions().mode() & 0o077 != 0 {
        return Err(CliError::UnsafeLog);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path).map_err(|_| CliError::UnsafeLog)?;
    let meta = file.metadata().map_err(|_| CliError::UnsafeLog)?;
    if !meta.is_file() {
        return Err(CliError::UnsafeLog);
    }
    #[cfg(unix)]
    if meta.dev() != before.dev()
        || meta.ino() != before.ino()
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(CliError::UnsafeLog);
    }
    Ok((file, meta))
}

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

fn log_paths(
    dir: &Path,
    attempt: &str,
    receipt: Option<&ExitReceipt>,
) -> Result<[PathBuf; 2], CliError> {
    let paths = [
        dir.join(format!("{attempt}.stdout.log")),
        dir.join(format!("{attempt}.stderr.log")),
    ];
    let recorded_paths_match = receipt.is_none_or(|r| {
        [&r.stdout_path, &r.stderr_path]
            .into_iter()
            .zip(&paths)
            .all(|(recorded, expected)| {
                fs::canonicalize(recorded).is_ok_and(|canonical| canonical == *expected)
            })
    });
    if !recorded_paths_match {
        return Err(CliError::UnsafeLog);
    }
    Ok(paths)
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

fn logs(
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

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
