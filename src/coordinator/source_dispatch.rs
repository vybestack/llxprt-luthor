//! Launch only recovered, attempt-free selections. Existing launch intents are
//! never automatically replayed; their reservations and operator controls stand.
use super::{
    ids::IdCreator,
    ports::{ScheduleError, SupervisorLauncher},
    reports::{ScheduleReport, SourceHold},
};
use crate::{
    config::Config,
    github::{project::ProjectReader, pull_request::PullRequestReader},
    state::{EffectiveConfigSnapshot, StateError, StateStore, scheduling, task_records},
    supervisor,
};

pub(crate) struct SourceDispatchDependencies<'a, P, Q, L, I> {
    pub config: &'a Config,
    pub projects: &'a mut P,
    pub prs: &'a mut Q,
    pub launcher: &'a mut L,
    pub ids: &'a mut I,
}

pub(crate) fn dispatch_reconciled_sources<P, Q, L, I>(
    store: &mut StateStore,
    report: &mut ScheduleReport,
    dependencies: SourceDispatchDependencies<'_, P, Q, L, I>,
) -> Result<(), ScheduleError>
where
    P: ProjectReader,
    Q: PullRequestReader,
    L: SupervisorLauncher,
    I: IdCreator,
{
    let tasks = scheduling::unresolved_sources(store)?
        .into_iter()
        .map(|(task, _)| task)
        .collect::<std::collections::BTreeSet<_>>();
    for task in tasks {
        if task_records::task_phase(store, &task)?.as_deref() != Some("claimed")
            || task_records::latest_attempt(store, &task)?.is_some()
        {
            continue;
        }
        let selection =
            task_records::selection_evidence(store, &task)?.ok_or(StateError::InvalidSelection)?;
        if selection.effective_config != EffectiveConfigSnapshot::from(dependencies.config) {
            task_records::hold_task(store, &task, "source recovery configuration changed")?;
            continue;
        }
        let owner = match crate::ownership::WorktreeOwner::acquire(store.root(), &task) {
            Ok(owner) => owner,
            Err(_) => {
                task_records::hold_task(store, &task, "source recovery ownership unavailable")?;
                continue;
            }
        };
        let verified = verify_source(&selection, dependencies.projects, dependencies.prs);
        if let Err(reason) = verified {
            task_records::hold_task(store, &task, reason)?;
            continue;
        }
        let attempt = format!("attempt-{}", dependencies.ids.create()?);
        match supervisor::prepare_initial_owned(store, &task, &attempt, &owner) {
            Ok(plan) => {
                dependencies
                    .launcher
                    .launch(store, &plan, &owner)
                    .map_err(super::ports::DispatchError::from)?;
                report.launched.push(plan);
            }
            Err(supervisor::SupervisorError::State(error)) => return Err(error.into()),
            Err(_) => task_records::hold_task(store, &task, "source recovery preparation held")?,
        }
    }
    report.startup.source_holds = scheduling::unresolved_sources(store)?
        .into_iter()
        .map(|(task_id, kind)| SourceHold { task_id, kind })
        .collect();
    Ok(())
}

fn verify_source<P: ProjectReader, Q: PullRequestReader>(
    selection: &crate::state::SelectionEvidence,
    projects: &mut P,
    prs: &mut Q,
) -> Result<(), &'static str> {
    let login = prs
        .authenticated_identity()
        .map_err(|_| "source recovery identity unavailable")?;
    if login != selection.candidate.mapping.allowed_pr_author {
        return Err("source recovery identity mismatch");
    }
    super::ports::verify_completion_claim(projects, selection)
        .map_err(|_| "source recovery claim unavailable")?;
    match crate::github::pull_request::lookup(
        prs,
        &selection.candidate.mapping.code_repository,
        &selection.candidate.issue_url,
    ) {
        Ok(crate::github::pull_request::LookupResult::Absent) => Ok(()),
        _ => Err("source recovery PR not absent"),
    }
}
