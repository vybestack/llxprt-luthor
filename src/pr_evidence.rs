use crate::{
    github::pull_request::PullRequestEvidence,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerifiedOpenPr {
    pub(crate) id: u64,
    pub(crate) number: u64,
    pub(crate) url: String,
    pub(crate) repository_id: u64,
    pub(crate) repository: String,
    pub(crate) head_repository_id: u64,
    pub(crate) head_repository: String,
    pub(crate) base_branch: String,
    pub(crate) head_branch: String,
    pub(crate) author: String,
    pub(crate) active_login: String,
    pub(crate) tracker_issue_url: String,
    pub(crate) draft: bool,
    pub(crate) checks: Vec<String>,
    pub(crate) created_at: String,
    pub(crate) head_commit_sha: String,
    pub(crate) observed_at: u64,
    pub(crate) attempt_id: String,
}

impl VerifiedOpenPr {
    pub fn from_matching(
        pr: PullRequestEvidence,
        expected: &ExpectedPr,
        current_login: &str,
        attempt_id: &str,
        observed_at: u64,
    ) -> Result<Self, &'static str> {
        let identity = PrIdentityEvidence {
            id: pr.id,
            repository_id: pr.repository_id,
            repository: pr.repository.clone(),
            base_repository: pr.base_repository.clone(),
            base_branch: pr.base_branch.clone(),
            head_repository_id: pr.head_repository_id,
            head_repository: pr.head_repository.clone(),
            head_branch: pr.head_branch.clone(),
            author: pr.author.clone(),
            open: true,
            draft: pr.draft,
            checks: pr.checks.clone(),
        };
        if !matches!(
            verify(identity, expected, &pr.body),
            Verification::Matching(_)
        ) {
            return Err("pull_request_mismatch");
        }
        if expected.current_identity != current_login {
            return Err("current_identity_mismatch");
        }
        if expected.issue_url != pr.tracker_issue_url {
            return Err("tracker_issue_mismatch");
        }
        if pr.id == 0
            || observed_at == 0
            || current_login.is_empty()
            || attempt_id.is_empty()
            || pr.number == 0
            || [
                pr.url.as_str(),
                pr.repository.as_str(),
                pr.head_repository.as_str(),
                pr.base_branch.as_str(),
                pr.head_branch.as_str(),
                pr.author.as_str(),
                pr.tracker_issue_url.as_str(),
                pr.created_at.as_str(),
                pr.commit_sha.as_str(),
            ]
            .iter()
            .any(|value| value.is_empty())
        {
            return Err("incomplete_evidence");
        }
        Ok(Self {
            id: pr.id,
            number: pr.number,
            url: pr.url,
            repository_id: pr.repository_id,
            repository: pr.repository,
            head_repository_id: pr.head_repository_id,
            head_repository: pr.head_repository,
            base_branch: pr.base_branch,
            head_branch: pr.head_branch,
            author: pr.author,
            active_login: current_login.to_owned(),
            tracker_issue_url: pr.tracker_issue_url,
            draft: pr.draft,
            checks: pr.checks,
            created_at: pr.created_at,
            head_commit_sha: pr.commit_sha,
            observed_at,
            attempt_id: attempt_id.to_owned(),
        })
    }
}
