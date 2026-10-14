use super::{
    SupervisorError,
    dispatch_abort::{self, DispatchAbort},
    processes::private_bytes,
};
use crate::{
    model::{ChildIdentity, LaunchPlan},
    ownership::{WorktreeOwner, WorktreeOwnerInternal},
    state::StateStore,
};
use rusqlite::params;
use std::path::Path;

/// The marker is not permission by itself. Require exact plan, exclusive owner,
/// absent unreleased processes and no evidence of a worker release or execution.
pub(crate) fn inspect(
    store: &StateStore,
    attempts: &Path,
    plan: &LaunchPlan,
    owner: &WorktreeOwner,
) -> Result<bool, SupervisorError> {
    let Some(abort) = dispatch_abort::read(attempts, &plan.attempt_id) else {
        return Ok(false);
    };
    if abort.plan != *plan
        || !dispatch_abort::processes_absent(&abort)
        || !child_matches(attempts, &abort)
        || owner
            .verify_protocol(
                &store.connection,
                store.root(),
                &plan.task_id,
                &plan.attempt_id,
            )
            .is_err()
        || !unreleased(&store.connection, &plan.task_id, &plan.attempt_id)?
    {
        return Ok(false);
    }
    for suffix in ["receipt.json", "supervisor-error.json"] {
        if !dispatch_abort::path_absent(&attempts.join(format!("{}.{suffix}", plan.attempt_id))) {
            return Ok(false);
        }
    }
    let current_plan = private_bytes(&attempts.join(format!("{}.plan.json", plan.attempt_id)))
        .and_then(|bytes| serde_json::from_slice::<LaunchPlan>(&bytes).ok());
    Ok(current_plan.as_ref() == Some(plan)
        && dispatch_abort::read(attempts, &plan.attempt_id).as_ref() == Some(&abort)
        && dispatch_abort::processes_absent(&abort))
}

fn child_matches(attempts: &Path, abort: &DispatchAbort) -> bool {
    let path = attempts.join(format!("{}.child.json", abort.plan.attempt_id));
    match &abort.child {
        None => dispatch_abort::path_absent(&path),
        Some(child) => {
            private_bytes(&path)
                .and_then(|bytes| serde_json::from_slice::<ChildIdentity>(&bytes).ok())
                .as_ref()
                == Some(child)
        }
    }
}

pub(crate) fn unreleased(
    db: &rusqlite::Connection,
    task: &str,
    attempt: &str,
) -> Result<bool, rusqlite::Error> {
    db.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM intents WHERE attempt_id=?2 AND kind IN ('gate_release','stop'))
         AND NOT EXISTS(SELECT 1 FROM evidence WHERE attempt_id=?2 AND kind IN
             ('gate_sent','supervisor_ready','child_registered','tracked_descendant','attempt_exit',
              'log_failure','telemetry_lost','exit_pr_lookup','pause_pr_lookup','verified_open_pr'))
         AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?2 AND task_id=?1 AND kind='launch')=1
         AND (SELECT COUNT(*) FROM intents WHERE attempt_id=?2 AND task_id=?1 AND kind='supervisor_dispatch')=1",
        params![task, attempt], |row| row.get(0),
    )
}
