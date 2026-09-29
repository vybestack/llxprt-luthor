use crate::{
    config::Marker,
    eligibility::Candidate,
    github::{
        project::{Issue, ProjectError, ProjectItem, ProjectReader, enumerate},
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
fn fresh<R: ProjectReader>(
    reader: &mut R,
    c: &Candidate,
) -> Result<(ProjectItem, Issue), ClaimError> {
    let entries = enumerate(reader, &c.project_id)?;
    let mut found = entries
        .into_iter()
        .filter(|(i, _)| i.item_id == c.item_id && i.issue_node_id == c.issue_node_id);
    let value = found.next().ok_or(ClaimError::Changed)?;
    if found.next().is_some() {
        return Err(ClaimError::Changed);
    }
    let (item, issue) = value;
    let marker = match &c.marker {
        Marker::Label { name } => issue.labels.iter().any(|x| x == name),
        Marker::ProjectField { name, value } => {
            item.fields.iter().any(|(n, v)| n == name && v == value)
        }
    };
    if issue.node_id != c.issue_node_id
        || issue.repository != c.repository
        || issue.tracker_repo_id != c.tracker_repo_id
        || issue.number != c.issue_number
        || issue.state != "open"
        || !marker
        || issue.milestone != c.source.milestone
        || item.item_id != c.item_id
    {
        return Err(ClaimError::Changed);
    }
    Ok((item, issue))
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
    if principal.trim().is_empty() {
        return Err(ClaimError::Changed);
    }
    let (item, issue) = fresh(projects, c)?;
    if !issue.assignees.is_empty() {
        return Err(ClaimError::Changed);
    }
    if lookup(prs, &c.mapping.code_repository, &c.issue_url)? != LookupResult::Absent {
        return Err(ClaimError::ExistingPr);
    }
    store.record_claim_intent(task_id, principal, &c.repository, c.issue_number)?;
    if writer
        .assign(&c.repository, c.issue_number, principal)
        .is_err()
    {
        store.set_task_phase(task_id, "held")?;
        return Err(ClaimError::Assignment);
    }
    let (after_item, after) = fresh(projects, c)?;
    if after.assignees != [principal] || after_item.item_id != item.item_id {
        store.set_task_phase(task_id, "held")?;
        return Err(ClaimError::Verify);
    }
    if lookup(prs, &c.mapping.code_repository, &c.issue_url)? != LookupResult::Absent {
        store.set_task_phase(task_id, "held")?;
        return Err(ClaimError::Verify);
    }
    store.record_evidence(task_id, None, "claim_verified", principal)?;
    store.set_task_phase(task_id, "claimed")?;
    Ok(())
}
