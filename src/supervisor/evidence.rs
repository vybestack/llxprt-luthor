#[cfg(unix)]
fn verify_dispatch_binding(
    store: &StateStore,
    plan: &LaunchPlan,
) -> Result<Result<(), &'static str>, SupervisorError> {
    for (kind, missing) in [
        ("launch", "missing launch intent"),
        ("supervisor_dispatch", "missing dispatch intent"),
    ] {
        match store.intent_payload(&plan.task_id, &plan.attempt_id, kind) {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(Err(missing)),
            Err(StateError::LaunchBlocked) => {
                return Ok(Err("launch or dispatch plan binding mismatch"));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(
        super::binding::verify_observed_plan(&store.connection, store.root(), plan)
            .map_err(|_| "launch or dispatch plan binding mismatch"),
    )
}

use super::{error::SupervisorError, processes::*};
use crate::model::*;
use crate::state::{StateStore, WorktreeIdentity};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(unix)]
pub(crate) fn reconciliation_plan_evidence(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<Result<(PathBuf, LaunchPlan), &'static str>, SupervisorError> {
    let attempts = store.root().join("attempts");
    use std::os::unix::fs::PermissionsExt;
    let Ok(dir) = fs::symlink_metadata(&attempts) else {
        return Ok(Err("missing attempts directory"));
    };
    if !dir.file_type().is_dir() || dir.permissions().mode() & 0o777 != 0o700 {
        return Ok(Err("unsafe attempts directory"));
    }
    let plan = match reconciliation_dispatch_plan(store, attempt_id, task_id, &attempts)? {
        Ok(plan) => plan,
        Err(reason) => return Ok(Err(reason)),
    };
    let Some(worktree) = store.evidence_payload(task_id, None, "worktree_created")? else {
        return Ok(Err("missing worktree evidence"));
    };
    let Some(worktree) = serde_json::from_str::<WorktreeIdentity>(&worktree).ok() else {
        return Ok(Err("invalid worktree evidence"));
    };
    let record = store.worktree_record(task_id)?;
    if worktree.path != plan.worktree
        || record.as_ref().is_none_or(|r| {
            r.identity.as_ref() != Some(&worktree)
                || r.intent.path != worktree.path
                || r.intent.branch != worktree.branch
                || r.intent.base != worktree.base
                || r.intent.repository != worktree.repository
        })
    {
        return Ok(Err("worktree identity mismatch"));
    }
    let Some(selection) = store.selection_evidence(task_id)? else {
        return Ok(Err("missing selection"));
    };
    if crate::state::selection_for_attempt(store, &plan).is_err()
        || selection.candidate.mapping.code_repository != worktree.repository
    {
        return Ok(Err("selection mismatch"));
    }
    let Some(claim) = store.evidence_payload(task_id, None, "claim_verified")? else {
        return Ok(Err("missing claim evidence"));
    };
    if claim.trim().is_empty() || claim != selection.effective_config.assignment_login {
        return Ok(Err("claim identity mismatch"));
    }
    Ok(Ok((attempts, plan)))
}

#[cfg(unix)]
pub(crate) fn recovery_child_evidence(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    attempts: &Path,
) -> Result<Result<ChildIdentity, &'static str>, SupervisorError> {
    let Some(child_file) = private_bytes(&attempts.join(format!("{attempt_id}.child.json")))
        .and_then(|bytes| serde_json::from_slice::<ChildIdentity>(&bytes).ok())
    else {
        return Ok(Err("missing or invalid child identity"));
    };
    let Some(child_evidence) =
        store.evidence_payload(task_id, Some(attempt_id), "child_registered")?
    else {
        return Ok(Err("missing child registration"));
    };
    if serde_json::from_str::<ChildIdentity>(&child_evidence)
        .ok()
        .as_ref()
        != Some(&child_file)
        || child_file.pid == 0
        || i32::try_from(child_file.pid).is_err()
        || child_file.group_id != child_file.pid
        || child_file.boot_identity.trim().is_empty()
        || child_file.start_identity.trim().is_empty()
    {
        return Ok(Err("child registration mismatch"));
    }
    Ok(Ok(child_file))
}

#[cfg(unix)]
pub(crate) fn reconciliation_process_evidence(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    attempts: &Path,
) -> Result<Result<ReconciliationProcesses, &'static str>, SupervisorError> {
    let Some(child_file) = private_bytes(&attempts.join(format!("{attempt_id}.child.json")))
        .and_then(|bytes| serde_json::from_slice::<ChildIdentity>(&bytes).ok())
    else {
        return Ok(Err("missing or invalid child identity"));
    };
    let Some(child_evidence) =
        store.evidence_payload(task_id, Some(attempt_id), "child_registered")?
    else {
        return Ok(Err("missing child registration"));
    };
    if serde_json::from_str::<ChildIdentity>(&child_evidence)
        .ok()
        .as_ref()
        != Some(&child_file)
        || child_file.pid == 0
        || child_file.group_id != child_file.pid
        || child_file.boot_identity.is_empty()
        || child_file.start_identity.is_empty()
    {
        return Ok(Err("child registration mismatch"));
    }
    if store
        .evidence_payload(task_id, Some(attempt_id), "log_failure")?
        .is_some()
    {
        return Ok(Err("log drain failed"));
    }
    let Some(release) = store.intent_payload(task_id, attempt_id, "gate_release")? else {
        return Ok(Err("missing gate release decision"));
    };
    let Some(ready) = store.evidence_payload(task_id, Some(attempt_id), "supervisor_ready")? else {
        return Ok(Err("missing or invalid supervisor identity"));
    };
    let Some(supervisor) = recorded_process(&ready) else {
        return Ok(Err("missing or invalid supervisor identity"));
    };
    if recorded_process(&release).as_ref() != Some(&supervisor) {
        return Ok(Err("supervisor identity contradiction"));
    }
    let sent = store.evidence_payload(task_id, Some(attempt_id), "gate_sent")?;
    if sent.is_some() && sent.as_deref().and_then(recorded_process).as_ref() != Some(&supervisor) {
        return Ok(Err("supervisor identity contradiction"));
    }
    if supervisor.pid == child_file.pid {
        return Ok(Err("supervisor and child identity contradiction"));
    }
    let tracked = store
        .evidence_payloads(task_id, attempt_id, "tracked_descendant")?
        .into_iter()
        .map(|payload| recorded_process(&payload))
        .collect::<Option<Vec<_>>>();
    let Some(tracked) = tracked else {
        return Ok(Err("invalid tracked descendant identity"));
    };
    Ok(Ok(ReconciliationProcesses {
        child: child_file,
        supervisor,
        tracked,
        sent,
    }))
}

#[cfg(unix)]
pub(crate) struct ReconciliationProcesses {
    pub(crate) child: ChildIdentity,
    pub(crate) supervisor: ProcessIdentity,
    pub(crate) tracked: Vec<ProcessIdentity>,
    pub(crate) sent: Option<String>,
}

#[cfg(unix)]
pub(crate) fn recovery_dispatch_plan(
    store: &StateStore,
    attempt_id: &str,
    task_id: &str,
    attempts: &Path,
) -> Result<Result<LaunchPlan, &'static str>, SupervisorError> {
    let held = |reason| Ok(Err(reason));
    let Some(plan) = private_bytes(&attempts.join(format!("{attempt_id}.plan.json")))
        .and_then(|bytes| serde_json::from_slice::<LaunchPlan>(&bytes).ok())
    else {
        return held("missing or invalid plan");
    };
    if plan.task_id != task_id
        || plan.attempt_id != attempt_id
        || plan.session_id != task_id
        || plan.config_revision.is_empty()
    {
        return held("plan identity mismatch");
    }
    if let Err(reason) = verify_dispatch_binding(store, &plan)? {
        return held(reason);
    }

    Ok(Ok(plan))
}

#[cfg(unix)]
pub(crate) fn reconciliation_dispatch_plan(
    store: &StateStore,
    attempt_id: &str,
    task_id: &str,
    attempts: &Path,
) -> Result<Result<LaunchPlan, &'static str>, SupervisorError> {
    let plan: LaunchPlan = match private_bytes(&attempts.join(format!("{attempt_id}.plan.json")))
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(plan) => plan,
        None => return Ok(Err("missing or invalid plan")),
    };
    if plan.task_id != task_id
        || plan.attempt_id != attempt_id
        || plan.session_id != task_id
        || plan.config_revision.is_empty()
    {
        return Ok(Err("plan identity mismatch"));
    }
    if let Err(reason) = verify_dispatch_binding(store, &plan)? {
        return Ok(Err(reason));
    }
    Ok(Ok(plan))
}
