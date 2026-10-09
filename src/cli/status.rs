struct StatusTask {
    output: Value,
    task_id: String,
    phase: String,
    reason_sequence: i64,
    active_attempt: Option<String>,
}

use super::{
    CliError,
    history::cached_verified_pr,
    observation::{ObservationSummaries, displayed_reason, observations},
    output::{OutputAge, log_observation},
    process::operator_state,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

fn status_row(r: &rusqlite::Row<'_>, root: &Path) -> rusqlite::Result<StatusTask> {
    let task_id: String = r.get(0)?;
    let phase: String = r.get(1)?;
    let reason_sequence = r.get::<_, Option<i64>>(10)?.unwrap_or(0);
    let active_attempt = r.get::<_, Option<String>>(6)?;
    let mut task = json!({
        "task_id":r.get::<_,String>(0)?, "phase":r.get::<_,String>(1)?,
        "repository":r.get::<_,String>(2)?, "issue_number":r.get::<_,i64>(3)?,
        "reserved_slot":r.get::<_,bool>(4)?, "reason":r.get::<_,Option<String>>(5)?,
        "latest_attempt_id":r.get::<_,Option<String>>(7)?,
        "latest_attempt_outcome":r.get::<_,Option<String>>(8)?,
        "latest_attempt_outcome_unavailable_reason":if r.get::<_,Option<String>>(8)?.is_none(){Some("attempt has no verified exit outcome")}else{None},
        "latest_attempt_lifecycle":r.get::<_,Option<String>>(9)?,
        "pr_state":"unavailable",
        "pr_unavailable_reason":"status does not perform a fresh exhaustive PR read"
    });
    log_observation(
        &mut task,
        root,
        active_attempt.as_deref(),
        OutputAge::AttemptStart(r.get(11)?),
    );
    Ok(StatusTask {
        output: task,
        task_id,
        phase,
        reason_sequence,
        active_attempt,
    })
}

pub(crate) fn status(conn: &Connection, root: &Path) -> Result<Value, CliError> {
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
        (SELECT e.sequence FROM evidence e WHERE e.task_id=t.id AND e.kind='held_reason' ORDER BY e.sequence DESC LIMIT 1),
        (SELECT CAST(strftime('%s', a.created_at) AS INTEGER) FROM attempts a
         JOIN reservations r ON r.attempt_id=a.id
         WHERE a.task_id=t.id AND r.status='reserved' ORDER BY a.rowid DESC LIMIT 1)
        FROM tasks t ORDER BY t.created_at,t.id").map_err(|_| CliError::Database)?;
    let mut tasks = stmt
        .query_map([], |r| status_row(r, root))
        .map_err(|_| CliError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| CliError::Database)?;
    let tasks = tasks
        .iter_mut()
        .map(|task| present_task(conn, root, task))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(
        json!({"tasks":tasks,"capacity":{"reserved":reserved,"limit":capacity},
        "reserved_slot_count":reserved,"latest_telemetry":null,
        "telemetry_unavailable_reason":"no telemetry evidence recorded"}),
    )
}

fn present_task(conn: &Connection, root: &Path, saved: &mut StatusTask) -> Result<Value, CliError> {
    let task_id = saved.task_id.as_str();
    let saved_phase = saved.phase.clone();
    let reason_sequence = saved.reason_sequence;
    let active_attempt = &saved.active_attempt;
    let task = &mut saved.output;
    task["verified_pr"] = Value::Null;
    if task["phase"] == "pr_complete" {
        let verified_pr = cached_verified_pr(conn, task_id)?;
        task["pr_state"] = json!("open_at_last_verification");
        task["pr_unavailable_reason"] = Value::Null;
        task["verified_pr"] = verified_pr;
    }
    let (phase, process) =
        operator_state(conn, root, task_id, active_attempt.as_deref(), &saved_phase)?;
    if phase == "running" || phase == "stop_requested" {
        task["phase"] = json!(phase);
        if task["latest_attempt_id"].as_str() == active_attempt.as_deref() {
            task["latest_attempt_lifecycle"] = json!(phase);
        }
    }
    task["process"] = json!(process);
    let ObservationSummaries {
        pr,
        source,
        latest,
        sequence,
    } = observations(conn, task_id)?;
    let reason = displayed_reason(
        &phase,
        task["reason"].as_str(),
        reason_sequence,
        latest.as_ref(),
        sequence,
    );
    task["reason"] = json!(reason);

    task["last_observed_pr"] = json!(pr);
    task["last_observed_source"] = json!(source);
    task["last_observation"] = json!(latest);
    task["last_observed_pr_unavailable_reason"] = json!(if pr.is_none() {
        Some("no stored PR observation")
    } else {
        None
    });
    Ok(task.clone())
}
