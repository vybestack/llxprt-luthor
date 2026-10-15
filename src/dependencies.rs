//! Configured ordering uses fresh GitHub evidence, never local task completion.
use crate::{
    config::DependencyRule,
    github::project::{ProjectReadError, ProjectReader},
};
pub mod types;
pub use types::{DependencyState, DependencyWait};

pub fn waiting<R: ProjectReader>(
    reader: &mut R,
    rules: &[DependencyRule],
    repository: &str,
    issue_number: u64,
) -> Result<Vec<DependencyWait>, ProjectReadError> {
    let mut waiting = Vec::new();
    for rule in rules {
        if rule.issue.repository != repository || rule.issue.number != issue_number {
            continue;
        }
        for dependency in &rule.blocked_by {
            let state = reader.dependency(dependency)?;
            if state == DependencyState::Open
                || state == DependencyState::ClosedUnmergedPr && !rule.allow_closed_unmerged_prs
            {
                waiting.push(DependencyWait {
                    dependency: dependency.clone(),
                    state,
                });
            }
        }
    }
    Ok(waiting)
}
