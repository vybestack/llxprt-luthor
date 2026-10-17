pub mod claim;
pub mod cleanup;
mod cleanup_error;
pub mod cli;
pub mod config;
mod continuation_plan;
pub mod coordinator;
pub mod daemon;
pub mod dependencies;
pub mod disk;
pub mod eligibility;
pub mod native_transition;
pub mod platform;
pub mod pr_evidence;
pub mod github {
    pub mod budget;
    pub mod identity;
    pub mod project;
    pub mod pull_request;
    pub(crate) mod quota;
    pub(crate) mod read;
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

pub mod pr_merge;
pub mod pr_repair;

pub mod worker_build;

pub mod worker_capability;
