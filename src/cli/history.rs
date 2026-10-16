use super::{CliError, observation::observation_summary};
use crate::supervisor::{ExitReceipt, LaunchPlan};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

pub(crate) fn cached_verified_pr(conn: &Connection, task: &str) -> Result<Value, CliError> {
    let mut stmt = conn
        .prepare(
            "SELECT attempt_id,COALESCE((SELECT h.payload FROM evidence h WHERE h.task_id=?1 AND h.attempt_id=evidence.attempt_id AND h.kind='pr_head_verified' ORDER BY h.sequence DESC LIMIT 1),payload) FROM evidence WHERE task_id=?1 AND attempt_id=(SELECT attempt_id FROM evidence WHERE task_id=?1 AND kind IN ('verified_open_pr','pr_repair_retry_scheduled') ORDER BY sequence DESC LIMIT 1) AND kind IN ('verified_open_pr','pr_repair_retry_scheduled')",
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
            (
                "evidence",
                "held_reason"
                | "claim_verified"
                | "cleanup_failed"
                | "pr_repair_blocked"
                | "pr_checks_flaky_looking"
                | "pr_checks_repeat_failure",
            ) => Some(json!(payload)),
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
            (
                "evidence",
                "verified_open_pr"
                | "pr_head_verified"
                | "pr_base_merge_adopted"
                | "pr_hold_restored"
                | "cleanup_removed"
                | "cleanup_complete"
                | "pr_health"
                | "pr_repair_result"
                | "pr_failed_jobs_rerun_intent"
                | "pr_failed_jobs_rerun"
                | "pr_failed_jobs_rerun_acknowledged"
                | "pr_repair_pr_lookup"
                | "worker_pin_adopted"
                | "worker_pin_replaced",
            ) => serde_json::from_str::<Value>(&payload).ok(),
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

/// Latest cleanup result, independent of launch/claim holds. A subsequent
/// successful cleanup supersedes the failure in status without erasing history.
pub(crate) fn cleanup_failure(conn: &Connection, task: &str) -> Result<Option<String>, CliError> {
    let result: Option<(String, String)> = conn.query_row(
        "SELECT kind,payload FROM evidence WHERE task_id=?1 AND kind IN ('cleanup_failed','cleanup_complete') ORDER BY sequence DESC LIMIT 1",
        [task], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(|_| CliError::Database)?;
    Ok(result
        .filter(|(kind, _)| kind == "cleanup_failed")
        .map(|(_, reason)| reason))
}

/// The record of why a retired task reached its terminal phase. Every retired
/// task has one; a missing record is damaged state, not an absent value.
pub(crate) fn terminal(
    conn: &Connection,
    task: &str,
    phase: &str,
) -> Result<Option<Value>, CliError> {
    if crate::state::retirement::RetiredPhase::parse(phase).is_none() {
        return Ok(None);
    }
    crate::state::retirement::latest(conn, task)
        .map_err(|_| CliError::Database)?
        .map(Some)
        .ok_or(CliError::Database)
}

pub(crate) fn worker_build(conn: &Connection, task: &str) -> Result<Value, CliError> {
    let payload: Option<String> = conn.query_row(
        "SELECT i.detail FROM attempts a JOIN intents i ON i.task_id=a.task_id AND i.attempt_id=a.id AND i.kind='launch' WHERE a.task_id=?1 ORDER BY a.rowid LIMIT 1",
        [task], |row| row.get(0),
    ).optional().map_err(|_| CliError::Database)?;
    let Some(payload) = payload else {
        return Ok(Value::Null);
    };
    let plan: LaunchPlan = serde_json::from_str(&payload).map_err(|_| CliError::Database)?;
    let format: Option<String> = conn.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind='session_state_format' ORDER BY sequence DESC LIMIT 1",
        [task], |row| row.get(0),
    ).optional().map_err(|_| CliError::Database)?;
    let format: Option<Value> = format
        .map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(|_| CliError::Database)?;
    let build = crate::state::pin_replacement::effective_build(conn, &plan)
        .map_err(|_| CliError::Database)?;
    let mut summary = match build {
        Some(build) => json!({"path":build.path,"sha256":build.sha256,"pin_status":"pinned"}),
        None => json!({"path":plan.executable,"sha256":null,"pin_status":"legacy_unrecorded"}),
    };
    if let Some(format) = format {
        summary["reported_session_state_version"] = format["version"].clone();
    }
    Ok(summary)
}
