use std::{path::PathBuf, process::Command};

use serde_json::Value;

use super::{
    checks::{CheckRun, CheckRunPage, append_statuses},
    types::{ErrorCategory, LookupError, PullRequestReader, error},
};

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

impl GhPullRequestReader {
    fn checks(&self, repository: &str, sha: &str) -> Option<Vec<Value>> {
        let mut result = self.check_runs(repository, sha)?;
        let path = format!("repos/{repository}/commits/{sha}/status");
        let status = self.api(&["api", &path]).ok()?;
        append_statuses(&mut result, &status)?;
        Some(result)
    }

    fn check_runs(&self, repository: &str, sha: &str) -> Option<Vec<Value>> {
        let mut result = Vec::new();
        for page in 1..=10 {
            let path =
                format!("repos/{repository}/commits/{sha}/check-runs?per_page=100&page={page}");
            let response = self.api(&["api", &path]).ok()?;
            let page = CheckRunPage::parse(&response, result.len())?;
            for run in page.runs {
                result.push(CheckRun::parse(run)?.summary());
            }
            if result.len() == page.total {
                return (result.len() <= 1000).then_some(result);
            }
            if page.runs.is_empty() {
                return None;
            }
        }
        None
    }
}

fn status_value(value: &Value) -> Option<u16> {
    value
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| value.as_u64().and_then(|s| u16::try_from(s).ok()))
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
