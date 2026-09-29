use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid JSON configuration: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub state_root: PathBuf,
    pub worktree_root: PathBuf,
    pub capacity: usize,
    pub sources: Vec<Source>,
    pub mappings: Vec<Mapping>,
    pub initial: CommandTemplate,
    pub resume: CommandTemplate,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub project_id: String,
    pub repositories: Vec<String>,
    pub ready_marker: Marker,
    pub milestone: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Marker {
    Label { name: String },
    ProjectField { name: String, value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    pub tracker_repository: String,
    pub code_repository: String,
    pub checkout: PathBuf,
    pub base_branch: String,
    pub push_remote: String,
    pub allowed_pr_head_repository: String,
    pub allowed_pr_author: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTemplate {
    pub executable: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskValues {
    pub task_issue_number: String,
    pub task_repository: String,
    pub task_issue_url: String,
    pub task_id: String,
    pub attempt_id: String,
    pub worktree: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedCommand {
    pub executable: PathBuf,
    pub args: Vec<String>,
}

impl CommandTemplate {
    pub fn render(&self, values: &TaskValues) -> Result<RenderedCommand, ConfigError> {
        let args = self
            .args
            .iter()
            .map(|arg| render_argument(arg, values))
            .collect::<Result<_, _>>()?;
        Ok(RenderedCommand {
            executable: self.executable.clone(),
            args,
        })
    }
}

fn render_argument(argument: &str, values: &TaskValues) -> Result<String, ConfigError> {
    let mut rendered = String::with_capacity(argument.len());
    let mut rest = argument;
    while let Some(start) = rest.find(['{', '}']) {
        rendered.push_str(&rest[..start]);
        if rest.as_bytes()[start] == b'}' {
            return Err(ConfigError::Invalid("unmatched template delimiter".into()));
        }
        let tail = &rest[start + 1..];
        let Some(end) = tail.find(['{', '}']) else {
            return Err(ConfigError::Invalid("unclosed template variable".into()));
        };
        if tail.as_bytes()[end] != b'}' {
            return Err(ConfigError::Invalid("nested template delimiter".into()));
        }
        let name = &tail[..end];
        let value = match name {
            "task.issue_number" => Some(&values.task_issue_number),
            "task.repository" => Some(&values.task_repository),
            "task.issue_url" => Some(&values.task_issue_url),
            "task.id" => Some(&values.task_id),
            "attempt.id" => Some(&values.attempt_id),
            "worktree" => Some(&values.worktree),
            _ => return Err(ConfigError::Invalid("unsupported template variable".into())),
        }
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ConfigError::Invalid("missing template value".into()))?;
        rendered.push_str(value);
        rest = &tail[end + 1..];
    }
    if rest.contains('}') {
        return Err(ConfigError::Invalid("unmatched template delimiter".into()));
    }
    rendered.push_str(rest);
    Ok(rendered)
}

impl Config {
    pub fn from_json(json: &str) -> Result<Self, ConfigError> {
        let config: Self = serde_json::from_str(json)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.capacity == 0 || self.sources.is_empty() || self.mappings.is_empty() {
            return Err(ConfigError::Invalid(
                "capacity, sources and mappings must be non-empty".into(),
            ));
        }
        let mapping_repositories: HashSet<_> = self
            .mappings
            .iter()
            .map(|mapping| mapping.tracker_repository.as_str())
            .collect();
        let mut project_ids = HashSet::new();
        for source in &self.sources {
            if source.project_id.trim().is_empty() || source.repositories.is_empty() {
                return Err(ConfigError::Invalid(
                    "source project_id and repositories are required".into(),
                ));
            }
            if !project_ids.insert(&source.project_id) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate source project_id: {}",
                    source.project_id
                )));
            }
            for repository in &source.repositories {
                validate_repository(repository)?;
                if !mapping_repositories.contains(repository.as_str()) {
                    return Err(ConfigError::Invalid(format!(
                        "source repository has no mapping: {repository}"
                    )));
                }
            }
            match &source.ready_marker {
                Marker::Label { name } if name.trim().is_empty() => {
                    return Err(ConfigError::Invalid("empty label marker".into()));
                }
                Marker::ProjectField { name, value }
                    if name.trim().is_empty() || value.trim().is_empty() =>
                {
                    return Err(ConfigError::Invalid(
                        "project marker name and value are required".into(),
                    ));
                }
                _ => {}
            }
            if source
                .milestone
                .as_ref()
                .is_some_and(|m| m.trim().is_empty())
            {
                return Err(ConfigError::Invalid("milestone cannot be empty".into()));
            }
        }
        let mut trackers = HashSet::new();
        let mut code_repositories = HashSet::new();
        for mapping in &self.mappings {
            validate_repository(&mapping.tracker_repository)?;
            validate_repository(&mapping.code_repository)?;
            if !trackers.insert(&mapping.tracker_repository) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate mapping for {}",
                    mapping.tracker_repository
                )));
            }
            if !code_repositories.insert(&mapping.code_repository) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate code repository mapping: {}",
                    mapping.code_repository
                )));
            }
            if mapping.checkout.as_os_str().is_empty() || mapping.base_branch.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "mapping checkout and base_branch are required".into(),
                ));
            }
            validate_push_remote(&mapping.push_remote)?;
            validate_repository(&mapping.allowed_pr_head_repository)?;
            if mapping.allowed_pr_author.trim().is_empty()
                || !mapping
                    .allowed_pr_author
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
            {
                return Err(ConfigError::Invalid("invalid allowed_pr_author".into()));
            }
            if mapping.allowed_pr_head_repository == mapping.tracker_repository
                || mapping.allowed_pr_head_repository == mapping.code_repository
            {
                return Err(ConfigError::Invalid("allowed PR head repository must be distinct from tracker and code repositories".into()));
            }
        }
        validate_command(&self.initial)?;
        validate_command(&self.resume)?;
        Ok(())
    }
}

fn is_sensitive_header(value: &str) -> bool {
    let name = value
        .split_once([':', '='])
        .map(|(name, _)| name.trim().to_ascii_lowercase());
    matches!(
        name.as_deref(),
        Some("authorization" | "x-api-key" | "cookie" | "proxy-authorization")
    )
}

fn has_sensitive_marker(value: &str) -> bool {
    let compact: String = value
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    [
        "apikey",
        "token",
        "authkey",
        "credential",
        "password",
        "secret",
    ]
    .iter()
    .any(|needle| compact.contains(needle))
}

fn contains_credential(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if is_sensitive_header(value) || lower.trim_start().starts_with("bearer ") {
        return true;
    }
    let compact: String = lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let sensitive = [
        "apikey",
        "token",
        "authkey",
        "credential",
        "password",
        "secret",
    ]
    .iter()
    .any(|needle| compact.contains(needle) || compact.starts_with(needle));
    sensitive && (value.starts_with('-') || value.contains('=') || lower.starts_with("bearer "))
}

fn validate_repository(value: &str) -> Result<(), ConfigError> {
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || !part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    {
        return Err(ConfigError::Invalid(format!(
            "invalid repository name: {value}"
        )));
    }
    Ok(())
}

fn validate_push_remote(value: &str) -> Result<(), ConfigError> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.@/:".contains(c))
        || value.contains("..")
        || value.starts_with('-')
    {
        return Err(ConfigError::Invalid("invalid push_remote".into()));
    }
    if let Some((scheme, authority_path)) = value.split_once("://")
        && (!matches!(scheme, "https" | "ssh")
            || authority_path.contains('@')
            || !authority_path.contains('/'))
    {
        return Err(ConfigError::Invalid("invalid push_remote URL".into()));
    }
    Ok(())
}

fn validate_command(command: &CommandTemplate) -> Result<(), ConfigError> {
    if command.executable.as_os_str().is_empty() {
        return Err(ConfigError::Invalid(
            "command executable is required".into(),
        ));
    }
    if has_sensitive_marker(&command.executable.to_string_lossy()) {
        return Err(ConfigError::Invalid(
            "credential-bearing command executable is forbidden".into(),
        ));
    }
    let mut previous_option = "";
    for arg in &command.args {
        if contains_credential(arg)
            || (matches!(previous_option, "--header" | "-H")
                && (is_sensitive_header(arg) || arg.to_ascii_lowercase().starts_with("bearer ")))
        {
            return Err(ConfigError::Invalid(
                "credential-bearing command argument is forbidden".into(),
            ));
        }
        previous_option = if arg.starts_with('-') {
            arg.as_str()
        } else {
            ""
        };
        if arg.contains("${")
            || arg.contains("$(")
            || arg.contains('`')
            || arg.contains(';')
            || arg.contains('|')
            || arg.contains('>')
            || arg.contains('<')
        {
            return Err(ConfigError::Invalid(
                "shell interpolation/operators are forbidden in argv templates".into(),
            ));
        }
        let mut rest = arg.as_str();
        while let Some(start) = rest.find(['{', '}']) {
            if rest.as_bytes()[start] == b'}' {
                return Err(ConfigError::Invalid("unmatched template delimiter".into()));
            }
            let tail = &rest[start + 1..];
            let Some(end) = tail.find(['{', '}']) else {
                return Err(ConfigError::Invalid("unclosed template variable".into()));
            };
            if tail.as_bytes()[end] != b'}' {
                return Err(ConfigError::Invalid("nested template delimiter".into()));
            }
            if !matches!(
                &tail[..end],
                "task.issue_number"
                    | "task.repository"
                    | "task.issue_url"
                    | "task.id"
                    | "attempt.id"
                    | "worktree"
            ) {
                return Err(ConfigError::Invalid("unsupported template variable".into()));
            }
            rest = &tail[end + 1..];
        }
        if rest.contains('}') {
            return Err(ConfigError::Invalid("unmatched template delimiter".into()));
        }
    }
    Ok(())
}
