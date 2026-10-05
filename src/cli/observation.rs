use super::{
    CliError,
    observation_labels::{observation_stage, pr_stage_label},
};
use crate::{
    cli_identifiers::valid_attempt,
    github::pull_request::ErrorCategory,
    state::{PausePrEvidence, PausePrStatus},
};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};

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

struct Observation<'a> {
    conn: &'a Connection,
    task: &'a str,
    stage: &'a str,
    attempt: Option<&'a str>,
    recorded_secs: Option<i64>,
}

fn utc(conn: &Connection, seconds: i64) -> Option<String> {
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', ?1, 'unixepoch')",
        [seconds],
        |row| row.get(0),
    )
    .ok()
    .flatten()
}

fn source_summary(observation: &Observation<'_>, payload: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(payload).ok()?;
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
        return None;
    }
    let source = serde_json::from_value::<SourceObservation>(value).ok()?;
    if source.task_id != observation.task
        || observation.attempt.is_some()
        || source.status != "held"
        || source.reasons.is_empty()
        || source
            .reasons
            .iter()
            .any(|reason| safe_source_reason(reason).is_none())
    {
        return None;
    }
    let seconds = observation.recorded_secs?;
    let observed_at_utc = utc(observation.conn, seconds)?;
    let code = if source
        .reasons
        .iter()
        .any(|reason| reason == "source_read_failed")
    {
        "source_read_failed"
    } else {
        safe_source_reason(&source.reasons[0]).expect("validated reason")
    };
    Some(
        json!({"stage":observation.stage,"attempt_id":null,"category":"source_read",
        "observed_at_utc":observed_at_utc,"observed_at_unix_secs":seconds,
        "code":code,"http_status":null}),
    )
}

fn error_category(category: ErrorCategory) -> &'static str {
    match category {
        ErrorCategory::Permission => "permission",
        ErrorCategory::RateLimit => "rate_limit",
        ErrorCategory::NotFound => "not_found",
        ErrorCategory::Malformed => "malformed",
        ErrorCategory::Transport => "transport",
        ErrorCategory::Unknown => "unknown",
    }
}

fn pr_summary(observation: &Observation<'_>, payload: &str) -> Option<Value> {
    let proof = serde_json::from_str::<PausePrEvidence>(payload).ok()?;
    let id = observation.attempt.filter(|id| valid_attempt(id))?;
    let seconds = i64::try_from(proof.observed_at_unix_secs).ok()?;
    let observed_at_utc = (seconds > 0)
        .then(|| utc(observation.conn, seconds))
        .flatten()?;
    let category = match proof.status {
        PausePrStatus::Absent => "absent",
        PausePrStatus::Open => "open",
        PausePrStatus::Ambiguous => "ambiguous",
        PausePrStatus::Error {
            category,
            code,
            http_status,
        } => {
            let code = safe_code(&code)?;
            if http_status.is_some_and(|status| !(100..=599).contains(&status)) {
                return None;
            }
            return Some(
                json!({"stage":observation.stage,"attempt_id":id,"category":"error",
                "error_category":error_category(category),"observed_at_utc":observed_at_utc,
                "observed_at_unix_secs":seconds,"code":code,"http_status":http_status}),
            );
        }
    };
    Some(
        json!({"stage":observation.stage,"attempt_id":id,"category":category,
        "observed_at_utc":observed_at_utc,"observed_at_unix_secs":seconds,
        "code":null,"http_status":null}),
    )
}

pub(crate) fn observation_summary(
    conn: &Connection,
    task: &str,
    kind: &str,
    attempt: Option<&str>,
    payload: &str,
    recorded_secs: Option<i64>,
) -> Value {
    let stage = observation_stage(kind);
    let observation = Observation {
        conn,
        task,
        stage,
        attempt,
        recorded_secs,
    };
    let summary = if payload.len() > 64 * 1024 {
        None
    } else if stage == "source_observation" {
        source_summary(&observation, payload)
    } else {
        pr_summary(&observation, payload)
    };
    summary.unwrap_or_else(|| {
        json!({"stage":stage,
        "attempt_id":attempt.filter(|id| valid_attempt(id)),"category":"malformed",
        "observed_at_utc":recorded_secs.and_then(|seconds| utc(conn, seconds)),
        "observed_at_unix_secs":recorded_secs,"code":null,"http_status":null})
    })
}

#[derive(Default)]
pub(crate) struct ObservationSummaries {
    pub(crate) pr: Option<Value>,
    pub(crate) source: Option<Value>,
    pub(crate) latest: Option<Value>,
    pub(crate) sequence: i64,
}
pub(crate) fn observations(
    conn: &Connection,
    task: &str,
) -> Result<ObservationSummaries, CliError> {
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

pub(crate) fn displayed_reason(
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
