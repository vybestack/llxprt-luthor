mod observation_labels;
use crate::github::pull_request::ErrorCategory;
use crate::state::{PausePrEvidence, PausePrStatus};
#[cfg(unix)]
use crate::supervisor::{ChildIdentity, recorded_process, verified_live_process};
use crate::supervisor::{ExitReceipt, LaunchPlan};
use observation_labels::{observation_stage, pr_stage_label};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

const SILENCE_WARNING_THRESHOLD_SECONDS: u64 = 300;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceObservation {
    task_id: String,
    status: String,
    reasons: Vec<String>,
    #[serde(rename = "issue_state")]
    _issue_state: Option<String>,
    #[serde(rename = "assignees")]
    _assignees: Option<Vec<String>>,
    #[serde(rename = "marker_present")]
    _marker_present: Option<bool>,
    #[serde(rename = "project_membership")]
    _project_membership: Option<bool>,
    #[serde(rename = "worktree")]
    _worktree: Option<SourceWorktree>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceWorktree {
    UnverifiedPathPresent,
    UnverifiedPathAbsent,
    IdentityMatches,
    IdentityMismatch,
}

fn safe_code(code: &str) -> Option<&'static str> {
    Some(match code {
        "transport-error" => "transport-error",
        "command-failed" => "command-failed",
        "invalid-json" => "invalid-json",
        "invalid-page" => "invalid-page",
        "page-limit" => "page-limit",
        "oversized-page" => "oversized-page",
        "invalid-list-entry" => "invalid-list-entry",
        "invalid-pr-id" => "invalid-pr-id",
        "invalid-pr-details" => "invalid-pr-details",
        "page-overflow" => "page-overflow",
        _ => return None,
    })
}

fn safe_source_reason(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "issue_identity_mismatch" => "issue_identity_mismatch",
        "issue_not_open" => "issue_not_open",
        "marker_missing" => "marker_missing",
        "assignees_unexpected" => "assignees_unexpected",
        "project_membership_mismatch" => "project_membership_mismatch",
        "source_read_failed" => "source_read_failed",
        "claim_intent_mismatch" => "claim_intent_mismatch",
        "assignment_not_observed" => "assignment_not_observed",
        "claim_intent_unverified" => "claim_intent_unverified",
        "worktree_intent_mismatch" => "worktree_intent_mismatch",
        "worktree_unverified" => "worktree_unverified",
        "worktree_read_failed" => "worktree_read_failed",
        "selection_missing" => "selection_missing",
        "prelaunch_not_verified" => "prelaunch_not_verified",
        _ => return None,
    })
}

fn observation_summary(
    conn: &Connection,
    task: &str,
    kind: &str,
    attempt: Option<&str>,
    payload: &str,
    recorded_secs: Option<i64>,
) -> Value {
    let stage = observation_stage(kind);
    let id = attempt.filter(|id| valid_attempt(id));
    let utc = |seconds: i64| -> Option<String> {
        conn.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', ?1, 'unixepoch')",
            [seconds],
            |row| row.get(0),
        )
        .ok()
        .flatten()
    };
    let malformed = || {
        json!({"stage":stage,"attempt_id":id,"category":"malformed",
        "observed_at_utc":recorded_secs.and_then(utc),
        "observed_at_unix_secs":recorded_secs,"code":null,"http_status":null})
    };
    if payload.len() > 64 * 1024 {
        return malformed();
    }
    if stage == "source_observation" {
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            return malformed();
        };
        if [
            "task_id",
            "status",
            "reasons",
            "issue_state",
            "assignees",
            "marker_present",
            "project_membership",
            "worktree",
        ]
        .iter()
        .any(|key| value.get(*key).is_none())
        {
            return malformed();
        }
        let Ok(source) = serde_json::from_value::<SourceObservation>(value) else {
            return malformed();
        };
        if source.task_id != task
            || attempt.is_some()
            || source.status != "held"
            || source.reasons.is_empty()
            || source
                .reasons
                .iter()
                .any(|reason| safe_source_reason(reason).is_none())
        {
            return malformed();
        }
        let Some(seconds) = recorded_secs else {
            return malformed();
        };
        let Some(observed_at_utc) = utc(seconds) else {
            return malformed();
        };
        let code = if source
            .reasons
            .iter()
            .any(|reason| reason == "source_read_failed")
        {
            "source_read_failed"
        } else {
            safe_source_reason(&source.reasons[0]).expect("validated reason")
        };
        return json!({"stage":stage,"attempt_id":null,"category":"source_read",
            "observed_at_utc":observed_at_utc,"observed_at_unix_secs":seconds,
            "code":code,"http_status":null});
    }
    let Ok(proof) = serde_json::from_str::<PausePrEvidence>(payload) else {
        return malformed();
    };
    let Some(id) = id else { return malformed() };
    let Ok(seconds) = i64::try_from(proof.observed_at_unix_secs) else {
        return malformed();
    };
    let Some(observed_at_utc) = (seconds > 0).then(|| utc(seconds)).flatten() else {
        return malformed();
    };
    let category = match proof.status {
        PausePrStatus::Absent => "absent",
        PausePrStatus::Open => "open",
        PausePrStatus::Ambiguous => "ambiguous",
        PausePrStatus::Error {
            category,
            code,
            http_status,
        } => {
            let Some(code) = safe_code(&code) else {
                return malformed();
            };
            if http_status.is_some_and(|status| !(100..=599).contains(&status)) {
                return malformed();
            }
            let error_category = match category {
                ErrorCategory::Permission => "permission",
                ErrorCategory::RateLimit => "rate_limit",
                ErrorCategory::NotFound => "not_found",
                ErrorCategory::Malformed => "malformed",
                ErrorCategory::Transport => "transport",
                ErrorCategory::Unknown => "unknown",
            };
            return json!({"stage":stage,"attempt_id":id,"category":"error",
                "error_category":error_category,"observed_at_utc":observed_at_utc,
                "observed_at_unix_secs":seconds,"code":code,"http_status":http_status});
        }
    };
    json!({"stage":stage,"attempt_id":id,"category":category,
        "observed_at_utc":observed_at_utc,"observed_at_unix_secs":seconds,
        "code":null,"http_status":null})
}

#[derive(Default)]
struct ObservationSummaries {
    pr: Option<Value>,
    source: Option<Value>,
    latest: Option<Value>,
    sequence: i64,
}

fn observations(conn: &Connection, task: &str) -> Result<ObservationSummaries, CliError> {
    let mut stmt = conn
        .prepare(
            "SELECT sequence,kind,attempt_id,payload,
        CAST(strftime('%s',created_at) AS INTEGER) FROM evidence
        WHERE task_id=?1 AND kind IN ('pause_pr_lookup','exit_pr_lookup','retry_pr_lookup','source_observation')
        ORDER BY sequence",
        )
        .map_err(|_| CliError::Database)?;
    let rows = stmt
        .query_map([task], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })
        .map_err(|_| CliError::Database)?;
    let mut result = ObservationSummaries::default();
    for row in rows {
        let (seq, kind, attempt, payload, recorded_secs) = row.map_err(|_| CliError::Database)?;
        let summary = observation_summary(
            conn,
            task,
            &kind,
            attempt.as_deref(),
            &payload,
            recorded_secs,
        );
        if kind == "source_observation" {
            result.source = Some(summary.clone());
        } else {
            result.pr = Some(summary.clone());
        }
        result.sequence = seq;
        result.latest = Some(summary);
    }
    Ok(result)
}

fn displayed_reason(
    phase: &str,
    reason: Option<&str>,
    reason_sequence: i64,
    latest: Option<&Value>,
    observation_sequence: i64,
) -> Option<String> {
    if phase != "held" && phase != "stop_requested" {
        return None;
    }
    if observation_sequence > reason_sequence {
        let summary = latest?;
        if summary["category"] == "malformed" {
            return Some("observation malformed".into());
        }
        if summary["stage"] == "source_observation" {
            return Some("source reconciliation held".into());
        }
        let stage = pr_stage_label(summary["stage"].as_str());
        return Some(format!(
            "{stage} PR {}",
            summary["category"].as_str().unwrap_or("malformed")
        ));
    }
    reason.map(str::to_owned)
}

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

#[cfg(unix)]
fn live_process(conn: &Connection, root: &Path, task: &str, attempt: &str) -> Option<Value> {
    use std::os::unix::fs::PermissionsExt;
    let dir = attempts_dir(root).ok()?;
    if fs::metadata(&dir).ok()?.permissions().mode() & 0o777 != 0o700 {
        return None;
    }
    let (plan_file, _) = private_file(&dir.join(format!("{attempt}.plan.json"))).ok()?;
    let plan: LaunchPlan = serde_json::from_reader(plan_file).ok()?;
    if plan.task_id != task || plan.attempt_id != attempt || plan.session_id != task {
        return None;
    }
    let valid: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts a JOIN reservations r ON r.attempt_id=a.id AND r.task_id=a.task_id
         JOIN tasks t ON t.id=a.task_id WHERE a.id=?1 AND a.task_id=?2 AND a.lifecycle='launch_intended'
         AND a.outcome IS NULL AND r.status='reserved' AND t.state='held'
         AND (SELECT COUNT(*) FROM intents WHERE task_id=?2 AND attempt_id=?1 AND kind='launch' AND detail=?3)=1
         AND (SELECT COUNT(*) FROM intents WHERE task_id=?2 AND attempt_id=?1 AND kind='supervisor_dispatch' AND detail=?3)=1
         AND (SELECT COUNT(*) FROM intents WHERE task_id=?2 AND attempt_id=?1 AND kind='gate_release')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND attempt_id=?1 AND kind='child_registered')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND attempt_id=?1 AND kind='supervisor_ready')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND attempt_id=?1 AND kind='gate_sent')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?2 AND attempt_id=?1 AND kind='log_failure')=0)",
        rusqlite::params![attempt, task, serde_json::to_string(&plan).ok()?], |r| r.get(0)
    ).ok()?;
    if !valid
        || dir.join(format!("{attempt}.receipt.json")).exists()
        || dir
            .join(format!("{attempt}.supervisor-error.json"))
            .exists()
    {
        return None;
    }
    let (child_file, _) = private_file(&dir.join(format!("{attempt}.child.json"))).ok()?;
    let child: ChildIdentity = serde_json::from_reader(child_file).ok()?;
    let evidence = |table: &str, kind: &str| -> Option<String> {
        let sql = match table {
            "evidence" => {
                "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind=?3"
            }
            "intents" => {
                "SELECT detail FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind=?3"
            }
            _ => unreachable!(),
        };
        conn.query_row(sql, [task, attempt, kind], |r| r.get(0))
            .ok()
    };
    if serde_json::from_str::<ChildIdentity>(&evidence("evidence", "child_registered")?).ok()?
        != child
    {
        return None;
    }
    let supervisor = recorded_process(&evidence("evidence", "supervisor_ready")?)?;
    if recorded_process(&evidence("evidence", "gate_sent")?)? != supervisor
        || recorded_process(&evidence("intents", "gate_release")?)? != supervisor
        || !verified_live_process(&child, &supervisor)
    {
        return None;
    }
    Some(json!({"attempt_id":attempt,
        "child":{"pid":child.pid,"group_id":child.group_id,
        "boot_identity":child.boot_identity,"start_identity":child.start_identity},
        "supervisor":{"pid":supervisor.pid,"group_id":supervisor.pid,
        "boot_identity":supervisor.boot_identity,"start_identity":supervisor.start_identity}}))
}

fn operator_state(
    conn: &Connection,
    root: &Path,
    task: &str,
    attempt: Option<&str>,
    phase: &str,
) -> Result<(String, Option<Value>), CliError> {
    if phase != "held" {
        return Ok((phase.to_owned(), None));
    }
    let Some(attempt) = attempt.filter(|id| valid_attempt(id)) else {
        return Ok((phase.to_owned(), None));
    };
    let stopped: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop')",
        [task, attempt], |r| r.get(0)
    ).map_err(|_| CliError::Database)?;
    if stopped {
        return Ok(("stop_requested".into(), None));
    }
    #[cfg(unix)]
    if let Some(process) = live_process(conn, root, task, attempt) {
        // Recheck after probing the OS so a concurrent stop cannot be reported as running.
        let stopped: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop')",
            [task, attempt], |r| r.get(0)
        ).map_err(|_| CliError::Database)?;
        if stopped {
            return Ok(("stop_requested".into(), None));
        }
        return Ok(("running".into(), Some(process)));
    }
    #[cfg(not(unix))]
    let _ = root;
    Ok(("held".into(), None))
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

fn cached_verified_pr(conn: &Connection, task: &str) -> Result<Value, CliError> {
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

fn status_row(r: &rusqlite::Row<'_>, root: &Path) -> rusqlite::Result<Value> {
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
        "pr_unavailable_reason":"status does not perform a fresh exhaustive PR read",
        "reason_sequence":r.get::<_,Option<i64>>(10)?.unwrap_or(0)
    });
    log_observation(
        &mut task,
        root,
        active_attempt.as_deref(),
        OutputAge::AttemptStart(r.get(11)?),
    );
    Ok(task)
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
    for task in &mut tasks {
        let task_id = task["task_id"]
            .as_str()
            .ok_or(CliError::Database)?
            .to_owned();
        task["verified_pr"] = Value::Null;
        if task["phase"] == "pr_complete" {
            let verified_pr = cached_verified_pr(conn, &task_id)?;
            task["pr_state"] = json!("open_at_last_verification");
            task["pr_unavailable_reason"] = Value::Null;
            task["verified_pr"] = verified_pr;
        }
        let reserved_attempt: Option<String> = conn
            .query_row(
                "SELECT a.id FROM attempts a JOIN reservations r ON r.attempt_id=a.id
             WHERE a.task_id=?1 AND r.status='reserved' ORDER BY a.rowid DESC LIMIT 1",
                [&task_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| CliError::Database)?;
        let (phase, process) = operator_state(
            conn,
            root,
            &task_id,
            reserved_attempt.as_deref(),
            task["phase"].as_str().ok_or(CliError::Database)?,
        )?;
        if phase == "running" || phase == "stop_requested" {
            task["phase"] = json!(phase);
            if task["latest_attempt_id"].as_str() == reserved_attempt.as_deref() {
                task["latest_attempt_lifecycle"] = json!(phase);
            }
        }
        task["process"] = json!(process);
        let ObservationSummaries {
            pr,
            source,
            latest,
            sequence,
        } = observations(conn, &task_id)?;
        let reason = displayed_reason(
            &phase,
            task["reason"].as_str(),
            task["reason_sequence"].as_i64().unwrap_or(0),
            latest.as_ref(),
            sequence,
        );
        task["reason"] = json!(reason);
        task.as_object_mut()
            .ok_or(CliError::Database)?
            .remove("reason_sequence");
        task["last_observed_pr"] = json!(pr);
        task["last_observed_source"] = json!(source);
        task["last_observation"] = json!(latest);
        task["last_observed_pr_unavailable_reason"] = json!(if pr.is_none() {
            Some("no stored PR observation")
        } else {
            None
        });
    }
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

fn receipt_summary(receipt: &ExitReceipt) -> Value {
    json!({"attempt_id":receipt.attempt_id,"child_pid":receipt.child_pid,
        "exit_code":receipt.exit_code,"signal":receipt.signal,
        "stdout_path":receipt.stdout_path,"stdout_bytes":receipt.stdout_bytes,
        "stderr_path":receipt.stderr_path,"stderr_bytes":receipt.stderr_bytes,
        "stop_signals":receipt.stop_signals})
}

fn attempt_history(
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

enum OutputAge {
    LogOnly,
    AttemptStart(Option<i64>),
}

fn log_observation(output: &mut Value, root: &Path, attempt: Option<&str>, fallback: OutputAge) {
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
    let (mut attempts, session) = attempt_history(conn, task)?;
    let reserved: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM reservations WHERE task_id=?1 AND status='reserved')",
            [task],
            |r| r.get(0),
        )
        .map_err(|_| CliError::Database)?;
    let active_attempt = attempts
        .iter()
        .rev()
        .find(|a| a["reservation"] == "reserved");
    let reason = evidence
        .iter()
        .rev()
        .find(|e| e["kind"] == "held_reason")
        .and_then(|e| e["detail"].as_str());
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
        operator_state(conn, root, task, active_attempt_id.as_deref(), &phase)?;
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
