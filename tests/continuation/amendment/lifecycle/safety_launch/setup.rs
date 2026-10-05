pub(super) mod script;
use super::super::{Lane, database, executable_lane};
use luthor::{
    state::NeverDispatchedReason,
    supervisor::{self, LaunchPlan},
};
use std::path::Path;

pub(crate) fn lane(amended: bool) -> Lane {
    let mut lane = executable_lane();
    if !amended {
        database(&lane).execute_batch("DELETE FROM evidence WHERE kind='initial_branch_removed'; DELETE FROM intents WHERE kind='initial_branch_removal_seal';").unwrap();
        lane.f
            .config
            .initial
            .args
            .splice(2..2, ["--branch".into(), "luthor/{task.id}".into()]);
        let context = lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap();
        context
            .authorize(
                &mut lane.f.store,
                "bot",
                NeverDispatchedReason::LegacyPreflightRecovery,
            )
            .unwrap();
    }
    lane
}

pub(super) fn commit(lane: &mut Lane, amended: bool) -> LaunchPlan {
    if amended {
        let context = lane
            .f
            .store
            .never_dispatched_context("task-a", "attempt-task-a")
            .unwrap();
        lane.f
            .store
            .begin_amended_supervision(&context, &lane.f.config, "corrected-revision")
            .unwrap()
            .effective_plan
    } else {
        let saved = lane
            .f
            .store
            .launch_intent("attempt-task-a")
            .unwrap()
            .unwrap();
        lane.f
            .store
            .begin_supervision("task-a", "attempt-task-a", &saved)
            .unwrap();
        lane.plan.clone()
    }
}

pub(super) fn launch(
    lane: &mut Lane,
    amended: bool,
    binary: &Path,
) -> Result<(), supervisor::SupervisorError> {
    if amended {
        super::super::launch(lane, binary)
    } else {
        supervisor::execute_with_binary(&mut lane.f.store, &lane.plan, binary)
    }
}

pub(super) fn drift(plan: &LaunchPlan, change: &str) {
    match change {
        "tracked" => std::fs::write(plan.worktree.join("README"), "late drift").unwrap(),
        "untracked" => std::fs::write(plan.worktree.join("late-untracked"), "late drift").unwrap(),
        "descendant" => crate::git(
            &plan.worktree,
            &["commit", "--allow-empty", "-m", "late drift"],
        ),
        _ => unreachable!(),
    }
}
