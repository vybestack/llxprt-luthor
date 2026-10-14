use super::{CheckRule, ConfigError, RequiredChecks};
use serde::{Deserialize, Serialize};

/// Current mapping-local authorization, independent of worker launch policy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MergePolicy {
    Disabled {},
    Enabled {
        method: MergeMethod,
        #[serde(default = "all_checks")]
        required_checks: RequiredChecks,
        #[serde(default)]
        allow_draft: bool,
    },
}

impl MergePolicy {
    pub fn disabled() -> Self {
        Self::Disabled {}
    }
}

fn all_checks() -> RequiredChecks {
    RequiredChecks::Rule(CheckRule::All {})
}

impl MergePolicy {
    pub fn is_disabled(&self) -> bool {
        *self == Self::Disabled {}
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Self::Enabled {
            required_checks, ..
        } = self
        {
            required_checks
                .validate()
                .map_err(|reason| ConfigError::Invalid(reason.into()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }
}
