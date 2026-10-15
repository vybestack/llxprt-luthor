use super::{error::ConfigError, repository::validate_repository};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// An issue or PR, including prerequisites outside configured tracker repositories.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IssueReference {
    pub repository: String,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyRule {
    pub issue: IssueReference,
    pub blocked_by: Vec<IssueReference>,
    #[serde(default)]
    pub allow_closed_unmerged_prs: bool,
}

pub(crate) fn validate<'a>(
    sources: impl Iterator<Item = (&'a [String], &'a [DependencyRule])>,
) -> Result<(), ConfigError> {
    let sources: Vec<_> = sources.collect();
    let mut rules = HashMap::new();
    for &(repositories, dependencies) in &sources {
        let mut targets = HashSet::new();
        for rule in dependencies {
            validate_rule(rule)?;
            if !repositories.contains(&rule.issue.repository) || !targets.insert(&rule.issue) {
                return Err(invalid());
            }
            if let Some(prior) = rules.insert(&rule.issue, rule)
                && prior != rule
            {
                return Err(invalid());
            }
        }
    }
    for (repositories, dependencies) in &sources {
        for rule in rules.values() {
            if repositories.contains(&rule.issue.repository) && !dependencies.contains(rule) {
                return Err(invalid());
            }
        }
    }
    // Remove nodes whose configured prerequisites are already resolved. Remaining
    // nodes form a cycle, including cycles spanning projects/repositories.
    let mut remaining: HashSet<_> = rules.keys().copied().collect();
    while !remaining.is_empty() {
        let resolved: Vec<_> = remaining
            .iter()
            .copied()
            .filter(|issue| {
                rules[issue]
                    .blocked_by
                    .iter()
                    .all(|dependency| !remaining.contains(dependency))
            })
            .collect();
        if resolved.is_empty() {
            return Err(ConfigError::Invalid("cyclic issue dependencies".into()));
        }
        for issue in resolved {
            remaining.remove(issue);
        }
    }
    Ok(())
}

fn validate_rule(rule: &DependencyRule) -> Result<(), ConfigError> {
    validate_reference(&rule.issue)?;
    if rule.blocked_by.is_empty() {
        return Err(invalid());
    }
    let mut prerequisites = HashSet::new();
    for dependency in &rule.blocked_by {
        validate_reference(dependency)?;
        if dependency == &rule.issue || !prerequisites.insert(dependency) {
            return Err(invalid());
        }
    }
    Ok(())
}

fn validate_reference(reference: &IssueReference) -> Result<(), ConfigError> {
    validate_repository(&reference.repository)?;
    if reference.number == 0
        || reference
            .repository
            .split('/')
            .any(|part| matches!(part, "." | ".."))
    {
        return Err(invalid());
    }
    Ok(())
}

fn invalid() -> ConfigError {
    ConfigError::Invalid("invalid or conflicting issue dependency rule".into())
}
