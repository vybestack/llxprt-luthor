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
