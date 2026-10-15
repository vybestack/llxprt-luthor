//! Independent durable inbox: submitting an operator decision must not acquire
//! the coordinator lock or mutate the task journal. Only the coordinator drains it.
use super::StateError;
use crate::config::Config;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestKind {
    Resume,
    Retry {
        revision: String,
        actor: String,
        reason: String,
        revalidate_terminal_exit: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub task_id: String,
    pub previous_attempt_id: String,
    /// Stable across restarts: a recorded attempt is never dispatched again.
    pub attempt_id: String,
    pub config: Config,
    pub kind: RequestKind,
}

fn database(root: &Path) -> Result<Connection, StateError> {
    let dir = root.join("operator-requests");
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let db = Connection::open(dir.join("inbox.sqlite3"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "FULL")?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS requests (
        sequence INTEGER PRIMARY KEY AUTOINCREMENT,
        task_id TEXT NOT NULL, previous_attempt_id TEXT NOT NULL,
        attempt_id TEXT NOT NULL UNIQUE, payload TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'pending', reason TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
        CREATE UNIQUE INDEX IF NOT EXISTS one_pending_request
        ON requests(task_id,previous_attempt_id) WHERE status='pending';",
    )?;
    Ok(db)
}

/// Coalesce identical operator decisions, but never replace a different pending
/// authorization. Commit with FULL synchronous durability before acknowledging.
pub(crate) fn enqueue(request: &Request, reason: &str) -> Result<String, StateError> {
    let mut db = database(&request.config.state_root)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let existing: Option<String> = tx.query_row(
        "SELECT payload FROM requests WHERE task_id=?1 AND previous_attempt_id=?2 AND status='pending'",
        params![request.task_id, request.previous_attempt_id], |row| row.get(0),
    ).optional()?;
    if let Some(payload) = existing {
        let existing: Request = serde_json::from_str(&payload)?;
        let mut comparable = request.clone();
        comparable.attempt_id = existing.attempt_id.clone();
        if serde_json::to_value(&comparable)? != serde_json::to_value(&existing)? {
            return Err(StateError::LaunchBlocked);
        }
        return Ok(existing.attempt_id);
    }
    tx.execute("INSERT INTO requests(task_id,previous_attempt_id,attempt_id,payload,reason) VALUES(?1,?2,?3,?4,?5)",
        params![request.task_id,request.previous_attempt_id,request.attempt_id,serde_json::to_string(request)?,reason])?;
    tx.commit()?;
    Ok(request.attempt_id.clone())
}

pub(crate) fn pending(root: &Path) -> Result<Vec<Request>, StateError> {
    let db = database(root)?;
    let mut statement =
        db.prepare("SELECT payload FROM requests WHERE status='pending' ORDER BY sequence")?;
    let payloads = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    payloads
        .into_iter()
        .map(|payload| serde_json::from_str(&payload).map_err(StateError::from))
        .collect()
}

pub(crate) fn update(
    root: &Path,
    attempt: &str,
    status: &str,
    reason: &str,
) -> Result<(), StateError> {
    let db = database(root)?;
    if db.execute(
        "UPDATE requests SET status=?2,reason=?3 WHERE attempt_id=?1 AND status='pending'",
        params![attempt, status, reason],
    )? != 1
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

/// Read-only status is available even while the daemon owns the coordinator.
/// Do not expose private config snapshots, prompts or operator-supplied text.
pub fn summaries(root: &Path, task: Option<&str>) -> Result<Vec<Value>, StateError> {
    let path = root.join("operator-requests/inbox.sqlite3");
    if !path.try_exists()? {
        return Ok(Vec::new());
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = db.prepare("SELECT task_id,previous_attempt_id,attempt_id,status,reason,created_at FROM requests WHERE (?1 IS NULL OR task_id=?1) ORDER BY sequence")?;
    Ok(statement
        .query_map([task], |row| {
            Ok(json!({
                "task_id":row.get::<_,String>(0)?,"previous_attempt_id":row.get::<_,String>(1)?,
                "attempt_id":row.get::<_,String>(2)?,"status":row.get::<_,String>(3)?,
                "reason":row.get::<_,String>(4)?,"created_at":row.get::<_,String>(5)?
            }))
        })?
        .collect::<Result<_, _>>()?)
}
