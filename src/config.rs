use serde::Deserialize;
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub project_id: String,
    pub repositories: Vec<String>,
    pub ready_marker: Marker,
    pub milestone: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Marker {
    Label { name: String },
    ProjectField { name: String, value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    pub tracker_repository: String,
    pub code_repository: String,
    pub checkout: PathBuf,
    pub base_branch: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandTemplate {
    pub executable: PathBuf,
    pub args: Vec<String>,
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
        for source in &self.sources {
            if source.project_id.trim().is_empty() || source.repositories.is_empty() {
                return Err(ConfigError::Invalid(
                    "source project_id and repositories are required".into(),
                ));
            }
            for repository in &source.repositories {
                validate_repository(repository)?;
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
        for mapping in &self.mappings {
            validate_repository(&mapping.tracker_repository)?;
            validate_repository(&mapping.code_repository)?;
            if !trackers.insert(&mapping.tracker_repository) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate mapping for {}",
                    mapping.tracker_repository
                )));
            }
            if mapping.checkout.as_os_str().is_empty() || mapping.base_branch.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "mapping checkout and base_branch are required".into(),
                ));
            }
        }
        validate_command(&self.initial)?;
        validate_command(&self.resume)?;
        Ok(())
    }
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

fn validate_command(command: &CommandTemplate) -> Result<(), ConfigError> {
    if command.executable.as_os_str().is_empty() {
        return Err(ConfigError::Invalid(
            "command executable is required".into(),
        ));
    }
    for arg in &command.args {
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
        while let Some(start) = rest.find('{') {
            let tail = &rest[start + 1..];
            let Some(end) = tail.find('}') else {
                return Err(ConfigError::Invalid("unclosed template variable".into()));
            };
            if !matches!(
                &tail[..end],
                "task.issue_number"
                    | "task.repository"
                    | "task.issue_url"
                    | "task.id"
                    | "attempt.id"
                    | "worktree"
            ) {
                return Err(ConfigError::Invalid(format!(
                    "unsupported template variable {{{}}}",
                    &tail[..end]
                )));
            }
            rest = &tail[end + 1..];
        }
        if rest.contains('}') {
            return Err(ConfigError::Invalid("unmatched template delimiter".into()));
        }
    }
    Ok(())
}
