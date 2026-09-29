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
    let mut cursor = None;
    let mut seen_cursors = std::collections::HashSet::new();
    let mut seen_items = std::collections::HashSet::new();
    let item = loop {
        let page = reader
            .page(&c.project_id, cursor.as_deref())
            .map_err(ProjectError::Read)?;
        let matches = page
            .items
            .into_iter()
            .filter(|item| item.item_id == c.item_id || item.issue_node_id == c.issue_node_id)
            .collect::<Vec<_>>();
        for candidate in &matches {
            if !seen_items.insert(candidate.item_id.clone()) {
                return Err(ClaimError::Changed);
            }
        }
        if matches.len() > 1 {
            return Err(ClaimError::Changed);
        }
        if let Some(item) = matches.into_iter().next() {
            if item.item_id != c.item_id
                || item.issue_node_id != c.issue_node_id
                || item.repository != c.repository
                || item.tracker_repo_id != c.tracker_repo_id
                || item.issue_number != c.issue_number
            {
                return Err(ClaimError::Changed);
            }
            break item;
        }
        if !page.has_next_page {
            return Err(ClaimError::Changed);
        }
        let next = page
            .end_cursor
            .filter(|cursor| !cursor.is_empty())
            .ok_or(ClaimError::Source(ProjectError::MissingCursor))?;
        if !seen_cursors.insert(next.clone()) {
            return Err(ClaimError::Source(ProjectError::RepeatedCursor));
        }
        cursor = Some(next);
    };
    let issue = reader.issue(&item).map_err(ProjectError::IssueRead)?;
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
    let marker = match &c.marker {
        Marker::Label { name } => issue.labels.iter().any(|x| x == name),
        Marker::ProjectField { name, value } => {
            if item.unsupported_fields.iter().any(|field| field == name) {
                return Err(ClaimError::Changed);
            }
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
        || issue.milestone_id != c.milestone_id
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
    let configured_login = store
        .claim_assignment_login(task_id)?
        .ok_or(ClaimError::Changed)?;
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
    store.record_claim_intent(task_id, principal, &c.repository, c.issue_number)?;
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
        store.set_task_phase(task_id, "held")?;
        return Err(error);
    }
    store.record_evidence(task_id, None, "claim_verified", principal)?;
    store.set_task_phase(task_id, "claimed")?;
    Ok(())
}
