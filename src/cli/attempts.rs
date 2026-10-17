//! Persisted attempt history and its task-scoped display metadata.
use super::error::CliError;
use crate::supervisor::LaunchPlan;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

pub(crate) struct AttemptHistory {
    pub(crate) attempts: Vec<Value>,
    pub(crate) session: Option<String>,
}

impl AttemptHistory {
    pub(crate) fn active_id(&self) -> Option<&str> {
        self.attempts
            .iter()
            .rev()
            .find(|attempt| attempt["reservation"] == "reserved")
            .and_then(|attempt| attempt["id"].as_str())
    }

    pub(crate) fn latest_id(&self) -> Option<&str> {
        self.attempts
            .last()
            .and_then(|attempt| attempt["id"].as_str())
    }

    pub(crate) fn latest_outcome(&self) -> Option<&str> {
        self.attempts
            .last()
            .and_then(|attempt| attempt["outcome"].as_str())
    }
}

pub(crate) fn load(conn: &Connection, task: &str) -> Result<AttemptHistory, CliError> {
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
    Ok(AttemptHistory { attempts, session })
}
