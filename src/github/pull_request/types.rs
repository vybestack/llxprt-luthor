use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestEvidence {
    pub id: u64,
    pub number: u64,
    pub url: String,
    pub repository_id: u64,
    pub repository: String,
    pub base_repository_id: u64,
    pub base_repository: String,
    pub base_branch: String,
    pub head_repository_id: u64,
    pub head_repository: String,
    pub head_branch: String,
    pub author: String,
    pub draft: bool,
    pub tracker_issue_url: String,
    pub body: String,
    pub checks: Option<Vec<String>>,
    pub created_at: String,
    pub commit_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupResult {
    Absent,
    OpenPreexisting(Box<PullRequestEvidence>),
    Ambiguous(Vec<PullRequestEvidence>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ErrorCategory {
    Permission,
    RateLimit,
    NotFound,
    Malformed,
    Transport,
    Unknown,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("pull request lookup failed ({category:?}, {code})")]
pub struct LookupError {
    pub category: ErrorCategory,
    pub code: &'static str,
    pub status: Option<u16>,
}

pub trait PullRequestReader {
    fn authenticated_identity(&mut self) -> Result<String, LookupError>;
    fn page(&mut self, repository: &str, page: u32) -> Result<Vec<Value>, LookupError>;
    fn detail(&mut self, repository: &str, number: u64) -> Result<Value, LookupError>;
    fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError>;
}

pub(crate) fn error(
    category: ErrorCategory,
    code: &'static str,
    status: Option<u16>,
) -> LookupError {
    LookupError {
        category,
        code,
        status,
    }
}
