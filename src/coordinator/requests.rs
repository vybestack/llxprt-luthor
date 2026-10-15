//! Operator inbox admission and safe-point execution. A queued decision is not
//! launch authorization: every live precondition is checked again when drained.
mod admission;
mod outcome;
use super::{DispatchError, ResumeDependencies, RetryDependencies, SupervisorLauncher};
use crate::{
    config::Config,
    github::{
        identity::verify_authenticated_account, project::ProjectReader,
        pull_request::PullRequestReader,
    },
    state::{
        StateError, StateStore,
        requests::{self, Request, RequestKind},
        task_records,
    },
};
pub use admission::enqueue;
use std::path::Path;

/// Drain before automatic retries, repairs and new selections so operator
/// decisions are not starved by freshly discovered work. The caller holds the
/// coordinator lock and has completed startup reconciliation.
pub fn drain<P: ProjectReader, Q: PullRequestReader, L: SupervisorLauncher>(
    store: &mut StateStore,
    config: &Config,
    gh: &Path,
    projects: &mut P,
    prs: &mut Q,
    launcher: &mut L,
) -> Result<Vec<crate::supervisor::LaunchPlan>, StateError> {
    let mut launched = Vec::new();
    for request in requests::pending(store.root())? {
        if request.config.state_root != store.root() || request.config.capacity != config.capacity {
            requests::update(
                store.root(),
                &request.attempt_id,
                "refused",
                "request configuration does not match coordinator",
            )?;
            continue;
        }
        // A crash after recording a plan but before updating the inbox must not
        // launch it again, even if the first dispatch's outcome is uncertain.
        if task_records::has_attempt(store, &request.task_id, &request.attempt_id)? {
            requests::update(
                store.root(),
                &request.attempt_id,
                "recorded",
                "attempt already recorded; inspect task reconciliation",
            )?;
            continue;
        }
        let result = apply(store, &request, gh, projects, prs, launcher);
        let (status, reason) = match result {
            Ok(ApplyOutcome::Launched(plan)) => {
                launched.push(*plan);
                ("applied", "worker launch authorized".into())
            }
            Ok(ApplyOutcome::IdentityReadDeferred) => (
                "pending",
                "GitHub account verification temporarily unavailable".into(),
            ),
            Err(error) => outcome::failure(error)?,
        };
        requests::update(store.root(), &request.attempt_id, status, &reason)?;
    }
    Ok(launched)
}

enum ApplyOutcome {
    Launched(Box<crate::supervisor::LaunchPlan>),
    IdentityReadDeferred,
}

fn apply<P: ProjectReader, Q: PullRequestReader, L: SupervisorLauncher>(
    store: &mut StateStore,
    request: &Request,
    gh: &Path,
    projects: &mut P,
    prs: &mut Q,
    launcher: &mut L,
) -> Result<ApplyOutcome, DispatchError> {
    if task_records::latest_attempt(store, &request.task_id)?.as_deref()
        != Some(&request.previous_attempt_id)
    {
        return Err(DispatchError::RetryHeld {
            reason: "latest attempt changed; request is stale".into(),
        });
    }
    let selection = task_records::selection_evidence(store, &request.task_id)?
        .ok_or(StateError::InvalidSelection)?;
    match verify_authenticated_account(gh, &selection.candidate) {
        Ok(()) => {}
        Err(crate::github::identity::IdentityError::Transient) => {
            return Ok(ApplyOutcome::IdentityReadDeferred);
        }
        Err(_) => return Err(DispatchError::RetryHeld {
            reason:
                "authenticated GitHub identity is not the authorized PR author or is unavailable"
                    .into(),
        }),
    }
    match &request.kind {
        RequestKind::Resume => super::resume_pending(
            store,
            ResumeDependencies {
                task_id: &request.task_id,
                attempt_id: &request.attempt_id,
                projects,
                prs,
                launcher,
            },
        )
        .map(|plan| ApplyOutcome::Launched(Box::new(plan))),
        RequestKind::Retry {
            revision,
            actor,
            reason,
            revalidate_terminal_exit,
        } => super::retry_one(
            store,
            RetryDependencies {
                task_id: &request.task_id,
                previous_attempt_id: &request.previous_attempt_id,
                attempt_id: &request.attempt_id,
                config: &request.config,
                config_revision: revision,
                actor,
                reason,
                revalidate_terminal_exit: *revalidate_terminal_exit,
                projects,
                prs,
                launcher,
            },
        )
        .map(|plan| ApplyOutcome::Launched(Box::new(plan))),
    }
}
