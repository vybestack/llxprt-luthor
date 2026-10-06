pub mod claim;
pub mod cli;
pub mod config;
pub mod coordinator;
pub mod daemon;
pub mod eligibility;
pub mod platform;
pub mod pr_evidence;
pub mod github {
    pub mod identity;
    pub mod project;
    pub mod pull_request;
}
pub mod state;
pub mod supervisor;
pub mod worktree;

pub(crate) mod model;

mod worker_instructions;

mod cli_identifiers;
mod launch_command;
#[cfg(unix)]
mod ownership;
#[cfg(unix)]
pub use ownership::{OwnershipError, WorktreeOwner, WorktreeOwnerProtocol};
