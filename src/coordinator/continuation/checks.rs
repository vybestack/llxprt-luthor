use super::ports::{
    ContinuationLocalInspector, ContinuationProcessInspector, ContinuationRefusal,
    ContinuationResult, ProcessInspectionError,
};
use crate::state::journal;
use crate::{
    claim,
    github::{
        project::ProjectReader,
        pull_request::{LookupResult, PullRequestReader, lookup},
    },
    state::{NeverDispatchedContext, StateError, StateStore},
};

pub(crate) fn verify_local(
    context: &NeverDispatchedContext,
    local: &mut impl ContinuationLocalInspector,
    processes: &mut impl ContinuationProcessInspector,
    owner: &crate::ownership::WorktreeOwner,
) -> Result<(), ContinuationRefusal> {
    if local.session_environment()? != context.plan().session_environment {
        return Err(ContinuationRefusal::EnvironmentChanged);
    }
    local.inspect(context)?;
    processes
        .inspect(context, owner)
        .map_err(|error| match error {
            ProcessInspectionError::Conflict => ContinuationRefusal::ProcessConflict,
            ProcessInspectionError::Unavailable => ContinuationRefusal::ProcessUnavailable,
        })
}

pub(crate) fn verify_source(
    context: &NeverDispatchedContext,
    projects: &mut impl ProjectReader,
) -> Result<(), ContinuationRefusal> {
    let (_, issue) =
        claim::fresh(projects, &context.selection().candidate).map_err(|error| match error {
            claim::ClaimError::Changed => ContinuationRefusal::ClaimChanged,
            _ => ContinuationRefusal::SourceUnavailable,
        })?;
    if issue.assignees != [context.claim().principal.as_str()] {
        return Err(ContinuationRefusal::ClaimChanged);
    }
    Ok(())
}

pub(crate) fn verify_pr(
    context: &NeverDispatchedContext,
    actor: &str,
    prs: &mut impl PullRequestReader,
) -> Result<(), ContinuationRefusal> {
    let mapping = &context.selection().candidate.mapping;
    if actor != mapping.allowed_pr_author || actor.is_empty() || actor.len() > 128 {
        return Err(ContinuationRefusal::ActorMismatch);
    }
    if prs
        .authenticated_identity()
        .map_err(|_| ContinuationRefusal::IdentityUnavailable)?
        != actor
    {
        return Err(ContinuationRefusal::ActorMismatch);
    }
    match lookup(
        prs,
        &mapping.code_repository,
        &context.selection().candidate.issue_url,
    ) {
        Ok(LookupResult::Absent) => Ok(()),
        Ok(_) => Err(ContinuationRefusal::PrPresent),
        Err(_) => Err(ContinuationRefusal::PrUnavailable),
    }
}

pub(crate) fn refuse(
    store: &mut StateStore,
    context: &NeverDispatchedContext,
    reason: ContinuationRefusal,
) -> Result<ContinuationResult, StateError> {
    // A fixed enum, never external output, actor text, or credentials. This
    // task-scoped annotation does not replace history or release its slot.
    journal::record_evidence(
        store,
        context.task_id(),
        None,
        "held_reason",
        &serde_json::to_string(&reason)?,
    )?;
    Ok(ContinuationResult::Held(reason))
}
