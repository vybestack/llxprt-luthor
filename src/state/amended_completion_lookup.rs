use crate::model::{ExitPrEvidence, ExitReceipt, LaunchPlan, PausePrStatus, StateError};
use rusqlite::{Connection, params};

type Row = (i64, String);

/// Prior unavailable/ambiguous/open reads can be retried while held. Bind them
/// all so terminal observation cannot silently drop or add a lookup later.
pub(crate) fn read(
    db: &Connection,
    plan: &LaunchPlan,
    receipt: &ExitReceipt,
) -> Result<(Row, Vec<Row>), StateError> {
    let kind = if receipt.stop_signals.is_empty() {
        "exit_pr_lookup"
    } else {
        "pause_pr_lookup"
    };
    let rows: Vec<(i64, String, Option<String>, String, String)> = db.prepare(
        "SELECT sequence,task_id,attempt_id,kind,payload FROM evidence
         WHERE (task_id=?1 OR attempt_id=?2) AND kind IN ('exit_pr_lookup','pause_pr_lookup') ORDER BY sequence",
    )?.query_map(params![plan.task_id,plan.attempt_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?.collect::<Result<_,_>>()?;
    let (last, prior) = rows.split_last().ok_or(StateError::LaunchBlocked)?;
    let current: ExitPrEvidence = super::proofs::parse_saved(&last.4)?;
    let mut retained = Vec::new();
    for (sequence, task, attempt, saved_kind, payload) in &rows {
        let lookup: ExitPrEvidence = super::proofs::parse_saved(payload)?;
        if task != &plan.task_id
            || attempt.as_deref() != Some(&plan.attempt_id)
            || saved_kind != kind
            || lookup.observed_at_unix_secs == 0
            || lookup.observed_at_unix_secs > current.observed_at_unix_secs
            || lookup.repository != current.repository
            || lookup.status == PausePrStatus::Absent
        {
            return Err(StateError::LaunchBlocked);
        }
        retained.push((*sequence, payload.clone()));
    }
    retained.truncate(prior.len());
    Ok(((last.0, last.4.clone()), retained))
}
