//! Opt-in task-attempt retry policy, separate from provider request replay.
use super::error::ConfigError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransientFailure {
    RateLimit,
    TransientServer,
    Connectivity,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetryPolicy {
    Disabled {},
    Bounded {
        /// Total attempts, including initial, manual retries, and resumes.
        max_attempts: u32,
        /// Minimum delay after the terminal outcome is first durably observed.
        backoff_secs: u32,
        classes: Vec<TransientFailure>,
    },
}

impl RetryPolicy {
    pub fn validate(&self) -> Result<(), ConfigError> {
        let Self::Bounded {
            max_attempts,
            backoff_secs,
            classes,
        } = self
        else {
            return Ok(());
        };
        let unique: HashSet<_> = classes.iter().collect();
        if *max_attempts < 2
            || *backoff_secs == 0
            || classes.is_empty()
            || unique.len() != classes.len()
        {
            return Err(ConfigError::Invalid(
                "retry policy requires at least two total attempts, positive backoff, and distinct transient classes".into(),
            ));
        }
        Ok(())
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::Disabled {}
    }
}
