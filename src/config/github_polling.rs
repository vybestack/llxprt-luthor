//! Optional GitHub polling policy; dispatch and write authorization never use it.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubPolling {
    pub reserve: u64,
    pub interval_seconds: u64,
}

impl Default for GithubPolling {
    fn default() -> Self {
        Self {
            reserve: 500,
            interval_seconds: 300,
        }
    }
}
