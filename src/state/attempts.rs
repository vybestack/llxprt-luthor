use crate::model::*;
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) fn retry_evidence_matches(
    connection: &Connection,
    audit: &RetryAuthorization,
    selection: &SelectionEvidence,
) -> Result<bool, StateError> {
    let c = &selection.candidate;
    let source = &audit.source;
    let claim: Option<String> = connection
        .query_row(
            "SELECT detail FROM intents WHERE task_id=?1 AND kind='claim_assignment'",
            [&audit.plan.task_id],
            |row| row.get(0),
        )
        .optional()?;
    let expected = serde_json::json!({"principal":audit.config.assignment_login,"repository":c.repository,"number":c.issue_number});
    if source.observed_at_unix_secs == 0
        || source.claim != claim.unwrap_or_default()
        || serde_json::from_str::<serde_json::Value>(&source.claim).ok() != Some(expected)
        || source.item.item_id != c.item_id
        || source.item.issue_node_id != c.issue_node_id
        || source.item.repository != c.repository
        || source.item.tracker_repo_id != c.tracker_repo_id
        || source.item.issue_number != c.issue_number
        || source.issue.node_id != c.issue_node_id
        || source.issue.repository != c.repository
        || source.issue.tracker_repo_id != c.tracker_repo_id
        || source.issue.number != c.issue_number
        || source.issue.url != c.issue_url
        || source.issue.state != "open"
        || source.issue.assignees != [audit.config.assignment_login.as_str()]
    {
        return Ok(false);
    }
    terminal_evidence_matches(connection, audit)
}

pub(crate) fn same_task_config(
    original: &EffectiveConfigSnapshot,
    current: &EffectiveConfigSnapshot,
) -> bool {
    let mut comparable = current.clone();
    comparable.initial = original.initial.clone();
    comparable.resume = original.resume.clone();
    comparable == *original
}

/// Resolve a launch's revision through its exact per-attempt authorization,
/// never by accepting an arbitrary revision supplied by a plan file.
pub(crate) fn attempt_selection(
    connection: &Connection,
    plan: &crate::model::LaunchPlan,
) -> Result<SelectionEvidence, StateError> {
    let selection: String = connection.query_row(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id IS NULL AND kind='selection'",
        [&plan.task_id],
        |row| row.get(0),
    )?;
    let mut selection: SelectionEvidence = serde_json::from_str(&selection)?;
    let candidate = &selection.candidate;
    let consistent: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND tracker_repo_id=?2
         AND issue_node_id=?3 AND repository=?4 AND issue_number=?5 AND config_revision=?6)",
        params![
            plan.task_id,
            candidate.tracker_repo_id,
            candidate.issue_node_id,
            candidate.repository,
            candidate.issue_number,
            selection.config_revision
        ],
        |row| row.get(0),
    )?;
    if !consistent {
        return Err(StateError::LaunchBlocked);
    }
    let audits: Vec<String> = connection.prepare(
        "SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='retry_authorized'",
    )?.query_map(params![plan.task_id, plan.attempt_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    match audits.as_slice() {
        [] if selection.config_revision == plan.config_revision => {
            unaudited_selection(connection, plan, selection)
        }
        [payload] => {
            let audit: RetryAuthorization = serde_json::from_str(payload)?;
            let prior: String = connection.query_row(
                "SELECT i.detail FROM intents i JOIN attempts a ON a.id=i.attempt_id
                 WHERE i.task_id=?1 AND i.attempt_id=?2 AND i.kind='launch'
                 AND a.rowid < (SELECT rowid FROM attempts WHERE id=?3 AND task_id=?1)",
                params![
                    plan.task_id,
                    audit.previous_plan.attempt_id,
                    plan.attempt_id
                ],
                |row| row.get(0),
            )?;
            let prior: crate::model::LaunchPlan = serde_json::from_str(&prior)?;
            if audit.plan != *plan
                || !retry_evidence_matches(connection, &audit, &selection)?
                || audit.previous_plan != prior
                || audit.previous_plan.task_id != plan.task_id
                || !retry_audit_identity_matches(&audit, &selection, plan)
                || attempt_selection(connection, &prior)?.effective_config != audit.previous_config
            {
                return Err(StateError::LaunchBlocked);
            }
            selection.config_revision = plan.config_revision.clone();
            selection.effective_config = audit.config;
            Ok(selection)
        }
        _ => Err(StateError::LaunchBlocked),
    }
}

fn unaudited_selection(
    connection: &Connection,
    plan: &LaunchPlan,
    selection: SelectionEvidence,
) -> Result<SelectionEvidence, StateError> {
    let original: Option<String> = connection
        .query_row(
            "SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid LIMIT 1",
            [&plan.task_id],
            |row| row.get(0),
        )
        .optional()?;
    if original.as_deref() == Some(&plan.attempt_id) {
        return Ok(selection);
    }
    // Ordinary resumes have stopped-exit authorization, not retry authorization.
    // A natural-exit retry cannot acquire that identity by reusing a revision.
    stopped_resume_selection(connection, plan, selection)
}

fn stopped_resume_selection(
    connection: &Connection,
    plan: &LaunchPlan,
    selection: SelectionEvidence,
) -> Result<SelectionEvidence, StateError> {
    let previous: Option<(String, String, String, String)> = connection
        .query_row(
            "SELECT i.detail,e.payload,p.payload,a.outcome FROM attempts a
         JOIN reservations r ON r.attempt_id=a.id AND r.task_id=a.task_id
         JOIN intents i ON i.attempt_id=a.id AND i.task_id=a.task_id AND i.kind='launch'
         JOIN evidence e ON e.attempt_id=a.id AND e.task_id=a.task_id AND e.kind='attempt_exit'
         JOIN evidence p ON p.attempt_id=a.id AND p.task_id=a.task_id AND p.kind='pause_pr_lookup'
         WHERE a.task_id=?1 AND a.lifecycle='completed' AND a.outcome IS NOT NULL
           AND r.status='released'
           AND a.rowid=(SELECT MAX(rowid) FROM attempts WHERE task_id=?1
               AND rowid < (SELECT rowid FROM attempts WHERE task_id=?1 AND id=?2))
           AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND attempt_id=a.id AND kind='stop')=1
           AND (SELECT COUNT(*) FROM intents WHERE attempt_id=a.id AND kind='launch')=1
           AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=a.id AND kind='attempt_exit')=1
           AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=a.id AND kind='pause_pr_lookup')=1",
            params![plan.task_id, plan.attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let (previous, receipt, pause, outcome) = previous.ok_or(StateError::LaunchBlocked)?;
    let previous: LaunchPlan = serde_json::from_str(&previous)?;
    let receipt: ExitReceipt = serde_json::from_str(&receipt)?;
    let pause: PausePrEvidence = serde_json::from_str(&pause)?;
    if previous.task_id != plan.task_id
        || previous.config_revision != selection.config_revision
        || receipt.attempt_id != previous.attempt_id
        || receipt.stop_signals.is_empty()
        || outcome
            != format!(
                "exit_code={:?};signal={:?}",
                receipt.exit_code, receipt.signal
            )
        || pause.status != PausePrStatus::Absent
        || pause.observed_at_unix_secs == 0
        || pause.repository != selection.candidate.mapping.code_repository
        || attempt_selection(connection, &previous)?.effective_config != selection.effective_config
    {
        return Err(StateError::LaunchBlocked);
    }
    Ok(selection)
}

pub(crate) fn retry_context(
    connection: &Connection,
    task_id: &str,
    previous_attempt_id: &str,
) -> Result<(crate::model::LaunchPlan, crate::model::ExitReceipt), StateError> {
    super::continuation_hold::require_unamended_task(connection, task_id)?;
    let context: Option<(String, String, String)> = connection.query_row(
        "SELECT i.detail,e.payload,a.outcome FROM attempts a
         JOIN tasks t ON t.id=a.task_id
         JOIN reservations r ON r.attempt_id=a.id AND r.task_id=t.id
         JOIN intents i ON i.task_id=t.id AND i.attempt_id=a.id AND i.kind='launch'
         JOIN evidence e ON e.task_id=t.id AND e.attempt_id=a.id AND e.kind='attempt_exit'
         WHERE t.id=?1 AND a.id=?2 AND t.state='attention'
         AND a.lifecycle='completed' AND a.outcome IS NOT NULL AND r.status='released'
         AND a.id=(SELECT id FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1)
         AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?2 AND kind='launch')=1
         AND (SELECT COUNT(*) FROM evidence WHERE attempt_id=?2 AND kind='attempt_exit')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='claim_verified')=1
         AND (SELECT COUNT(*) FROM intents WHERE task_id=?1 AND kind='claim_assignment')=1
         AND (SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND kind='worktree_created')=1
         AND NOT EXISTS(SELECT 1 FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='stop')
         AND NOT EXISTS(SELECT 1 FROM reservations WHERE task_id=?1 AND status='reserved')
         AND NOT EXISTS(SELECT 1 FROM attempts WHERE task_id=?1 AND (lifecycle!='completed' OR outcome IS NULL))
         AND EXISTS(SELECT 1 FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='exit_pr_lookup'
             AND json_extract(payload,'$.status.status')='absent')",
        params![task_id, previous_attempt_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    let (plan, receipt, outcome) = context.ok_or(StateError::LaunchBlocked)?;
    let plan: crate::model::LaunchPlan = serde_json::from_str(&plan)?;
    let receipt: crate::model::ExitReceipt = serde_json::from_str(&receipt)?;
    if plan.task_id != task_id
        || plan.attempt_id != previous_attempt_id
        || plan.session_id != task_id
        || receipt.attempt_id != previous_attempt_id
        || receipt.exit_code.is_none()
        || receipt.signal.is_some()
        || !receipt.stop_signals.is_empty()
        || outcome
            != format!(
                "exit_code={:?};signal={:?}",
                receipt.exit_code, receipt.signal
            )
    {
        return Err(StateError::LaunchBlocked);
    }
    attempt_selection(connection, &plan)?;
    Ok((plan, receipt))
}

fn terminal_evidence_matches(
    connection: &Connection,
    audit: &RetryAuthorization,
) -> Result<bool, StateError> {
    if let Some(proof) = &audit.terminal_exit {
        let previous = &audit.previous_plan;
        let receipt: String = connection.query_row("SELECT payload FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='attempt_exit'", params![previous.task_id, previous.attempt_id], |row| row.get(0))?;
        if serde_json::from_str::<crate::model::ExitReceipt>(&receipt)? != proof.receipt
            || proof.receipt.attempt_id != previous.attempt_id
            || proof.observed_at_unix_secs == 0
            || !proof.matches_startup_rejection(previous)
        {
            return Ok(false);
        }
        for (kind, payload) in [
            ("child_registered", &proof.child_registration),
            ("supervisor_ready", &proof.supervisor_registration),
            ("gate_sent", &proof.gate_sent),
        ] {
            let matches: bool = connection.query_row("SELECT COUNT(*)=1 AND MIN(payload)=?4 FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind=?3", params![previous.task_id, previous.attempt_id, kind, payload], |row| row.get(0))?;
            if !matches {
                return Ok(false);
            }
        }
        let release: Option<String> = connection.query_row("SELECT detail FROM intents WHERE task_id=?1 AND attempt_id=?2 AND kind='gate_release'", params![previous.task_id, previous.attempt_id], |row| row.get(0)).optional()?;
        let tracked: usize = connection.query_row("SELECT COUNT(*) FROM evidence WHERE task_id=?1 AND attempt_id=?2 AND kind='tracked_descendant'", params![previous.task_id, previous.attempt_id], |row| row.get(0))?;
        if release.as_ref() != Some(&proof.gate_release) || tracked != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn retry_audit_identity_matches(
    audit: &RetryAuthorization,
    selection: &SelectionEvidence,
    plan: &LaunchPlan,
) -> bool {
    !(audit.actor != selection.candidate.mapping.allowed_pr_author
        || audit.reason.trim().is_empty()
        || audit.reservation != plan.attempt_id
        || audit.pr.status != PausePrStatus::Absent
        || audit.pr.observed_at_unix_secs == 0
        || audit.pr.repository != selection.candidate.mapping.code_repository
        || !same_task_config(&selection.effective_config, &audit.config)
        || plan.config_revision.trim().is_empty())
}
