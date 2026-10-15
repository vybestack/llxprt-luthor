use crate::{
    claim,
    coordinator::{DispatchError, retry::validate_retry_config},
    github::{
        identity::verify_authenticated_account,
        project::GhProjectReader,
        pull_request::{GhPullRequestReader, LookupResult, lookup},
    },
    state::{
        request_context,
        requests::{self, Request, RequestKind},
    },
};
use std::path::Path;

type Error = Box<dyn std::error::Error>;

/// Read a coherent snapshot without acquiring the coordinator lock. Permanent
/// refusals visible now are returned immediately, even when capacity is full.
/// The daemon repeats all checks; this snapshot is never execution authority.
pub fn enqueue(mut request: Request, gh: &Path, wait_reason: &str) -> Result<String, Error> {
    let (previous, selection) = request_context::read(
        &request.config.state_root,
        &request.task_id,
        &request.kind,
        &request.previous_attempt_id,
    )
    .map_err(
        |_| "request refused: task phase, latest attempt or verified exit precondition failed",
    )?;
    verify_authenticated_account(gh, &selection.candidate)?;
    if let RequestKind::Retry {
        revision,
        actor,
        reason,
        ..
    } = &request.kind
    {
        validate_retry_config(
            &request.config.state_root,
            &previous,
            &selection,
            &request.config,
            revision,
            actor,
            reason,
        )
        .map_err(|error| {
            format!(
                "request refused: {}",
                crate::coordinator::operator_error(&error)
            )
        })?;
    }
    let mut projects = GhProjectReader::new(gh.to_path_buf());
    let (_, issue) = claim::fresh(&mut projects, &selection.candidate).map_err(|error| {
        format!(
            "request refused: {}",
            crate::coordinator::operator_error(&DispatchError::Claim(error))
        )
    })?;
    if issue.assignees != [selection.effective_config.assignment_login.as_str()] {
        return Err("request refused: claim drift; assignment changed".into());
    }
    let mut prs = GhPullRequestReader::new(gh.to_path_buf());
    if lookup(
        &mut prs,
        &selection.candidate.mapping.code_repository,
        &selection.candidate.issue_url,
    )? != LookupResult::Absent
    {
        return Err("request refused: PR present or ambiguous; absent PR required".into());
    }
    request.previous_attempt_id = previous.attempt_id;
    requests::enqueue(&request, wait_reason).map_err(|error| match error {
        crate::state::StateError::LaunchBlocked => {
            "request refused: a different request is already pending for this attempt".into()
        }
        _ => "operator request storage failed; request not acknowledged".into(),
    })
}
