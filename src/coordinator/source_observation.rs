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

/// Reconcile source operations from durable intent and fresh observable proof.
/// Never reassign an issue or launch a worker; uncertain tasks retain their slot.
pub fn reconcile_source<P: ProjectReader>(
    store: &mut StateStore,
    task_id: &str,
    projects: &mut P,
) -> Result<SourceReconciliation, StateError> {
    if !matches!(
        task_records::task_phase(store, task_id)?.as_deref(),
        Some("preparing" | "selected" | "claimed" | "held")
    ) || task_records::latest_attempt(store, task_id)?.is_some()
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
        if report.reasons.is_empty() {
            recover_worktree(store, &selection, &mut report)?;
        } else {
            observe_worktree(store, &selection, &mut report)?;
        }
    } else {
        report.reasons.push("selection_missing");
    }
    if report.reasons.is_empty() {
        report.status = "verified";
    } else {
        task_records::hold_task(store, task_id, "source reconciliation held")?;
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
    store: &mut StateStore,
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
        let verified = task_records::source_claim_verified(store, &report.task_id)?;
        let login = &selection.effective_config.assignment_login;
        if verified.as_ref().is_some_and(|payload| payload != login) {
            report.reasons.push("claim_intent_mismatch");
        }
        let assigned = report.assignees.as_deref() == Some(&[login.clone()][..]);
        if report.reasons.is_empty() && (verified.is_some() || assigned) {
            task_records::confirm_claim(store, &report.task_id)?;
        } else if verified.is_none() {
            report.reasons.push("claim_intent_unverified");
        }
    } else {
        report.reasons.push("claim_intent_unverified");
    }
    Ok(())
}

fn recover_worktree(
    store: &mut StateStore,
    selection: &SelectionEvidence,
    report: &mut SourceReconciliation,
) -> Result<(), StateError> {
    let owner = crate::ownership::WorktreeOwner::acquire(store.root(), &report.task_id);
    let Ok(_owner) = owner else {
        report.reasons.push("worktree_unverified");
        return Ok(());
    };
    match crate::disk::inspect(
        &selection.effective_config.worktree_root,
        selection.effective_config.min_free_space_bytes,
    ) {
        Ok(space) if space.dispatch_held => {
            report.reasons.push("worktree_unverified");
            return Ok(());
        }
        Ok(_) => {}
        Err(worktree::WorktreeError::State(error)) => return Err(error),
        Err(worktree::WorktreeError::Io(error)) => return Err(error.into()),
        Err(_) => {
            report.reasons.push("worktree_read_failed");
            return Ok(());
        }
    }
    match worktree::ensure_worktree(
        store,
        &report.task_id,
        &selection.effective_config.worktree_root,
        &selection.candidate.mapping,
    ) {
        Ok(_) => {
            let record = worktree_records::worktree_record(store, &report.task_id)?
                .ok_or(StateError::InvalidSelection)?;
            if worktree::verify_never_dispatched(
                &record,
                &selection.candidate.mapping,
                &selection.effective_config.worktree_root,
                &report.task_id,
            )
            .is_err()
            {
                report.reasons.push("worktree_unverified");
            } else {
                report.worktree = Some(WorktreeInspection::IdentityMatches);
            }
        }
        Err(worktree::WorktreeError::State(error)) => return Err(error),
        Err(_) => {
            report.reasons.push("worktree_unverified");
            observe_worktree(store, selection, report)?;
        }
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
    let expected_path = match worktree::check_root(root) {
        Ok(root) => root.join(&report.task_id),
        Err(_) => {
            report.reasons.push("worktree_read_failed");
            return Ok(());
        }
    };
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
