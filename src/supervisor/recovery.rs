use super::{error::SupervisorError, evidence::*, processes::*};
use crate::model::*;
use crate::{
    state::{StateStore, WorktreeIdentity},
    worktree,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Read-only preliminary check. Quiescence is withheld until all durable identity proofs are available.
pub fn inspect_recovery_quiescence(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<RecoveryInspection, SupervisorError> {
    if !valid_attempt(attempt_id) {
        return Err(SupervisorError::Conflict);
    }
    if store.latest_attempt(task_id)?.as_deref() != Some(attempt_id) {
        return Ok(RecoveryInspection::Held(
            "attempt is not the latest attempt",
        ));
    }
    let receipt_path = store
        .root()
        .join("attempts")
        .join(format!("{attempt_id}.receipt.json"));
    match fs::symlink_metadata(&receipt_path) {
        Ok(_) => return Ok(RecoveryInspection::Held("attempt receipt path exists")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Ok(RecoveryInspection::Held(
                "attempt receipt path is unreadable",
            ));
        }
    }
    if !store.active_attempt_reservation(task_id, attempt_id)? {
        return Ok(RecoveryInspection::Held(
            "attempt is not the active reserved attempt",
        ));
    }
    if store
        .evidence_payload(task_id, Some(attempt_id), "attempt_exit")?
        .is_some()
    {
        return Ok(RecoveryInspection::Held("attempt exit receipt exists"));
    }

    let (attempts, plan) = match recovery_plan_evidence(store, task_id, attempt_id)? {
        Ok(context) => context,
        Err(reason) => return Ok(RecoveryInspection::Held(reason)),
    };
    let held = |reason| Ok(RecoveryInspection::Held(reason));
    let child_file = match recovery_child_evidence(store, task_id, attempt_id, &attempts)? {
        Ok(child) => child,
        Err(reason) => return held(reason),
    };
    if store
        .evidence_payload(task_id, Some(attempt_id), "log_failure")?
        .is_some()
    {
        return held("log drain failed");
    }
    if attempts
        .join(format!("{attempt_id}.supervisor-error.json"))
        .exists()
    {
        return held("supervisor error receipt exists");
    }
    recovery_process_quiescence(store, task_id, attempt_id, &attempts, &child_file, &plan)
}

#[cfg(unix)]
fn recovery_plan_evidence(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
) -> Result<Result<(PathBuf, LaunchPlan), &'static str>, SupervisorError> {
    let held = |reason| Ok(Err(reason));
    let attempts = store.root().join("attempts");
    use std::os::unix::fs::PermissionsExt;
    let Ok(dir) = fs::symlink_metadata(&attempts) else {
        return held("missing attempts directory");
    };
    if !dir.file_type().is_dir() || dir.permissions().mode() & 0o777 != 0o700 {
        return held("unsafe attempts directory");
    }
    let plan = match recovery_dispatch_plan(store, attempt_id, task_id, &attempts)? {
        Ok(plan) => plan,
        Err(reason) => return Ok(Err(reason)),
    };
    let Some(worktree) = store.evidence_payload(task_id, None, "worktree_created")? else {
        return held("missing worktree evidence");
    };
    let Some(worktree) = serde_json::from_str::<WorktreeIdentity>(&worktree).ok() else {
        return held("invalid worktree evidence");
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
        || !worktree::matches_snapshot(&worktree, &plan.expected_worktree).unwrap_or(false)
    {
        return held("worktree identity mismatch");
    }
    if worktree::verify_snapshot(&plan.expected_worktree).is_err() {
        return held("worktree snapshot mismatch");
    }
    let Some(selection) = store.selection_evidence(task_id)? else {
        return held("missing selection");
    };
    if crate::state::selection_for_attempt(store, &plan).is_err()
        || selection.candidate.mapping.code_repository != worktree.repository
    {
        return held("selection mismatch");
    }
    let Some(claim) = store.evidence_payload(task_id, None, "claim_verified")? else {
        return held("missing claim evidence");
    };
    if claim.trim().is_empty() || claim != selection.effective_config.assignment_login {
        return held("claim identity mismatch");
    }

    Ok(Ok((attempts, plan)))
}

#[cfg(unix)]
fn recovery_process_quiescence(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    attempts: &Path,
    child_file: &ChildIdentity,
    plan: &LaunchPlan,
) -> Result<RecoveryInspection, SupervisorError> {
    let receipt_path = attempts.join(format!("{attempt_id}.receipt.json"));
    let held = |reason| Ok(RecoveryInspection::Held(reason));
    let Some(release) = store.intent_payload(task_id, attempt_id, "gate_release")? else {
        return held("missing gate release decision");
    };
    let Some(ready) = store.evidence_payload(task_id, Some(attempt_id), "supervisor_ready")? else {
        return held("missing or invalid supervisor identity");
    };
    let Some(supervisor) = recorded_process(&ready) else {
        return held("missing or invalid supervisor identity");
    };
    if recorded_process(&release).as_ref() != Some(&supervisor) {
        return held("supervisor identity contradiction");
    }
    let Some(sent) = store.evidence_payload(task_id, Some(attempt_id), "gate_sent")? else {
        return held("missing gate sent evidence");
    };
    if recorded_process(&sent).as_ref() != Some(&supervisor) {
        return held("supervisor identity contradiction");
    }
    if supervisor.pid == child_file.pid {
        return held("supervisor and child identity contradiction");
    }

    let tracked = store
        .evidence_payloads(task_id, attempt_id, "tracked_descendant")?
        .into_iter()
        .map(|payload| recorded_process(&payload))
        .collect::<Option<Vec<_>>>();
    let Some(tracked) = tracked else {
        return held("invalid tracked descendant identity");
    };
    if !registered_processes_absent(child_file, &supervisor, &tracked) {
        return held("registered processes may still be live");
    }

    recovery_evidence_unchanged(
        store,
        task_id,
        attempt_id,
        attempts,
        RecoverySnapshot {
            plan,
            child: child_file,
            sent: &sent,
            ready: &ready,
            receipt_path: &receipt_path,
            tracked: &tracked,
            supervisor: &supervisor,
        },
    )
}

#[cfg(unix)]
fn recovery_evidence_unchanged(
    store: &StateStore,
    task_id: &str,
    attempt_id: &str,
    attempts: &Path,
    snapshot: RecoverySnapshot<'_>,
) -> Result<RecoveryInspection, SupervisorError> {
    let RecoverySnapshot {
        plan,
        child: child_file,
        sent,
        ready,
        receipt_path,
        tracked,
        supervisor,
    } = snapshot;
    let held = |reason| Ok(RecoveryInspection::Held(reason));
    let Some(current_plan) = private_bytes(&attempts.join(format!("{attempt_id}.plan.json")))
        .and_then(|bytes| serde_json::from_slice::<LaunchPlan>(&bytes).ok())
    else {
        return held("plan changed during recovery inspection");
    };
    let Some(current_child) = private_bytes(&attempts.join(format!("{attempt_id}.child.json")))
        .and_then(|bytes| serde_json::from_slice::<ChildIdentity>(&bytes).ok())
    else {
        return held("child identity changed during recovery inspection");
    };
    if super::binding::verify_observed_plan(&store.connection, store.root(), plan).is_err()
        || &current_plan != plan
        || &current_child != child_file
        || store
            .evidence_payload(task_id, Some(attempt_id), "gate_sent")?
            .as_deref()
            != Some(sent)
        || store
            .evidence_payload(task_id, Some(attempt_id), "supervisor_ready")?
            .as_deref()
            != Some(ready)
        || !matches!(
            fs::symlink_metadata(receipt_path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        )
        || store.latest_attempt(task_id)?.as_deref() != Some(attempt_id)
        || !store.active_attempt_reservation(task_id, attempt_id)?
    {
        return held("recovery evidence changed during inspection");
    }
    let current_tracked = store
        .evidence_payloads(task_id, attempt_id, "tracked_descendant")?
        .into_iter()
        .map(|payload| recorded_process(&payload))
        .collect::<Option<Vec<_>>>();
    let Some(current_tracked) = current_tracked else {
        return held("tracked descendant evidence changed during inspection");
    };
    if current_tracked != tracked
        || !registered_processes_absent(&current_child, supervisor, &current_tracked)
    {
        return held("registered process absence proof changed during inspection");
    }
    Ok(RecoveryInspection::Quiescent)
}

#[cfg(unix)]
struct RecoverySnapshot<'a> {
    plan: &'a LaunchPlan,
    child: &'a ChildIdentity,
    sent: &'a str,
    ready: &'a str,
    receipt_path: &'a Path,
    tracked: &'a [ProcessIdentity],
    supervisor: &'a ProcessIdentity,
}
