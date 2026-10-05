use super::{context::AmendedDispatchProof, proofs::parse_saved};
use crate::model::{
    ExitPrEvidence, ExitReceipt, LaunchPlan, PausePrStatus, StateError, VerifiedOpenPr,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

pub(crate) const KIND: &str = "amended_pr_completion";

/// Committed with the verified PR and task transition, never a launch permission.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    dispatch: String,
    gate_release: String,
    ready: (i64, String),
    child: (i64, String),
    gate_sent: (i64, String),
    exit: (i64, String),
    lookup: (i64, String),
    prior_lookups: Vec<(i64, String)>,
    verified_pr: (i64, String),
}

fn evidence(db: &Connection, plan: &LaunchPlan, kind: &str) -> Result<(i64, String), StateError> {
    let rows: Vec<(i64, String, Option<String>, String)> = db.prepare(
        "SELECT sequence,task_id,attempt_id,payload FROM evidence WHERE (task_id=?1 OR attempt_id=?2) AND kind=?3",
    )?.query_map(params![plan.task_id, plan.attempt_id, kind], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?.collect::<Result<_,_>>()?;
    match rows.as_slice() {
        [(sequence, task, attempt, payload)]
            if task == &plan.task_id && attempt.as_deref() == Some(&plan.attempt_id) =>
        {
            Ok((*sequence, payload.clone()))
        }
        _ => Err(StateError::LaunchBlocked),
    }
}

fn intent(db: &Connection, plan: &LaunchPlan, kind: &str) -> Result<String, StateError> {
    super::proofs::unique_payload(db, false, &plan.task_id, Some(&plan.attempt_id), kind)?
        .ok_or(StateError::LaunchBlocked)
}

fn completion(
    db: &Connection,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
) -> Result<Completion, StateError> {
    let (lookup, prior_lookups) = super::amended_completion_lookup::read(db, plan, receipt)?;
    let result = Completion {
        lookup,
        prior_lookups,
        dispatch: intent(db, plan, "supervisor_dispatch")?,
        gate_release: intent(db, plan, "gate_release")?,
        ready: evidence(db, plan, "supervisor_ready")?,
        child: evidence(db, plan, "child_registered")?,
        gate_sent: evidence(db, plan, "gate_sent")?,
        exit: evidence(db, plan, "attempt_exit")?,
        verified_pr: evidence(db, plan, "verified_open_pr")?,
    };
    validate_history(&result, plan, receipt)?;
    validate_pr(db, plan, &result)?;
    Ok(result)
}

fn validate_history(
    c: &Completion,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
) -> Result<(), StateError> {
    let dispatch: AmendedDispatchProof = parse_saved(&c.dispatch)?;
    let exit: ExitReceipt = parse_saved(&c.exit.1)?;
    let lookup: ExitPrEvidence = parse_saved(&c.lookup.1)?;
    if dispatch.effective_plan != *plan
        || exit != *receipt
        || receipt.attempt_id != plan.attempt_id
        || lookup.status != PausePrStatus::Open
        || lookup.observed_at_unix_secs == 0
        || c.prior_lookups
            .iter()
            .any(|(sequence, _)| *sequence <= c.exit.0 || *sequence >= c.lookup.0)
        || !(dispatch.amendment_sequence < c.child.0
            && c.child.0 < c.ready.0
            && c.ready.0 < c.gate_sent.0
            && c.gate_sent.0 < c.exit.0
            && c.exit.0 < c.lookup.0
            && c.lookup.0 < c.verified_pr.0)
    {
        return Err(StateError::LaunchBlocked);
    }
    validate_processes(c, receipt)
}

#[cfg(unix)]
fn validate_processes(c: &Completion, receipt: &ExitReceipt) -> Result<(), StateError> {
    let child: crate::model::ChildIdentity = parse_saved(&c.child.1)?;
    let ready = crate::model::recorded_process(&c.ready.1).ok_or(StateError::LaunchBlocked)?;
    if crate::model::recorded_process(&c.gate_release).as_ref() != Some(&ready)
        || crate::model::recorded_process(&c.gate_sent.1).as_ref() != Some(&ready)
        || child.pid == 0
        || child.pid != child.group_id
        || child.pid == ready.pid
        || child.pid != receipt.child_pid
        || child.boot_identity != receipt.boot_identity
        || child.start_identity != receipt.child_start_identity
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_processes(_: &Completion, _: &ExitReceipt) -> Result<(), StateError> {
    Err(StateError::LaunchBlocked)
}

fn validate_pr(db: &Connection, plan: &LaunchPlan, c: &Completion) -> Result<(), StateError> {
    let proof: VerifiedOpenPr = parse_saved(&c.verified_pr.1)?;
    let lookup: ExitPrEvidence = parse_saved(&c.lookup.1)?;
    let selection = super::proofs::unique_payload(db, true, &plan.task_id, None, "selection")?
        .ok_or(StateError::LaunchBlocked)?;
    let worktree =
        super::proofs::unique_payload(db, true, &plan.task_id, None, "worktree_created")?
            .ok_or(StateError::LaunchBlocked)?;
    let intent = super::proofs::unique_payload(db, false, &plan.task_id, None, "worktree_create")?
        .ok_or(StateError::LaunchBlocked)?;
    super::proofs::validate_pr_identity(
        &parse_saved(&selection)?,
        &parse_saved(&intent)?,
        &parse_saved(&worktree)?,
        &proof,
    )?;
    let count: usize = db.query_row("SELECT COUNT(*) FROM evidence WHERE kind='verified_open_pr' AND json_extract(payload,'$.id')=?1", [proof.id], |r| r.get(0))?;
    if count != 1
        || proof.id == 0
        || proof.number == 0
        || proof.head_repository_id == 0
        || proof.attempt_id != plan.attempt_id
        || proof.observed_at != lookup.observed_at_unix_secs
        || proof.repository != lookup.repository
        || proof.url.is_empty()
        || proof.created_at.is_empty()
        || proof.head_commit_sha.is_empty()
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

pub(crate) fn record(db: &Connection, task: &str, attempt: &str) -> Result<(), StateError> {
    let Some(audit) =
        super::proofs::unique_payload(db, true, task, Some(attempt), "initial_branch_removed")?
    else {
        return Ok(());
    };
    let audit: super::context::InitialBranchRemovalAudit = parse_saved(&audit)?;
    let plan = &audit.effective_plan;
    let receipt = parse_saved(&evidence(db, plan, "attempt_exit")?.1)?;
    let binding = completion(db, plan, &receipt)?;
    db.execute(
        "INSERT INTO intents(id,task_id,attempt_id,kind,detail) VALUES(?1,?2,?3,?4,?5)",
        params![
            format!("pr-completion-{attempt}"),
            task,
            attempt,
            KIND,
            serde_json::to_string(&binding)?
        ],
    )?;
    Ok(())
}

pub(crate) fn validate(
    db: &Connection,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
) -> Result<(), StateError> {
    let rows: Vec<(String, String, Option<String>, String)> = db.prepare(
        "SELECT id,task_id,attempt_id,detail FROM intents WHERE (task_id=?1 OR attempt_id=?2) AND kind=?3",
    )?.query_map(params![plan.task_id,plan.attempt_id,KIND], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?.collect::<Result<_,_>>()?;
    let [(id, task, attempt, payload)] = rows.as_slice() else {
        return Err(StateError::LaunchBlocked);
    };
    if id != &format!("pr-completion-{}", plan.attempt_id)
        || task != &plan.task_id
        || attempt.as_deref() != Some(&plan.attempt_id)
    {
        return Err(StateError::LaunchBlocked);
    }
    let saved: Completion = parse_saved(payload)?;
    if saved != completion(db, plan, receipt)? {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

pub(crate) fn require_absent(db: &Connection, plan: &LaunchPlan) -> Result<(), StateError> {
    let found: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM intents WHERE (task_id=?1 OR attempt_id=?2) AND kind=?3)
         OR EXISTS(SELECT 1 FROM evidence WHERE (task_id=?1 OR attempt_id=?2) AND kind='verified_open_pr')",
        params![plan.task_id,plan.attempt_id,KIND], |r| r.get(0),
    )?;
    if found {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
