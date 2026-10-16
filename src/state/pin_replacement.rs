//! Audited replacement of a recorded build, without rewriting session history.
use super::{StateStore, pin_adoption, pin_quiescence};
use crate::{
    config::Config,
    model::{LaunchPlan, StateError},
    ownership::WorktreeOwner,
    worker_build::WorkerBuild,
    worker_capability::{self, SessionStateVersions},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub use super::pin_adoption::PinAdoptionRequest as PinReplacementRequest;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinReplacement {
    pub task_id: String,
    pub actor: String,
    pub reason: String,
    pub executable: std::path::PathBuf,
    pub previous_worker_build: WorkerBuild,
    pub worker_build: WorkerBuild,
    /// Attempts existing before this authorization, including earlier pin eras.
    pub historical_attempts: Vec<String>,
    pub session_state_version: u64,
    pub capability: SessionStateVersions,
    pub observed_at_unix_secs: u64,
    pub phase: String,
}

fn refused(reason: &'static str) -> StateError {
    StateError::PinReplacementRefused(reason)
}

/// Preview probes only the requested binary, not the task session. No DB writes.
pub fn preview(
    config: &Config,
    task: &str,
    request: PinReplacementRequest<'_>,
) -> Result<PinReplacement, StateError> {
    let db = Connection::open_with_flags(
        config.state_root.join("state.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let tx = db.unchecked_transaction()?;
    let _owner = owner(&config.state_root, task)?;
    let audit = inspect(&tx, config, task, request)?;
    tx.commit()?;
    Ok(audit)
}

/// Revalidate under both locks and atomically append provenance. No launch.
pub fn replace(
    store: &mut StateStore,
    config: &Config,
    task: &str,
    request: PinReplacementRequest<'_>,
) -> Result<PinReplacement, StateError> {
    if store.root() != config.state_root {
        return Err(refused(
            "configuration state root differs from the opened store",
        ));
    }
    let _owner = owner(store.root(), task)?;
    let tx = store.connection.transaction()?;
    let audit = inspect(&tx, config, task, request)?;
    tx.execute(
        "INSERT INTO evidence(task_id,attempt_id,kind,payload) VALUES(?1,NULL,'worker_pin_replaced',?2)",
        params![task, serde_json::to_string(&audit)?],
    )?;
    tx.commit()?;
    Ok(audit)
}

fn owner(root: &Path, task: &str) -> Result<WorktreeOwner, StateError> {
    WorktreeOwner::acquire_existing(root, task)
        .map_err(|_| refused("worktree owner is live or unavailable; reconcile first"))
}

fn inspect(
    db: &Connection,
    config: &Config,
    task: &str,
    request: PinReplacementRequest<'_>,
) -> Result<PinReplacement, StateError> {
    if request.actor.trim().is_empty() || request.reason.trim().is_empty() {
        return Err(refused("actor and reason are required"));
    }
    let phase = pin_quiescence::require_idle(db, task)?;
    super::continuation_hold::require_unamended_task(db, task)?;
    let plans = plans(db, task)?;
    let first = plans
        .first()
        .ok_or_else(|| refused("task has no recorded pin"))?;
    let previous_worker_build = effective_build(db, first)?.ok_or_else(|| {
        refused("task is legacy_unrecorded; use adopt-pin for lifetime provenance")
    })?;
    if !valid_build(&previous_worker_build) {
        return Err(StateError::LaunchBlocked);
    }
    for plan in &plans {
        if effective_build(db, plan)?.as_ref() != Some(&previous_worker_build) {
            return Err(StateError::LaunchBlocked);
        }
    }
    if request.executable.as_os_str() != config.initial.executable.as_os_str()
        && request.executable.as_os_str() != config.resume.executable.as_os_str()
    {
        return Err(refused(
            "executable differs from the current config initial/resume template",
        ));
    }
    let worker_build = WorkerBuild::capture(request.executable, &first.worktree)?;
    if request.sha256.len() != 64 || worker_build.sha256 != request.sha256.to_ascii_lowercase() {
        return Err(refused("executable SHA-256 does not match"));
    }
    if worker_build == previous_worker_build {
        return Err(refused("replacement is already the current pin"));
    }
    let session_state_version = recorded_version(db, task)?;
    let capability =
        worker_capability::attest(&worker_build, &first.worktree, session_state_version)?;
    // The probe cannot authorize work that became active during inspection.
    pin_quiescence::require_idle(db, task)?;
    Ok(PinReplacement {
        task_id: task.into(),
        actor: request.actor.into(),
        reason: request.reason.into(),
        executable: request.executable.into(),
        previous_worker_build,
        worker_build,
        historical_attempts: plans.iter().map(|plan| plan.attempt_id.clone()).collect(),
        session_state_version,
        capability,
        observed_at_unix_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_secs(),
        phase,
    })
}

fn recorded_version(db: &Connection, task: &str) -> Result<u64, StateError> {
    let payload: Option<String> = db.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind='session_state_format' ORDER BY sequence DESC LIMIT 1",
        [task], |row| row.get(0),
    ).optional()?;
    let value: serde_json::Value = serde_json::from_str(&payload.ok_or_else(|| {
        refused("session state version is unrecorded; compatibility cannot be attested")
    })?)?;
    value["version"]
        .as_u64()
        .ok_or_else(|| refused("invalid recorded session state version"))
}

fn plans(db: &Connection, task: &str) -> Result<Vec<LaunchPlan>, StateError> {
    let rows: Vec<(String, String)> = db.prepare(
        "SELECT a.id,i.detail FROM attempts a JOIN intents i ON i.task_id=a.task_id AND i.attempt_id=a.id AND i.kind='launch' WHERE a.task_id=?1 ORDER BY a.rowid",
    )?.query_map([task], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
    rows.into_iter()
        .map(|(attempt, payload)| {
            let plan: LaunchPlan = serde_json::from_str(&payload)?;
            if plan.task_id != task || plan.attempt_id != attempt || plan.session_id != task {
                return Err(StateError::LaunchBlocked);
            }
            Ok(plan)
        })
        .collect()
}

/// Validate each historical plan against its own pin era, then resolve the
/// current authorized build for continuation. Never hash obsolete binaries.
pub(crate) fn effective_build(
    db: &Connection,
    plan: &LaunchPlan,
) -> Result<Option<WorkerBuild>, StateError> {
    let payloads = replacement_payloads(db, &plan.task_id)?;
    if payloads.is_empty() {
        // No new authorization: retain the existing legacy/read-only contract.
        return pin_adoption::effective_build(db, plan);
    }
    let all = plans(db, &plan.task_id)?;
    let first = all.first().ok_or(StateError::LaunchBlocked)?;
    let initial = pin_adoption::effective_build(db, first)?;
    let replacements = replacements(&plan.task_id, initial.as_ref(), payloads)?;
    let original = initial.ok_or(StateError::LaunchBlocked)?;
    let expected = replacements
        .iter()
        .find(|audit| audit.historical_attempts.contains(&plan.attempt_id))
        .map(|audit| &audit.previous_worker_build)
        .or_else(|| replacements.last().map(|audit| &audit.worker_build))
        .ok_or(StateError::LaunchBlocked)?;
    match &plan.worker_build {
        Some(build) if build == expected => {}
        None if *expected == original
            && pin_adoption::effective_build(db, plan)?.as_ref() == Some(&original) => {}
        _ => return Err(StateError::LaunchBlocked),
    }
    Ok(replacements.last().map(|audit| audit.worker_build.clone()))
}

fn replacement_payloads(db: &Connection, task: &str) -> Result<Vec<String>, StateError> {
    Ok(db.prepare(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='worker_pin_replaced' ORDER BY sequence",
    )?.query_map([task], |row| row.get(0))?.collect::<Result<_, _>>()?)
}

fn replacements(
    task: &str,
    initial: Option<&WorkerBuild>,
    rows: Vec<String>,
) -> Result<Vec<PinReplacement>, StateError> {
    let mut audits = Vec::new();
    let mut previous = initial.cloned();
    let mut attempts = Vec::new();
    for payload in rows {
        let audit: PinReplacement = serde_json::from_str(&payload)?;
        if audit.task_id != task
            || audit.actor.trim().is_empty()
            || audit.reason.trim().is_empty()
            || audit.observed_at_unix_secs == 0
            || !valid_build(&audit.worker_build)
            || !valid_build(&audit.previous_worker_build)
            || previous.as_ref() != Some(&audit.previous_worker_build)
            || audit.previous_worker_build == audit.worker_build
            || audit.historical_attempts.is_empty()
            || !attempts
                .iter()
                .all(|id| audit.historical_attempts.contains(id))
            || !audit
                .capability
                .session_state_versions
                .contains(&audit.session_state_version)
        {
            return Err(StateError::LaunchBlocked);
        }
        previous = Some(audit.worker_build.clone());
        attempts = audit.historical_attempts.clone();
        audits.push(audit);
    }
    Ok(audits)
}

fn valid_build(build: &WorkerBuild) -> bool {
    build.path.is_absolute()
        && build.sha256.len() == 64
        && build
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
