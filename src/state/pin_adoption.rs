//! Explicit, append-only provenance for sessions selected before build pins.
use super::{StateStore, proofs};
use crate::{
    model::{LaunchPlan, SelectionEvidence, StateError},
    ownership::WorktreeOwner,
    worker_build::WorkerBuild,
};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinAdoption {
    pub task_id: String,
    pub actor: String,
    pub reason: String,
    pub executable: std::path::PathBuf,
    pub worker_build: WorkerBuild,
    pub legacy_attempts: Vec<String>,
    pub observed_at_unix_secs: u64,
    pub previous_phase: String,
    pub phase: String,
}

pub struct PinAdoptionRequest<'a> {
    pub executable: &'a Path,
    pub sha256: &'a str,
    pub actor: &'a str,
    pub reason: &'a str,
}

fn refused(reason: &'static str) -> StateError {
    StateError::PinAdoptionRefused(reason)
}

pub(crate) fn adopted(db: &Connection, task: &str) -> Result<Option<PinAdoption>, StateError> {
    proofs::unique_payload(db, true, task, None, "worker_pin_adopted")?
        .map(|payload| {
            let audit: PinAdoption = proofs::parse_saved(&payload)?;
            if audit.task_id != task
                || audit.actor.trim().is_empty()
                || audit.reason.trim().is_empty()
                || audit.observed_at_unix_secs == 0
                || !audit.worker_build.path.is_absolute()
            {
                return Err(StateError::LaunchBlocked);
            }
            Ok(audit)
        })
        .transpose()
}

/// Historical launch/dispatch artifacts remain byte-for-byte unchanged.
pub(crate) fn effective_build(
    db: &Connection,
    plan: &LaunchPlan,
) -> Result<Option<WorkerBuild>, StateError> {
    let audit = adopted(db, &plan.task_id)?;
    match (plan.worker_build.as_ref(), audit) {
        (Some(build), Some(audit)) if *build != audit.worker_build => {
            Err(StateError::LaunchBlocked)
        }
        (Some(build), _) => Ok(Some(build.clone())),
        (None, Some(audit)) if audit.legacy_attempts.contains(&plan.attempt_id) => {
            Ok(Some(audit.worker_build))
        }
        (None, Some(_)) => Err(StateError::LaunchBlocked),
        (None, None) => Ok(None),
    }
}

/// No schema creation, migration, task transition or evidence write in preview.
pub fn preview(
    root: &Path,
    task: &str,
    request: PinAdoptionRequest<'_>,
) -> Result<PinAdoption, StateError> {
    let db =
        Connection::open_with_flags(root.join("state.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tx = db.unchecked_transaction()?;
    let audit = inspect(&tx, task, request)?;
    let _owner = WorktreeOwner::acquire_existing(root, task)
        .map_err(|_| refused("worktree owner is live or unavailable; reconcile first"))?;
    tx.commit()?;
    Ok(audit)
}

pub fn adopt(
    store: &mut StateStore,
    task: &str,
    request: PinAdoptionRequest<'_>,
) -> Result<PinAdoption, StateError> {
    let tx = store.connection.transaction()?;
    let audit = inspect(&tx, task, request)?;
    let _owner = WorktreeOwner::acquire_existing(&store.root, task)
        .map_err(|_| refused("worktree owner is live or unavailable; reconcile first"))?;
    tx.execute("INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'worker_pin_adopted',?2)",
        params![task, serde_json::to_string(&audit)?])?;
    tx.execute(
        "UPDATE tasks SET state=?2 WHERE id=?1",
        params![task, audit.phase],
    )?;
    tx.commit()?;
    Ok(audit)
}

fn inspect(
    db: &Connection,
    task: &str,
    request: PinAdoptionRequest<'_>,
) -> Result<PinAdoption, StateError> {
    if request.actor.trim().is_empty() || request.reason.trim().is_empty() {
        return Err(refused("actor and reason are required"));
    }
    let previous_phase = require_quiescent(db, task)?;
    let (first, legacy_attempts) = legacy_plans(db, task)?;
    let selection: String = db.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection'",
        [task],
        |row| row.get(0),
    )?;
    let selection: SelectionEvidence = serde_json::from_str(&selection)?;
    let config = selection.effective_config;
    if request.executable != config.initial.executable
        && request.executable != config.resume.executable
    {
        return Err(refused("executable is not in the saved selection snapshot"));
    }
    let worker_build = WorkerBuild::capture(request.executable, &first.worktree)?;
    if request.sha256.len() != 64
        || !request.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || worker_build.sha256 != request.sha256.to_ascii_lowercase()
    {
        return Err(refused("executable SHA-256 does not match"));
    }
    let phase = restored_phase(db, task, &previous_phase)?;
    Ok(PinAdoption {
        task_id: task.into(),
        actor: request.actor.into(),
        reason: request.reason.into(),
        executable: request.executable.into(),
        worker_build,
        legacy_attempts,
        observed_at_unix_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_secs(),
        previous_phase,
        phase,
    })
}

fn legacy_plans(db: &Connection, task: &str) -> Result<(LaunchPlan, Vec<String>), StateError> {
    let plans: Vec<(String, String)> = db.prepare("SELECT a.id,i.detail FROM attempts a JOIN intents i ON i.task_id=a.task_id AND i.attempt_id=a.id AND i.kind='launch' WHERE a.task_id=?1 ORDER BY a.rowid")?
        .query_map([task], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
    if plans.is_empty() || adopted(db, task)?.is_some() {
        return Err(refused(
            "task is not legacy_unrecorded; no repinning authorized",
        ));
    }
    let mut first = None;
    let mut attempts = Vec::new();
    for (attempt, payload) in plans {
        let plan: LaunchPlan = serde_json::from_str(&payload)?;
        if plan.task_id != task || plan.attempt_id != attempt || plan.worker_build.is_some() {
            return Err(refused(
                "task is not legacy_unrecorded; no repinning authorized",
            ));
        }
        attempts.push(attempt);
        if first.is_none() {
            first = Some(plan);
        }
    }
    Ok((first.ok_or(StateError::LaunchBlocked)?, attempts))
}

fn require_quiescent(db: &Connection, task: &str) -> Result<String, StateError> {
    let valid: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1)
        AND EXISTS(SELECT 1 FROM attempts WHERE task_id=?1)
        AND NOT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND kind='cleanup')
        AND NOT EXISTS(SELECT 1 FROM reservations WHERE task_id=?1 AND status='reserved')
        AND NOT EXISTS(SELECT 1 FROM attempts a WHERE task_id=?1 AND
            (lifecycle!='completed' OR outcome IS NULL OR
             (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=a.id AND kind='launch')!=1 OR
             (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=a.id AND kind='attempt_exit')!=1))",
        [task], |row| row.get(0))?;
    if !valid {
        return Err(refused(
            "task may be running or has an unverified exit; reconcile first",
        ));
    }
    Ok(
        db.query_row("SELECT state FROM tasks WHERE id=?1", [task], |row| {
            row.get(0)
        })?,
    )
}

// Undo only the specific legacy-build hold, never unrelated safety holds.
fn restored_phase(db: &Connection, task: &str, phase: &str) -> Result<String, StateError> {
    use rusqlite::OptionalExtension;
    let reason: Option<String> = db.query_row("SELECT payload FROM evidence WHERE task_id=?1 AND kind='held_reason' ORDER BY sequence DESC LIMIT 1", [task], |row| row.get(0)).optional()?;
    let pin_hold = reason.is_some_and(|reason| {
        // legacy_plans already proved every plan is unpinned; this bounded
        // repair code carries no executable paths or upstream error text.
        reason == "PR repair held: worker-build-held"
            || ((reason.starts_with("legacy session worker ")
                || reason.starts_with("PR repair held: legacy session worker "))
                && reason.contains("; hash unrecorded; requested template "))
    });
    if phase != "held" || !pin_hold {
        return Ok(phase.into());
    }
    super::continuation_hold::require_unamended_task(db, task)?;
    let pr: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND kind='verified_open_pr' AND attempt_id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1))", [task], |row| row.get(0))?;
    if pr {
        return Ok("pr_complete".into());
    }
    let status: Option<Option<String>> = db.query_row("SELECT CASE WHEN json_extract(payload,'$.status.status')='absent' THEN kind ELSE NULL END FROM evidence WHERE task_id=?1 AND attempt_id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1) AND kind IN ('exit_pr_lookup','pause_pr_lookup') ORDER BY sequence DESC LIMIT 1", [task], |row| row.get(0)).optional()?;
    Ok(match status.flatten().as_deref() {
        Some("exit_pr_lookup") => "attention",
        Some("pause_pr_lookup") => "paused",
        _ => phase,
    }
    .into())
}
