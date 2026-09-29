use crate::{
    github::pull_request::{LookupError, PullRequestReader},
    state::{StateError, StateStore},
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrIdentityEvidence {
    pub id: u64,
    pub repository_id: u64,
    pub repository: String,
    pub base_repository: String,
    pub base_branch: String,
    pub head_repository_id: u64,
    pub head_repository: String,
    pub head_branch: String,
    pub author: String,
    pub open: bool,
    pub draft: bool,
    pub checks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedPr {
    pub issue_url: String,
    pub repository_id: u64,
    pub repository: String,
    pub base_branch: String,
    pub head_repository_id: u64,
    pub head_repository: String,
    pub task_branch: String,
    pub allowed_author: String,
    pub current_identity: String,
}
#[derive(Debug, Error)]
pub enum ExpectedPrError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("task has no persisted selection, verified worktree, or attempt")]
    MissingEvidence,
    #[error("persisted worktree identity conflicts with its intent or selection")]
    WorktreeMismatch,
    #[error("current identity does not match the configured allowed PR author")]
    AuthorMismatch,
    #[error("repository identity lookup returned an invalid ID")]
    InvalidRepositoryId,
    #[error(transparent)]
    RepositoryLookup(#[from] LookupError),
}

pub fn expected_for_task<Q: PullRequestReader>(
    store: &StateStore,
    task_id: &str,
    reader: &mut Q,
    current_identity: &str,
) -> Result<ExpectedPr, ExpectedPrError> {
    let selection = store
        .selection_evidence(task_id)?
        .ok_or(ExpectedPrError::MissingEvidence)?;
    let worktree = store
        .worktree_record(task_id)?
        .ok_or(ExpectedPrError::MissingEvidence)?;
    let identity = worktree.identity.ok_or(ExpectedPrError::MissingEvidence)?;
    let mapping = &selection.candidate.mapping;
    if identity.branch != worktree.intent.branch
        || identity.repository != mapping.code_repository
        || worktree.intent.repository != mapping.code_repository
        || worktree.intent.base != mapping.base_branch
        || identity.base != mapping.base_branch
        || identity.branch.is_empty()
        || identity.head.is_empty()
        || identity.repository.is_empty()
    {
        return Err(ExpectedPrError::WorktreeMismatch);
    }
    if store.latest_attempt(task_id)?.is_none() {
        return Err(ExpectedPrError::MissingEvidence);
    }
    if current_identity != mapping.allowed_pr_author || current_identity.is_empty() {
        return Err(ExpectedPrError::AuthorMismatch);
    }
    let repository_id = reader.repository_identity(&mapping.code_repository)?;
    let head_repository_id = reader.repository_identity(&mapping.allowed_pr_head_repository)?;
    if repository_id == 0 || head_repository_id == 0 {
        return Err(ExpectedPrError::InvalidRepositoryId);
    }
    Ok(ExpectedPr {
        issue_url: selection.candidate.issue_url,
        repository_id,
        repository: mapping.code_repository.clone(),
        base_branch: mapping.base_branch.clone(),
        head_repository_id,
        head_repository: mapping.allowed_pr_head_repository.clone(),
        task_branch: identity.branch,
        allowed_author: mapping.allowed_pr_author.clone(),
        current_identity: current_identity.to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    Matching(PrIdentityEvidence),
    Mismatch(&'static str),
}

pub fn verify(pr: PrIdentityEvidence, expected: &ExpectedPr, body: &str) -> Verification {
    if !body
        .lines()
        .any(|line| line == format!("Tracker-Issue: {}", expected.issue_url))
    {
        return Verification::Mismatch("tracker_link");
    }
    if !pr.open || pr.id == 0 {
        return Verification::Mismatch("not_open_or_missing_id");
    }
    if pr.repository_id != expected.repository_id || pr.repository != expected.repository {
        return Verification::Mismatch("target_repository");
    }
    if pr.base_repository != expected.repository || pr.base_branch != expected.base_branch {
        return Verification::Mismatch("base");
    }
    if pr.head_repository_id != expected.head_repository_id
        || pr.head_repository != expected.head_repository
        || pr.head_branch != expected.task_branch
    {
        return Verification::Mismatch("head");
    }
    if pr.author != expected.allowed_author || pr.author != expected.current_identity {
        return Verification::Mismatch("author");
    }
    Verification::Matching(pr)
}
