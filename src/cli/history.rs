use super::{CliError, observation::observation_summary};
use crate::supervisor::{ExitReceipt, LaunchPlan};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

pub(crate) fn cached_verified_pr(conn: &Connection, task: &str) -> Result<Value, CliError> {
    let mut stmt = conn
        .prepare(
            "SELECT attempt_id,payload FROM evidence WHERE task_id=?1 AND kind='verified_open_pr'",
        )
        .map_err(|_| CliError::Database)?;
    let rows = stmt
        .query_map([task], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| CliError::Database)?;
    let proofs = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| CliError::Database)?;
    let [(evidence_attempt, payload)] = proofs.as_slice() else {
        return Err(CliError::Database);
    };
    let detail: Value = serde_json::from_str(payload).map_err(|_| CliError::Database)?;
    let id = detail["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or(CliError::Database)?;
    let url = detail["url"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(CliError::Database)?;
    let repository = detail["repository"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(CliError::Database)?;
    let head_repository = detail["head_repository"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(CliError::Database)?;
    let draft = detail["draft"].as_bool().ok_or(CliError::Database)?;
    let checks = match &detail["checks"] {
        Value::Null => Value::Null,
        Value::Array(checks) if checks.iter().all(Value::is_string) => json!(checks),
        _ => return Err(CliError::Database),
    };
    let observed_at = detail["observed_at"]
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or(CliError::Database)?;
    let attempt_id = detail["attempt_id"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(CliError::Database)?;
    if evidence_attempt.as_deref() != Some(attempt_id) {
        return Err(CliError::Database);
    }
    Ok(
        json!({"id":id,"url":url,"repository":repository,"head_repository":head_repository,
        "draft":draft,"checks":checks,"observed_at":observed_at,"attempt_id":attempt_id}),
    )
}

pub(crate) fn events(conn: &Connection, table: &str, task: &str) -> Result<Vec<Value>, CliError> {
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
            (
                "evidence",
                "pause_pr_lookup" | "exit_pr_lookup" | "retry_pr_lookup" | "source_observation",
            ) => Some(observation_summary(
                conn,
                task,
                &kind,
                attempt_id.as_deref(),
                &payload,
                created_at_unix_secs,
            )),
            ("evidence", "attempt_exit") => serde_json::from_str::<ExitReceipt>(&payload)
                .ok()
                .map(|receipt| receipt_summary(&receipt)),
            ("evidence", "verified_open_pr") => serde_json::from_str::<Value>(&payload).ok(),
            _ => None,
        };
        Ok(
            json!({"attempt_id":attempt_id,"kind":kind,"created_at":created_at,
                "created_at_unix_secs":created_at_unix_secs,"detail":detail}),
        )
    })
    .collect()
}

pub(crate) fn receipt_summary(receipt: &ExitReceipt) -> Value {
    json!({"attempt_id":receipt.attempt_id,"child_pid":receipt.child_pid,
        "exit_code":receipt.exit_code,"signal":receipt.signal,
        "stdout_path":receipt.stdout_path,"stdout_bytes":receipt.stdout_bytes,
        "stderr_path":receipt.stderr_path,"stderr_bytes":receipt.stderr_bytes,
        "stop_signals":receipt.stop_signals})
}

pub(crate) fn attempt_history(
    conn: &Connection,
    task: &str,
) -> Result<(Vec<Value>, Option<String>), CliError> {
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
    Ok((attempts, session))
}
