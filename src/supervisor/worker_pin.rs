use super::SupervisorError;
use crate::{
    model::LaunchPlan,
    state::{StateStore, task_records},
    worker_build::WorkerBuild,
};
use std::path::Path;

pub(crate) fn verify(plan: &LaunchPlan) -> Result<(), SupervisorError> {
    let build = plan.worker_build.as_ref().ok_or_else(|| {
        SupervisorError::WorkerBuildHeld(
            "legacy session has no recorded build hash; explicit migration required".into(),
        )
    })?;
    build.verify().map_err(|error| {
        SupervisorError::WorkerBuildHeld(format!(
            "pinned {}; executable missing or changed ({error}); no replacement authorized",
            build.label()
        ))
    })
}

/// Templates can change argv, never silently change the build reading a session.
pub(crate) fn continuation(
    store: &mut StateStore,
    first: &LaunchPlan,
    latest: &LaunchPlan,
    template: &Path,
) -> Result<WorkerBuild, SupervisorError> {
    let result = continuation_build(first, latest, template);
    if let Err(SupervisorError::WorkerBuildHeld(reason)) = &result {
        task_records::hold_task(store, &first.task_id, reason)?;
    }
    result
}

fn continuation_build(
    first: &LaunchPlan,
    latest: &LaunchPlan,
    template: &Path,
) -> Result<WorkerBuild, SupervisorError> {
    if first.worker_build != latest.worker_build {
        return Err(SupervisorError::Conflict);
    }
    let Some(build) = first.worker_build.as_ref() else {
        return Err(SupervisorError::WorkerBuildHeld(format!(
            "legacy session worker {}; hash unrecorded; requested template {}; explicitly migrate the session before continuation",
            first.executable.display(),
            template.display()
        )));
    };
    if let Err(error) = build.verify() {
        return Err(SupervisorError::WorkerBuildHeld(format!(
            "pinned {}; unavailable or changed ({error}); requested template {}; restore the pinned build or explicitly migrate the session",
            build.label(),
            template.display()
        )));
    }
    Ok(build.clone())
}
