use super::{
    context::{InitialBranchRemovalAudit, MAX_AUDIT_BYTES, NeverDispatchedContext, SavedRows},
    continuation::{self, ProofRow},
    proofs::{dispatch_evidence, dispatch_intent, parse_saved, validate_owner_protocol},
};
use crate::model::{ExitReceipt, LaunchPlan, StateError};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::path::Path;

pub(crate) fn read_context(
    db: &Connection,
    root: &Path,
    plan: &LaunchPlan,
) -> Result<NeverDispatchedContext, StateError> {
    let rows: Vec<(i64, String)> = db.prepare(
        "SELECT sequence,payload FROM evidence WHERE (task_id=?1 OR attempt_id=?2) AND kind='initial_branch_removed'",
    )?.query_map(params![plan.task_id, plan.attempt_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let [(sequence, payload)] = rows.as_slice() else {
        return Err(StateError::LaunchBlocked);
    };
    if payload.len() > MAX_AUDIT_BYTES {
        return Err(StateError::LaunchBlocked);
    }
    let audit: InitialBranchRemovalAudit = parse_saved(payload)?;
    validate_owner_protocol(db, &plan.task_id, &plan.attempt_id)?;
    validate_lifecycle(db, plan)?;
    let mut snapshot = continuation::saved_rows(db, &plan.task_id, &plan.attempt_id, true)?;
    normalize_rows(&mut snapshot)?;
    strip_annotations(&mut snapshot, *sequence, plan)?;
    let intents = retained_proofs(&snapshot, false)?;
    let evidence = retained_proofs(&snapshot, true)?;
    if snapshot != audit.original_rows {
        return Err(StateError::LaunchBlocked);
    }
    continuation::assemble_context(
        db,
        root,
        &plan.task_id,
        &plan.attempt_id,
        snapshot,
        &intents,
        &evidence,
    )
}

fn validate_lifecycle(db: &Connection, plan: &LaunchPlan) -> Result<(), StateError> {
    let (phase, lifecycle, outcome, reservation): (String, String, Option<String>, String) = db.query_row(
        "SELECT t.state,a.lifecycle,a.outcome,r.status FROM tasks t
         JOIN attempts a ON a.task_id=t.id JOIN reservations r ON r.task_id=t.id AND r.attempt_id=a.id
         WHERE t.id=?1 AND a.id=?2 AND (SELECT COUNT(*) FROM attempts WHERE task_id=?1)=1
         AND (SELECT COUNT(*) FROM reservations WHERE task_id=?1 OR attempt_id=?2)=1",
        params![plan.task_id, plan.attempt_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let exits: Vec<(String, Option<String>, String)> = db.prepare(
        "SELECT task_id,attempt_id,payload FROM evidence WHERE (task_id=?1 OR attempt_id=?2) AND kind='attempt_exit'",
    )?.query_map(params![plan.task_id, plan.attempt_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    if phase != "pr_complete" {
        super::amended_completion::require_absent(db, plan)?;
    }
    match (
        phase.as_str(),
        lifecycle.as_str(),
        outcome.as_deref(),
        reservation.as_str(),
        exits.as_slice(),
    ) {
        ("held", "launch_intended", None, "reserved", []) => Ok(()),
        (
            "held" | "attention" | "paused" | "pr_complete",
            "completed",
            Some(outcome),
            "released",
            [(task, attempt, payload)],
        ) => {
            let receipt: ExitReceipt = parse_saved(payload)?;
            if task != &plan.task_id
                || attempt.as_deref() != Some(&plan.attempt_id)
                || receipt.attempt_id != plan.attempt_id
                || receipt.exit_code.is_some() == receipt.signal.is_some()
                || outcome
                    != format!(
                        "exit_code={:?};signal={:?}",
                        receipt.exit_code, receipt.signal
                    )
            {
                return Err(StateError::LaunchBlocked);
            }
            validate_terminal_phase(db, plan, &phase, &receipt)
        }
        _ => Err(StateError::LaunchBlocked),
    }
}

fn validate_terminal_phase(
    db: &Connection,
    plan: &LaunchPlan,
    phase: &str,
    receipt: &ExitReceipt,
) -> Result<(), StateError> {
    let kind = match phase {
        "pr_complete" => return super::amended_completion::validate(db, plan, receipt),
        "held" => return Ok(()),
        "attention" if receipt.stop_signals.is_empty() => "exit_pr_lookup",
        "paused" if !receipt.stop_signals.is_empty() => "pause_pr_lookup",
        _ => return Err(StateError::LaunchBlocked),
    };
    let lookup: String = db.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind=?3 ORDER BY sequence DESC LIMIT 1",
        params![plan.task_id, plan.attempt_id, kind], |row| row.get(0),
    )?;
    let lookup: crate::model::ExitPrEvidence = parse_saved(&lookup)?;
    let selection: String = db.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND kind='selection' AND attempt_id IS NULL",
        [&plan.task_id],
        |row| row.get(0),
    )?;
    let selection: crate::model::SelectionEvidence = parse_saved(&selection)?;
    if lookup.status != crate::model::PausePrStatus::Absent
        || lookup.observed_at_unix_secs == 0
        || lookup.repository != selection.candidate.mapping.code_repository
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

fn normalize_rows(snapshot: &mut SavedRows) -> Result<(), StateError> {
    for (table, replacements) in [
        (0, vec![(6, Value::from("held"))]),
        (
            1,
            vec![(3, Value::from("launch_intended")), (4, Value::Null)],
        ),
        (2, vec![(3, Value::from("reserved"))]),
    ] {
        let [row] = snapshot.0[table].as_mut_slice() else {
            return Err(StateError::LaunchBlocked);
        };
        let mut values: Vec<Value> = serde_json::from_str(row)?;
        for (index, value) in replacements {
            *values.get_mut(index).ok_or(StateError::LaunchBlocked)? = value;
        }
        *row = serde_json::to_string(&values)?;
    }
    Ok(())
}

fn annotation(kind: &str, evidence: bool) -> bool {
    if evidence {
        dispatch_evidence(kind)
            || matches!(
                kind,
                "verified_open_pr"
                    | "attempt_exit"
                    | "exit_pr_lookup"
                    | "pause_pr_lookup"
                    | "attention_reason"
                    | "held_reason"
                    | "log_failure"
                    | "independent_stop_decision"
                    | "independent_stop_signal"
                    | "independent_group_absent"
            )
    } else {
        dispatch_intent(kind) || kind == "stop" || kind == super::amended_completion::KIND
    }
}

fn strip_annotations(
    snapshot: &mut SavedRows,
    sequence: i64,
    plan: &LaunchPlan,
) -> Result<(), StateError> {
    for table in [3, 4] {
        let evidence = table == 4;
        let mut retained = Vec::new();
        for row in &snapshot.0[table] {
            let values: Vec<Value> = serde_json::from_str(row)?;
            let scope = if evidence { 1 } else { 2 };
            let kind = values
                .get(scope + 2)
                .and_then(Value::as_str)
                .ok_or(StateError::LaunchBlocked)?;
            let post_audit = evidence && values[0].as_i64().is_some_and(|s| s > sequence);
            let removable = annotation(kind, evidence) && (!evidence || post_audit);
            if removable {
                if values[scope].as_str() != Some(&plan.task_id)
                    || (values[scope + 1].as_str() != Some(&plan.attempt_id)
                        && !(kind == "held_reason" && values[scope + 1].is_null()))
                {
                    return Err(StateError::LaunchBlocked);
                }
            } else {
                retained.push(row.clone());
            }
        }
        snapshot.0[table] = retained;
    }
    Ok(())
}

fn retained_proofs(snapshot: &SavedRows, evidence: bool) -> Result<Vec<ProofRow>, StateError> {
    let mut rows = Vec::new();
    for row in &snapshot.0[if evidence { 4 } else { 3 }] {
        let values: Vec<Value> = serde_json::from_str(row)?;
        let start = if evidence { 1 } else { 2 };
        let string = |index: usize| {
            values[index]
                .as_str()
                .map(str::to_owned)
                .ok_or(StateError::LaunchBlocked)
        };
        rows.push((
            string(start)?,
            values[start + 1].as_str().map(str::to_owned),
            string(start + 2)?,
            string(start + 3)?,
        ));
    }
    Ok(rows)
}
