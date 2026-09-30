use std::{path::PathBuf, process::Command};

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

pub struct GhPullRequestReader {
    pub executable: PathBuf,
}

impl GhPullRequestReader {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    pub fn repository_identity(&self, name: &str) -> Result<u64, LookupError> {
        let malformed = || error(ErrorCategory::Malformed, "invalid-repository", None);
        let mut segments = name.split('/');
        let Some(owner) = segments.next() else {
            return Err(malformed());
        };
        let Some(repository) = segments.next() else {
            return Err(malformed());
        };
        if segments.next().is_some()
            || !valid_repository_segment(owner)
            || !valid_repository_segment(repository)
        {
            return Err(malformed());
        }

        let value = self.api(&["api", &format!("repos/{name}")])?;
        let id = value
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or_else(malformed)?;
        if value.get("full_name").and_then(Value::as_str) != Some(name) {
            return Err(malformed());
        }
        Ok(id)
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

impl GhPullRequestReader {
    fn checks(&self, repository: &str, sha: &str) -> Option<Vec<Value>> {
        let mut result = Vec::new();
        let mut complete = false;
        for page in 1..=10 {
            let path =
                format!("repos/{repository}/commits/{sha}/check-runs?per_page=100&page={page}");
            let response = self.api(&["api", &path]).ok()?;
            let runs = response.get("check_runs")?.as_array()?;
            let total = response.get("total_count")?.as_u64()? as usize;
            if runs.len() > 100 || result.len().saturating_add(runs.len()) > total {
                return None;
            }
            for run in runs {
                let name = run.get("name")?.as_str()?;
                let status = run.get("status")?.as_str()?;
                if name.is_empty()
                    || name.len() > 128
                    || !name.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
                    || status.is_empty()
                    || status.len() > 32
                    || !status.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                {
                    return None;
                }
                let conclusion = run
                    .get("conclusion")
                    .and_then(Value::as_str)
                    .unwrap_or("pending");
                if conclusion.len() > 32
                    || !conclusion
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'_')
                {
                    return None;
                }
                result.push(Value::String(format!("{}:{}:{}", name, status, conclusion)));
            }
            if result.len() == total {
                complete = true;
                break;
            }
            if runs.is_empty() {
                return None;
            }
        }
        if !complete || result.len() > 1000 {
            return None;
        }
        let status_path = format!("repos/{repository}/commits/{sha}/status");
        let status = self.api(&["api", &status_path]).ok()?;
        let statuses = status.get("statuses")?.as_array()?;
        if statuses.len() > 1000 {
            return None;
        }
        for item in statuses {
            let context = item.get("context")?.as_str()?;
            let state = item.get("state")?.as_str()?;
            if context.is_empty()
                || context.len() > 128
                || !context.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
                || state.is_empty()
                || state.len() > 32
                || !state.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            {
                return None;
            }
            let summary = format!("{context}:{state}");
            if !result
                .iter()
                .any(|existing| existing.as_str() == Some(&summary))
            {
                result.push(Value::String(summary));
            }
        }
        Some(result)
    }
}

fn valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_repository_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

impl PullRequestReader for GhPullRequestReader {
    fn authenticated_identity(&mut self) -> Result<String, LookupError> {
        let output = Command::new(&self.executable)
            .args(["api", "user", "--jq", ".login"])
            .output()
            .map_err(|_| error(ErrorCategory::Transport, "identity-command-failed", None))?;
        if !output.status.success() {
            return Err(error(
                ErrorCategory::Unknown,
                "identity-command-failed",
                None,
            ));
        }
        let stdout = std::str::from_utf8(&output.stdout)
            .map_err(|_| error(ErrorCategory::Malformed, "identity-invalid-output", None))?;
        let login = stdout.strip_suffix('\n').unwrap_or(stdout);
        if login.is_empty()
            || login.trim() != login
            || login.contains(['\n', '\r'])
            || !login
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(error(
                ErrorCategory::Malformed,
                "identity-invalid-output",
                None,
            ));
        }
        if login != "acoliver" {
            return Err(error(ErrorCategory::Permission, "identity-mismatch", None));
        }
        Ok(login.to_owned())
    }

    fn page(&mut self, repository: &str, page: u32) -> Result<Vec<Value>, LookupError> {
        let path = format!("repos/{repository}/pulls?state=open&per_page=100&page={page}");
        self.api(&["api", &path])?
            .as_array()
            .cloned()
            .ok_or_else(|| error(ErrorCategory::Malformed, "invalid-page", None))
    }
    fn detail(&mut self, repository: &str, number: u64) -> Result<Value, LookupError> {
        let mut detail = self.api(&["api", &format!("repos/{repository}/pulls/{number}")])?;
        let valid_repo = repository.split('/').count() == 2
            && repository.split('/').all(valid_repository_segment);
        let sha = detail.pointer("/head/sha").and_then(Value::as_str);
        if valid_repo
            && let Some(sha) = sha.filter(|sha| valid_sha(sha))
            && let Some(checks) = self.checks(repository, sha)
            && let Some(object) = detail.as_object_mut()
        {
            object.insert("luthor_checks".to_owned(), Value::Array(checks));
        }
        Ok(detail)
    }

    fn repository_identity(&mut self, name: &str) -> Result<u64, LookupError> {
        GhPullRequestReader::repository_identity(self, name)
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
                .filter(|n| *n > 0)
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
            if evidence.number != number || evidence.tracker_issue_url != issue_url {
                return Err(error(ErrorCategory::Malformed, "invalid-pr-details", None));
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
        1 => LookupResult::OpenPreexisting(Box::new(matches.remove(0))),
        _ => LookupResult::Ambiguous(matches),
    })
}

fn has_tracker_line(body: &str, issue_url: &str) -> bool {
    body.lines()
        .any(|line| line == format!("Tracker-Issue: {issue_url}"))
}

fn parse_evidence(value: &Value, repository: &str) -> Result<PullRequestEvidence, LookupError> {
    let bad = || error(ErrorCategory::Malformed, "invalid-pr-details", None);
    let id = value
        .get("id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(bad)?;
    let number = value
        .get("number")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(bad)?;
    let state = value.get("state").and_then(Value::as_str).ok_or_else(bad)?;
    if state != "open" {
        return Err(bad());
    }
    let url = string(value, "html_url").ok_or_else(bad)?;
    let expected_prefix = format!("https://github.com/{repository}/pull/");
    let url_number = url.strip_prefix(&expected_prefix).ok_or_else(bad)?;
    if url_number != number.to_string() {
        return Err(bad());
    }
    let repository_id = value
        .pointer("/base/repo/id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(bad)?;
    let base_repository_id = repository_id;
    let head_repository_id = value
        .pointer("/head/repo/id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(bad)?;
    let body = value.get("body").and_then(Value::as_str).ok_or_else(bad)?;
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
    let created_at = value
        .get("created_at")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(bad)?
        .to_owned();
    let commit_sha = value
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(bad)?
        .to_owned();
    let tracker_issue_url = body
        .lines()
        .find_map(|line| line.strip_prefix("Tracker-Issue: "))
        .unwrap_or("")
        .to_owned();
    Ok(PullRequestEvidence {
        id,
        number,
        url: url.to_owned(),
        repository_id,
        repository: repository.to_owned(),
        base_repository_id,
        base_repository,
        base_branch,
        head_repository_id,
        head_repository,
        head_branch,
        author,
        draft,
        tracker_issue_url,
        body: body.to_owned(),
        checks: value
            .get("luthor_checks")
            .and_then(Value::as_array)
            .map(|checks| {
                checks
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            }),
        created_at,
        commit_sha,
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

#[cfg(test)]
mod identity_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn gh_script(contents: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gh");
        std::fs::write(&path, contents).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    #[test]
    fn accepts_single_authorized_login() {
        let (_dir, executable) = gh_script(b"#!/bin/sh\nprintf 'acoliver\\n'\n");
        let mut reader = GhPullRequestReader::new(executable);
        assert_eq!(reader.authenticated_identity().unwrap(), "acoliver");
    }

    #[test]
    fn rejects_malformed_or_extra_output() {
        for script in [
            b"#!/bin/sh\nprintf '\\n'\n".as_slice(),
            b"#!/bin/sh\nprintf 'acoliver\\nother\\n'\n",
        ] {
            let (_dir, executable) = gh_script(script);
            let mut reader = GhPullRequestReader::new(executable);
            assert_eq!(
                reader.authenticated_identity().unwrap_err().category,
                ErrorCategory::Malformed
            );
        }
    }
}
