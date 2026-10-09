use crate::state::{journal, task_records};
use crate::{
    config::Marker,
    eligibility::Candidate,
    github::{
        project::{Issue, ProjectError, ProjectItem, ProjectReader},
        pull_request::{LookupError, LookupResult, PullRequestReader, lookup},
    },
    state::{StateError, StateStore},
};
use std::{path::PathBuf, process::Command};
use thiserror::Error;

pub trait AssignmentWriter {
    fn assign(
        &mut self,
        repository: &str,
        number: u64,
        principal: &str,
    ) -> Result<(), AssignmentError>;
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentError {
    pub ambiguous: bool,
}
pub struct GhAssignmentWriter {
    pub executable: PathBuf,
}
impl AssignmentWriter for GhAssignmentWriter {
    fn assign(
        &mut self,
        repository: &str,
        number: u64,
        principal: &str,
    ) -> Result<(), AssignmentError> {
        let path = format!("repos/{repository}/issues/{number}/assignees");
        let field = format!("assignees[]={principal}");
        let result = Command::new(&self.executable)
            .args(["api", "-X", "POST", &path, "-f", &field])
            .output();
        match result {
            Ok(out) if out.status.success() => Ok(()),
            _ => Err(AssignmentError { ambiguous: true }),
        }
    }
}
#[derive(Debug, Error)]
pub enum ClaimError {
    #[error("state persistence failed: {0}")]
    State(#[from] StateError),
    #[error("source read failed: {0}")]
    Source(#[from] ProjectError),
    #[error("PR lookup failed: {0}")]
    PullRequest(#[from] LookupError),
    #[error("PR lookup was not absent")]
    ExistingPr,
    #[error("source evidence changed before claim")]
    Changed,
    #[error("assignment intent already exists; automatic retry forbidden")]
    IntentExists,
    #[error("assignment was ambiguous or failed; task held")]
    Assignment,
    #[error("post-assignment verification failed; task held")]
    Verify,
}
pub(crate) fn fresh<R: ProjectReader>(
    reader: &mut R,
    c: &Candidate,
) -> Result<(ProjectItem, Issue), ClaimError> {
    let item = find_project_item(reader, c)?;
    let issue = reader.issue(&item).map_err(ProjectError::IssueRead)?;
    verify_issue_identity(&item, &issue)?;
    verify_claim_snapshot(c, &item, &issue)?;
    Ok((item, issue))
}

fn find_project_item<R: ProjectReader>(
    reader: &mut R,
    c: &Candidate,
) -> Result<ProjectItem, ClaimError> {
    let mut cursor = None;
    let mut seen_cursors = std::collections::HashSet::new();
    let mut seen_items = std::collections::HashSet::new();
    let mut seen_issues = std::collections::HashSet::new();
    let mut target = None;
    loop {
        let page = reader
            .page(&c.project_id, cursor.as_deref())
            .map_err(ProjectError::Read)?;
        for item in page.items {
            verify_unique_item(&item, &mut seen_items, &mut seen_issues)?;
            if references_candidate(&item, c) {
                if target.is_some() {
                    return Err(ClaimError::Changed);
                }
                verify_target_item(&item, c)?;
                target = Some(item);
            }
        }
        if !page.has_next_page {
            break;
        }
        let next = page
            .end_cursor
            .filter(|cursor| !cursor.is_empty())
            .ok_or(ClaimError::Source(ProjectError::MissingCursor))?;
        if !seen_cursors.insert(next.clone()) {
            return Err(ClaimError::Source(ProjectError::RepeatedCursor));
        }
        cursor = Some(next);
    }
    target.ok_or(ClaimError::Changed)
}

fn verify_unique_item(
    item: &ProjectItem,
    seen_items: &mut std::collections::HashSet<String>,
    seen_issues: &mut std::collections::HashSet<String>,
) -> Result<(), ClaimError> {
    if item.item_id.is_empty() || item.issue_node_id.is_empty() {
        return Err(ClaimError::Changed);
    }
    if !seen_items.insert(item.item_id.clone()) || !seen_issues.insert(item.issue_node_id.clone()) {
        return Err(ClaimError::Changed);
    }
    Ok(())
}

fn references_candidate(item: &ProjectItem, c: &Candidate) -> bool {
    item.item_id == c.item_id
        || item.issue_node_id == c.issue_node_id
        || (item.repository == c.repository && item.issue_number == c.issue_number)
}

fn verify_target_item(item: &ProjectItem, c: &Candidate) -> Result<(), ClaimError> {
    if item.item_id != c.item_id
        || item.issue_node_id != c.issue_node_id
        || item.repository != c.repository
        || item.tracker_repo_id != c.tracker_repo_id
        || item.issue_number != c.issue_number
    {
        return Err(ClaimError::Changed);
    }
    Ok(())
}

fn verify_issue_identity(item: &ProjectItem, issue: &Issue) -> Result<(), ClaimError> {
    if issue.node_id != item.issue_node_id
        || issue.repository != item.repository
        || issue.tracker_repo_id != item.tracker_repo_id
        || issue.number != item.issue_number
        || issue.url
            != format!(
                "https://github.com/{}/issues/{}",
                item.repository, item.issue_number
            )
    {
        return Err(ClaimError::Changed);
    }
    Ok(())
}

fn claim_marker_matches(
    marker: &Marker,
    item: &ProjectItem,
    issue: &Issue,
) -> Result<bool, ClaimError> {
    match marker {
        Marker::Label { name } => Ok(issue.labels.iter().any(|x| x == name)),
        Marker::ProjectField { name, value } => {
            if item.unsupported_fields.iter().any(|field| field == name) {
                return Err(ClaimError::Changed);
            }
            Ok(item.fields.iter().any(|(n, v)| n == name && v == value))
        }
    }
}

fn verify_claim_snapshot(
    c: &Candidate,
    item: &ProjectItem,
    issue: &Issue,
) -> Result<(), ClaimError> {
    let marker = claim_marker_matches(&c.marker, item, issue)?;
    if issue.node_id != c.issue_node_id
        || issue.repository != c.repository
        || issue.tracker_repo_id != c.tracker_repo_id
        || issue.number != c.issue_number
        || issue.state != "open"
        || !marker
        || issue.milestone != c.milestone_title
        || issue.milestone_id != c.milestone_id
        || c.source
            .milestone
            .as_ref()
            .is_some_and(|title| issue.milestone.as_ref() != Some(title))
        || item.item_id != c.item_id
    {
        return Err(ClaimError::Changed);
    }
    Ok(())
}
pub fn claim<P: ProjectReader, Q: PullRequestReader, W: AssignmentWriter>(
    store: &mut StateStore,
    task_id: &str,
    c: &Candidate,
    principal: &str,
    projects: &mut P,
    prs: &mut Q,
    writer: &mut W,
) -> Result<(), ClaimError> {
    let selection = task_records::selection_evidence(store, task_id)?.ok_or(ClaimError::Changed)?;
    if selection.candidate != *c {
        return Err(ClaimError::Changed);
    }
    let configured_login = selection.effective_config.assignment_login;
    if principal != configured_login || principal.trim().is_empty() {
        return Err(ClaimError::Changed);
    }
    let (item, issue) = fresh(projects, c)?;
    if !issue.assignees.is_empty() {
        return Err(ClaimError::Changed);
    }
    if lookup(prs, &c.mapping.code_repository, &c.issue_url)? != LookupResult::Absent {
        return Err(ClaimError::ExistingPr);
    }
    task_records::record_claim_intent(store, task_id, principal, &c.repository, c.issue_number)?;
    let result = (|| {
        writer
            .assign(&c.repository, c.issue_number, principal)
            .map_err(|_| ClaimError::Assignment)?;
        let (after_item, after) = fresh(projects, c).map_err(|_| ClaimError::Verify)?;
        if after.assignees != [principal] || after_item.item_id != item.item_id {
            return Err(ClaimError::Verify);
        }
        if lookup(prs, &c.mapping.code_repository, &c.issue_url).map_err(|_| ClaimError::Verify)?
            != LookupResult::Absent
        {
            return Err(ClaimError::Verify);
        }
        Ok(())
    })();
    if let Err(error) = result {
        task_records::set_task_phase(store, task_id, "held")?;
        return Err(error);
    }
    journal::record_evidence(store, task_id, None, "claim_verified", principal)?;
    task_records::set_task_phase(store, task_id, "claimed")?;
    Ok(())
}
