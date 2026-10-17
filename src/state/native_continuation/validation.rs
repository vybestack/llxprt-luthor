use super::records::{launch, payload};
use crate::native_transition::Transition;
use crate::{
    model::{ExitReceipt, LaunchPlan, PausePrStatus, SelectionEvidence, StateError},
    state::{NeverDispatchedContext, proofs::parse_saved},
};
use rusqlite::{Connection, params};

pub(crate) fn selection(
    initial: &NeverDispatchedContext,
    t: &Transition,
) -> Result<SelectionEvidence, StateError> {
    let mut selection = initial.selection().clone();
    let audit = initial.amendment().ok_or(StateError::LaunchBlocked)?;
    if audit.future_template_correction.is_some()
        && (audit.current_config != t.config
            || audit.current_config_revision != t.plan.config_revision)
    {
        return Err(StateError::LaunchBlocked);
    }
    if crate::state::amendment::policy::correction_for_config(initial, &t.config)?.is_none()
        || t.actor != selection.candidate.mapping.allowed_pr_author
        || t.observed_at_unix_secs == 0
        || t.plan.config_revision.trim().is_empty()
        || t.plan.config_revision.len() > 128
        || t.plan.config_revision.contains(['\0', '\n', '\r'])
    {
        return Err(StateError::LaunchBlocked);
    }
    selection.effective_config = t.config.clone();
    selection.config_revision = t.plan.config_revision.clone();
    Ok(selection)
}

pub(crate) fn validate(
    db: &Connection,
    initial: &NeverDispatchedContext,
    t: &Transition,
) -> Result<(), StateError> {
    let selection = selection(initial, t)?;
    let previous = &t.predecessor.effective_plan;
    let expected = crate::continuation_plan::render(
        &selection,
        initial.effective_plan(),
        previous,
        &t.plan.attempt_id,
        t.mode.stopped(),
    )?;
    if t.plan != expected
        || t.plan.task_id != initial.task_id()
        || previous.task_id != initial.task_id()
        || t.plan.session_id != initial.task_id()
        || previous.attempt_id == t.plan.attempt_id
        || !crate::cli_identifiers::valid_attempt(&t.plan.attempt_id)
        || t.pr.status != PausePrStatus::Absent
        || t.pr.repository != selection.candidate.mapping.code_repository
        || t.pr.observed_at_unix_secs == 0
        || t.pr.observed_at_unix_secs > t.observed_at_unix_secs
        || t.source.observed_at_unix_secs == 0
        || t.source.observed_at_unix_secs > t.observed_at_unix_secs
    {
        return Err(StateError::LaunchBlocked);
    }
    let audit = crate::model::RetryAuthorization {
        actor: t.actor.clone(),
        reason: "native continuation".into(),
        previous_plan: previous.clone(),
        previous_config: t.config.clone(),
        config: t.config.clone(),
        plan: t.plan.clone(),
        reservation: t.plan.attempt_id.clone(),
        pr: t.pr.clone(),
        source: t.source.clone(),
        terminal_exit: None,
    };
    if !crate::state::attempt_evidence::retry_evidence_matches(db, &audit, &selection)? {
        return Err(StateError::LaunchBlocked);
    }
    terminal(db, t)?;
    if t.predecessor.transition_sequence.is_some() {
        dispatch(
            db,
            previous,
            t.predecessor
                .transition_sequence
                .ok_or(StateError::LaunchBlocked)?,
        )?;
    }
    Ok(())
}

fn terminal(db: &Connection, t: &Transition) -> Result<(), StateError> {
    let p = &t.predecessor.effective_plan;
    let receipt: ExitReceipt = parse_saved(&payload(
        db,
        true,
        &p.task_id,
        &p.attempt_id,
        "attempt_exit",
    )?)?;
    let valid: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM attempts a JOIN reservations r ON r.task_id=a.task_id AND r.attempt_id=a.id WHERE a.task_id=?1 AND a.id=?2 AND a.lifecycle='completed' AND a.outcome=?3 AND r.status='released')",
        params![p.task_id,p.attempt_id,format!("exit_code={:?};signal={:?}",receipt.exit_code,receipt.signal)], |r| r.get(0))?;
    if !valid
        || receipt.attempt_id != p.attempt_id
        || receipt.exit_code.is_some() == receipt.signal.is_some()
        || receipt.stop_signals.is_empty() == t.mode.stopped()
        || (!t.mode.stopped() && receipt.exit_code.is_none())
    {
        return Err(StateError::LaunchBlocked);
    }
    let stops: usize = db.query_row(
        "SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop'",
        params![p.task_id, p.attempt_id],
        |r| r.get(0),
    )?;
    if stops != usize::from(t.mode.stopped()) {
        return Err(StateError::LaunchBlocked);
    }
    let kind = if t.mode.stopped() {
        "pause_pr_lookup"
    } else {
        "exit_pr_lookup"
    };
    let lookup: String = db.query_row("SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind=?3 ORDER BY sequence DESC LIMIT 1", params![p.task_id,p.attempt_id,kind], |r|r.get(0))?;
    let lookup: crate::model::ExitPrEvidence = parse_saved(&lookup)?;
    if lookup.status != PausePrStatus::Absent
        || lookup.observed_at_unix_secs == 0
        || lookup.repository != t.pr.repository
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

pub(crate) fn dispatch(
    db: &Connection,
    plan: &LaunchPlan,
    sequence: i64,
) -> Result<(), StateError> {
    let proof: crate::state::context::AmendedDispatchProof = parse_saved(&payload(
        db,
        false,
        &plan.task_id,
        &plan.attempt_id,
        "supervisor_dispatch",
    )?)?;
    if proof.amendment_sequence != sequence || proof.effective_plan != *plan {
        return Err(StateError::LaunchBlocked);
    }
    crate::state::proofs::validate_owner_protocol(db, &plan.task_id, &plan.attempt_id)?;
    crate::state::dispatch_stages::verify(db, &plan.task_id, &plan.attempt_id)?;
    let ordered: bool = db.query_row("SELECT NOT EXISTS(SELECT 1 FROM evidence WHERE attempt_id=?1 AND kind IN ('child_registered','supervisor_ready','gate_sent','attempt_exit') AND sequence<=?2)", params![plan.attempt_id,sequence], |r|r.get(0))?;
    if !ordered {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}
pub(crate) fn active(db: &Connection, plan: &LaunchPlan) -> Result<(), StateError> {
    let valid: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM tasks t JOIN attempts a ON a.task_id=t.id JOIN reservations r ON r.task_id=t.id AND r.attempt_id=a.id WHERE t.id=?1 AND a.id=?2 AND t.state='held' AND a.lifecycle='launch_intended' AND a.outcome IS NULL AND r.status='reserved' AND a.id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1) AND (SELECT COUNT(*) FROM reservations WHERE task_id=?1 AND status='reserved')=1)", params![plan.task_id,plan.attempt_id], |r|r.get(0))?;
    if !valid || launch(db, &plan.task_id, &plan.attempt_id)? != *plan {
        return Err(StateError::LaunchBlocked);
    }
    Ok(())
}

pub(crate) fn observed(db: &Connection, plan: &LaunchPlan) -> Result<(), StateError> {
    let (phase,lifecycle,outcome,reservation,latest): (String,String,Option<String>,String,bool) = db.query_row("SELECT t.state,a.lifecycle,a.outcome,r.status,a.id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1) FROM tasks t JOIN attempts a ON a.task_id=t.id JOIN reservations r ON r.task_id=t.id AND r.attempt_id=a.id WHERE t.id=?1 AND a.id=?2", params![plan.task_id,plan.attempt_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    if lifecycle == "launch_intended"
        && outcome.is_none()
        && reservation == "reserved"
        && latest
        && phase == "held"
    {
        return crate::state::amended_completion::require_absent(db, plan);
    }
    if lifecycle != "completed" || reservation != "released" {
        return Err(StateError::LaunchBlocked);
    }
    let receipt: ExitReceipt = parse_saved(&payload(
        db,
        true,
        &plan.task_id,
        &plan.attempt_id,
        "attempt_exit",
    )?)?;
    if receipt.attempt_id != plan.attempt_id
        || receipt.exit_code.is_some() == receipt.signal.is_some()
        || outcome
            != Some(format!(
                "exit_code={:?};signal={:?}",
                receipt.exit_code, receipt.signal
            ))
    {
        return Err(StateError::LaunchBlocked);
    }
    if latest {
        crate::state::amended_observation::validate_terminal_phase(db, plan, &phase, &receipt)
    } else {
        crate::state::amended_completion::require_absent(db, plan)
    }
}
