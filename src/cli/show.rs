use super::{
    CliError,
    history::{attempt_history, cached_verified_pr, events},
    observation::{ObservationSummaries, displayed_reason, observations},
    output::{OutputAge, log_observation},
    process::operator_state,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use std::path::Path;

pub(crate) fn show(conn: &Connection, root: &Path, task: &str) -> Result<Value, CliError> {
    let TaskIdentity {
        phase,
        repository,
        number,
    } = task_identity(conn, task)?;
    let ShowDetails {
        evidence,
        intents,
        candidate,
        worktree,
    } = show_details(conn, task)?;
    let (mut attempts, session) = attempt_history(conn, task)?;
    let reserved = reserved_slot(conn, task)?;
    let active_attempt = attempts
        .iter()
        .rev()
        .find(|a| a["reservation"] == "reserved");
    let latest_attempt_id = attempts
        .last()
        .and_then(|a| a["id"].as_str())
        .map(str::to_owned);
    let latest_attempt_outcome = attempts
        .last()
        .and_then(|a| a["outcome"].as_str())
        .map(str::to_owned);
    let verified_pr = if phase == "pr_complete" {
        Some(cached_verified_pr(conn, task)?)
    } else {
        None
    };
    let active_attempt_id = active_attempt
        .and_then(|a| a["id"].as_str())
        .map(str::to_owned);
    let DisplayObservation {
        phase: display_phase,
        process,
        reason,
        observations:
            ObservationSummaries {
                pr: last_observed_pr,
                source: last_observed_source,
                latest: last_observation,
                ..
            },
    } = display_observation(conn, root, task, &phase, &mut attempts, &evidence)?;
    let mut output = json!({"task":{"id":task,"repository":repository,"issue_number":number,
        "issue_url":candidate.as_ref().and_then(|v|v.get("issue_url")),
        "source":candidate.as_ref().and_then(|v|v.get("source")),
        "mapping":candidate.as_ref().and_then(|v|v.get("mapping"))},
        "phase":display_phase,"reason":reason,"reserved_slot":reserved,"attempts":attempts,
        "process":process,"intents":intents,"evidence":evidence,"session":session,
        "worktree":worktree.and_then(|v|serde_json::from_str::<Value>(&v).ok()),
        "latest_attempt_id":latest_attempt_id,
        "latest_attempt_outcome":latest_attempt_outcome,
        "latest_attempt_outcome_unavailable_reason":if latest_attempt_outcome.is_none(){Some("attempt has no verified exit outcome")}else{None},
        "pr_state":if verified_pr.is_some(){"open_at_last_verification"}else{"unavailable"},
        "pr_unavailable_reason":if verified_pr.is_none(){Some("show does not perform a fresh exhaustive PR read")}else{None},
        "verified_pr":verified_pr,
        "last_pr_verification_at_unix_secs":if phase == "pr_complete" { evidence.iter().find(|item| item["kind"] == "verified_open_pr").and_then(|item| item["detail"]["observed_at"].as_u64()) } else { None },
        "last_observed_pr":last_observed_pr,
        "last_observed_source":last_observed_source,
        "last_observation":last_observation,
        "last_observed_pr_unavailable_reason":if last_observed_pr.is_none(){Some("no stored PR observation")}else{None}});
    log_observation(
        &mut output,
        root,
        active_attempt_id.as_deref(),
        OutputAge::LogOnly,
    );
    Ok(output)
}

struct ShowDetails {
    evidence: Vec<Value>,
    intents: Vec<Value>,
    candidate: Option<Value>,
    worktree: Option<String>,
}

fn show_details(conn: &Connection, task: &str) -> Result<ShowDetails, CliError> {
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
    Ok(ShowDetails {
        evidence,
        intents,
        candidate,
        worktree,
    })
}

struct DisplayObservation {
    phase: String,
    process: Option<Value>,
    reason: Option<String>,
    observations: ObservationSummaries,
}

fn display_observation(
    conn: &Connection,
    root: &Path,
    task: &str,
    phase: &str,
    attempts: &mut [Value],
    evidence: &[Value],
) -> Result<DisplayObservation, CliError> {
    let active_attempt = attempts
        .iter()
        .rev()
        .find(|a| a["reservation"] == "reserved");
    let reason = evidence
        .iter()
        .rev()
        .find(|e| e["kind"] == "held_reason")
        .and_then(|e| e["detail"].as_str());
    let ObservationSummaries {
        pr: last_observed_pr,
        source: last_observed_source,
        latest: last_observation,
        sequence: observation_sequence,
    } = observations(conn, task)?;
    let reason_sequence: i64 = conn.query_row(
        "SELECT sequence FROM evidence WHERE task_id=?1 AND kind='held_reason' ORDER BY sequence DESC LIMIT 1",
        [task], |row| row.get(0)).optional().map_err(|_| CliError::Database)?.unwrap_or(0);
    let active_attempt_id = active_attempt
        .and_then(|a| a["id"].as_str())
        .map(str::to_owned);
    let (display_phase, process) =
        operator_state(conn, root, task, active_attempt_id.as_deref(), phase)?;
    let reason = displayed_reason(
        &display_phase,
        reason,
        reason_sequence,
        last_observation.as_ref(),
        observation_sequence,
    );
    if (display_phase == "running" || display_phase == "stop_requested")
        && let Some(attempt) = attempts
            .iter_mut()
            .rev()
            .find(|a| a["reservation"] == "reserved")
    {
        attempt["lifecycle"] = json!(display_phase);
    }
    Ok(DisplayObservation {
        phase: display_phase,
        process,
        reason,
        observations: ObservationSummaries {
            pr: last_observed_pr,
            source: last_observed_source,
            latest: last_observation,
            sequence: observation_sequence,
        },
    })
}

struct TaskIdentity {
    phase: String,
    repository: String,
    number: i64,
}

fn task_identity(conn: &Connection, task: &str) -> Result<TaskIdentity, CliError> {
    let row: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT state,repository,issue_number FROM tasks WHERE id=?1",
            [task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| CliError::Database)?;
    let (phase, repository, number) = row.ok_or(CliError::TaskNotFound)?;
    Ok(TaskIdentity {
        phase,
        repository,
        number,
    })
}

fn reserved_slot(conn: &Connection, task: &str) -> Result<bool, CliError> {
    let reserved: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM reservations WHERE task_id=?1 AND status='reserved')",
            [task],
            |r| r.get(0),
        )
        .map_err(|_| CliError::Database)?;
    Ok(reserved)
}
