//! Shared gate-selection semantics for an immutable PR-head observation.
use super::health::RequiredChecksSource;
use crate::config::{CheckRule, RequiredChecks};
use serde_json::Value;

pub(crate) fn source(checks: &RequiredChecks) -> RequiredChecksSource {
    match checks {
        RequiredChecks::Names(_) => RequiredChecksSource::ConfiguredList,
        RequiredChecks::Rule(CheckRule::All {}) => RequiredChecksSource::ConfiguredAll,
        RequiredChecks::Rule(CheckRule::Github {}) => RequiredChecksSource::None,
    }
}

pub(crate) fn selected(
    checks: &RequiredChecks,
    name: &str,
    node: &Value,
    github_required: bool,
) -> bool {
    match checks {
        RequiredChecks::Names(names) => names.iter().any(|configured| configured == name),
        RequiredChecks::Rule(CheckRule::All {}) => node["conclusion"] != "SKIPPED",
        RequiredChecks::Rule(CheckRule::Github {}) => github_required,
    }
}

// A named gate that has not appeared is pending, never an implicit success.
pub(crate) fn missing(checks: &RequiredChecks, selected_names: &[&str]) -> bool {
    match checks {
        RequiredChecks::Names(names) => names
            .iter()
            .any(|name| !selected_names.contains(&name.as_str())),
        RequiredChecks::Rule(_) => false,
    }
}
