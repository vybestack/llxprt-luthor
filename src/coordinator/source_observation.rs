use crate::state::{journal, task_records, worktree_records};
use crate::{
    config::Marker,
    eligibility::Candidate,
    github::project::{Issue, ProjectItem, ProjectReader, enumerate_target},
    state::{SelectionEvidence, StateError, StateStore},
    worktree::{self, WorktreeInspection},
};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceReconciliation {
    pub task_id: String,
    pub status: &'static str,
    pub reasons: Vec<&'static str>,
    pub issue_state: Option<String>,
    pub assignees: Option<Vec<String>>,
    pub marker_present: Option<bool>,
    pub project_membership: Option<bool>,
    pub worktree: Option<WorktreeInspection>,
}

/// Observe unfinished source operations without verifying an intent, assigning,
/// adopting a worktree, or releasing the task for scheduling.
pub fn reconcile_source<P: ProjectReader>(
    store: &mut StateStore,
    task_id: &str,
    projects: &mut P,
) -> Result<SourceReconciliation, StateError> {
    if task_records::task_phase(store, task_id)?.is_none()
        || task_records::latest_attempt(store, task_id)?.is_some()
    {
        return Err(StateError::InvalidSelection);
    }
    let mut report = SourceReconciliation {
        task_id: task_id.to_owned(),
        status: "held",
        reasons: vec![],
        issue_state: None,
        assignees: None,
        marker_present: None,
        project_membership: None,
        worktree: None,
    };
    if let Some(selection) = task_records::selection_evidence(store, task_id)? {
        observe_project(&selection, projects, &mut report);
        observe_claim_intent(store, &selection, &mut report)?;
        observe_worktree(store, &selection, &mut report)?;
    } else {
        report.reasons.push("selection_missing");
    }
    if report.reasons.is_empty() {
        report.reasons.push("prelaunch_not_verified");
    }
    journal::record_evidence(
        store,
        task_id,
        None,
        "source_observation",
        &serde_json::to_string(&report)?,
    )?;
    Ok(report)
}

fn observe_project<P: ProjectReader>(
    selection: &SelectionEvidence,
    projects: &mut P,
    report: &mut SourceReconciliation,
) {
    let candidate = &selection.candidate;
    let items = match enumerate_target(
        projects,
        &candidate.project_id,
        &candidate.repository,
        candidate.issue_number,
    ) {
        Ok(items) => items,
        Err(_) => {
            report.reasons.push("source_read_failed");
            return;
        }
    };
    let matching = items
        .iter()
        .filter(|(item, _)| {
            item.item_id == candidate.item_id
                && item.issue_node_id == candidate.issue_node_id
                && item.tracker_repo_id == candidate.tracker_repo_id
        })
        .collect::<Vec<_>>();
    report.project_membership = Some(matching.len() == 1 && items.len() == 1);
    if let [(item, issue)] = matching.as_slice() {
        observe_issue(selection, item, issue, report);
    } else {
        report.reasons.push("project_membership_mismatch");
    }
    if report.project_membership == Some(false)
        && !report.reasons.contains(&"project_membership_mismatch")
    {
        report.reasons.push("project_membership_mismatch");
    }
}

fn observe_issue(
    selection: &SelectionEvidence,
    item: &ProjectItem,
    issue: &Issue,
    report: &mut SourceReconciliation,
) {
    let candidate = &selection.candidate;
    report.issue_state = Some(issue.state.clone());
    report.assignees = Some(issue.assignees.clone());
    let marker = match &candidate.marker {
        Marker::Label { name } => issue.labels.contains(name),
        Marker::ProjectField { name, value } => {
            !item.unsupported_fields.contains(name)
                && item.fields.contains(&(name.clone(), value.clone()))
        }
    };
    report.marker_present = Some(marker);
    if !issue_identity_matches(candidate, issue) {
        report.reasons.push("issue_identity_mismatch");
    }
    if issue.state != "open" {
        report.reasons.push("issue_not_open");
    }
    if !marker {
        report.reasons.push("marker_missing");
    }
    if issue.assignees != [selection.effective_config.assignment_login.as_str()]
        && !issue.assignees.is_empty()
    {
        report.reasons.push("assignees_unexpected");
    }
}

fn issue_identity_matches(candidate: &Candidate, issue: &Issue) -> bool {
    issue.node_id == candidate.issue_node_id
        && issue.repository == candidate.repository
        && issue.tracker_repo_id == candidate.tracker_repo_id
        && issue.url == candidate.issue_url
        && issue.milestone == candidate.milestone_title
        && issue.milestone_id == candidate.milestone_id
        && !candidate
            .source
            .milestone
            .as_ref()
            .is_some_and(|title| issue.milestone.as_ref() != Some(title))
}

fn observe_claim_intent(
    store: &StateStore,
    selection: &SelectionEvidence,
    report: &mut SourceReconciliation,
) -> Result<(), StateError> {
    if let Some(detail) = task_records::source_claim_intent(store, &report.task_id)? {
        let candidate = &selection.candidate;
        let expected = serde_json::json!({"principal": selection.effective_config.assignment_login,
            "repository": candidate.repository, "number": candidate.issue_number});
        if serde_json::from_str::<serde_json::Value>(&detail).ok() != Some(expected) {
            report.reasons.push("claim_intent_mismatch");
        }
        if report.assignees.as_deref() == Some(&[][..]) {
            report.reasons.push("assignment_not_observed");
        }
        report.reasons.push("claim_intent_unverified");
    }
    Ok(())
}

fn observe_worktree(
    store: &StateStore,
    selection: &SelectionEvidence,
    report: &mut SourceReconciliation,
) -> Result<(), StateError> {
    let Some(record) = worktree_records::worktree_record(store, &report.task_id)? else {
        return Ok(());
    };
    let root = &selection.effective_config.worktree_root;
    let expected_path = std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.clone())
        .join(&report.task_id);
    if record.intent.path != expected_path {
        report.reasons.push("worktree_intent_mismatch");
        return Ok(());
    }
    match worktree::inspect_record(&record, &selection.candidate.mapping) {
        Ok(inspection) => {
            if inspection != WorktreeInspection::IdentityMatches {
                report.reasons.push("worktree_unverified");
            }
            report.worktree = Some(inspection);
        }
        Err(_) => report.reasons.push("worktree_read_failed"),
    }
    Ok(())
}
