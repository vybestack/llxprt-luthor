use std::{path::PathBuf, process::Command};

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestEvidence {
    pub id: u64,
    pub url: String,
    pub repository: String,
    pub base_repository: String,
    pub base_branch: String,
    pub head_repository: String,
    pub head_branch: String,
    pub author: String,
    pub draft: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupResult {
    Absent,
    OpenPreexisting(PullRequestEvidence),
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
    fn page(&mut self, repository: &str, page: u32) -> Result<Vec<Value>, LookupError>;
    fn detail(&mut self, repository: &str, number: u64) -> Result<Value, LookupError>;
}

pub struct GhPullRequestReader {
    pub executable: PathBuf,
}

impl GhPullRequestReader {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    fn api(&self, args: &[&str]) -> Result<Value, LookupError> {
        let output = Command::new(&self.executable)
            .args(args)
            .output()
            .map_err(|_| error(ErrorCategory::Transport, "transport-error", None))?;
        let parsed = serde_json::from_slice::<Value>(&output.stdout);
        if !output.status.success() {
            let value = parsed.as_ref().ok();
            let status = value
                .and_then(|v| v.get("status"))
                .and_then(status_value)
                .or_else(|| {
                    String::from_utf8_lossy(&output.stderr)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .windows(2)
                        .find_map(|w| (w[0] == "HTTP").then(|| w[1].parse().ok()).flatten())
                });
            let message = value
                .and_then(|v| v.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            let category = match status {
                Some(401) => ErrorCategory::Permission,
                Some(403) if message.contains("rate limit") || stderr.contains("rate limit") => {
                    ErrorCategory::RateLimit
                }
                Some(403) => ErrorCategory::Permission,
                Some(404) => ErrorCategory::NotFound,
                Some(429) => ErrorCategory::RateLimit,
                _ => ErrorCategory::Unknown,
            };
            return Err(error(category, "command-failed", status));
        }
        parsed.map_err(|_| error(ErrorCategory::Malformed, "invalid-json", None))
    }
}

impl PullRequestReader for GhPullRequestReader {
    fn page(&mut self, repository: &str, page: u32) -> Result<Vec<Value>, LookupError> {
        let path = format!("repos/{repository}/pulls?state=open&per_page=100&page={page}");
        let value = self.api(&["api", &path])?;
        value
            .as_array()
            .cloned()
            .ok_or_else(|| error(ErrorCategory::Malformed, "invalid-page", None))
    }

    fn detail(&mut self, repository: &str, number: u64) -> Result<Value, LookupError> {
        self.api(&["api", &format!("repos/{repository}/pulls/{number}")])
    }
}

pub fn lookup<R: PullRequestReader>(
    reader: &mut R,
    repository: &str,
    issue_url: &str,
) -> Result<LookupResult, LookupError> {
    let mut matches = Vec::new();
    let mut page_number = 1;
    loop {
        if page_number > 10_000 {
            return Err(error(ErrorCategory::Malformed, "page-limit", None));
        }
        let page = reader.page(repository, page_number)?;
        if page.len() > 100 {
            return Err(error(ErrorCategory::Malformed, "oversized-page", None));
        }
        let short_page = page.len() < 100;
        for item in page {
            let number = item
                .get("number")
                .and_then(Value::as_u64)
                .ok_or_else(|| error(ErrorCategory::Malformed, "invalid-list-entry", None))?;
            let body = item
                .get("body")
                .and_then(Value::as_str)
                .ok_or_else(|| error(ErrorCategory::Malformed, "invalid-list-entry", None))?;
            if !has_tracker_line(body, issue_url) {
                continue;
            }
            let detail = reader.detail(repository, number)?;
            let evidence = parse_evidence(&detail, repository)?;
            if evidence.id == 0 {
                return Err(error(ErrorCategory::Malformed, "invalid-pr-id", None));
            }
            matches.push(evidence);
        }
        if short_page {
            break;
        }
        page_number = page_number
            .checked_add(1)
            .ok_or_else(|| error(ErrorCategory::Malformed, "page-overflow", None))?;
    }
    Ok(match matches.len() {
        0 => LookupResult::Absent,
        1 => LookupResult::OpenPreexisting(matches.remove(0)),
        _ => LookupResult::Ambiguous(matches),
    })
}

fn has_tracker_line(body: &str, issue_url: &str) -> bool {
    body.lines()
        .any(|line| line == format!("Tracker-Issue: {issue_url}"))
}

fn parse_evidence(value: &Value, repository: &str) -> Result<PullRequestEvidence, LookupError> {
    let bad = || error(ErrorCategory::Malformed, "invalid-pr-details", None);
    let id = value.get("id").and_then(Value::as_u64).ok_or_else(bad)?;
    let state = value.get("state").and_then(Value::as_str).ok_or_else(bad)?;
    if state != "open" {
        return Err(bad());
    }
    let url = string(value, "html_url").ok_or_else(bad)?;
    let expected_prefix = format!("https://github.com/{repository}/pull/");
    if !url.starts_with(&expected_prefix) {
        return Err(bad());
    }
    let base_repository = value
        .pointer("/base/repo/full_name")
        .and_then(Value::as_str)
        .ok_or_else(bad)?
        .to_owned();
    let base_branch = value
        .pointer("/base/ref")
        .and_then(Value::as_str)
        .ok_or_else(bad)?
        .to_owned();
    let head_repository = value
        .pointer("/head/repo/full_name")
        .and_then(Value::as_str)
        .ok_or_else(bad)?
        .to_owned();
    let head_branch = value
        .pointer("/head/ref")
        .and_then(Value::as_str)
        .ok_or_else(bad)?
        .to_owned();
    let author = value
        .pointer("/user/login")
        .and_then(Value::as_str)
        .ok_or_else(bad)?
        .to_owned();
    if base_repository != repository
        || base_branch.is_empty()
        || head_branch.is_empty()
        || head_repository.is_empty()
        || author.is_empty()
    {
        return Err(bad());
    }
    let draft = value
        .get("draft")
        .and_then(Value::as_bool)
        .ok_or_else(bad)?;
    Ok(PullRequestEvidence {
        id,
        url: url.to_owned(),
        repository: repository.to_owned(),
        base_repository,
        base_branch,
        head_repository,
        head_branch,
        author,
        draft,
    })
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}
fn status_value(value: &Value) -> Option<u16> {
    value
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| value.as_u64().and_then(|s| u16::try_from(s).ok()))
}
fn error(category: ErrorCategory, code: &'static str, status: Option<u16>) -> LookupError {
    LookupError {
        category,
        code,
        status,
    }
}
