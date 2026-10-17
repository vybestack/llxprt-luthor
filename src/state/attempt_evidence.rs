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
