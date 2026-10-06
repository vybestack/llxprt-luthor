mod continuation;
mod preflight;

mod coordinator {
    pub(crate) mod dispatch_scenarios;
    mod fakes;
    mod fixture;
    pub(crate) mod resume_scenarios;
    pub(crate) mod scheduling_scenarios;

    pub(crate) use fakes::{FakeGithub, FakeLauncher, FakePr, FakeWriter, FixedIds};
    pub(crate) use fixture::{Fixture, git};
}

use coordinator::{FakeGithub, FakeLauncher, FakePr, FakeWriter, FixedIds, Fixture, git};
use luthor::{
    coordinator::{DispatchError, ScheduleDependencies, schedule_candidates},
    supervisor::SupervisorError,
};
use std::fs;

#[test]
fn verified_claim_and_worktree_precede_fake_launch() {
    coordinator::dispatch_scenarios::verified_claim_and_worktree_precede_fake_launch();
}

#[test]
fn existing_pr_blocks_assignment_and_holds_selection() {
    coordinator::dispatch_scenarios::existing_pr_blocks_assignment_and_holds_selection();
}

#[test]
fn changed_claim_blocks_before_worktree_or_launch() {
    coordinator::dispatch_scenarios::changed_claim_blocks_before_worktree_or_launch();
}

#[test]
fn new_pr_or_failed_lookup_blocks_before_reservation() {
    coordinator::dispatch_scenarios::new_pr_or_failed_lookup_blocks_before_reservation();
}

#[test]
fn failed_launch_retains_reservation_and_second_schedule_cannot_launch() {
    coordinator::dispatch_scenarios::failed_launch_retains_reservation_and_second_schedule_cannot_launch();
}

#[test]
fn distinct_tasks_keep_selection_and_repository_issue_uniqueness() {
    coordinator::dispatch_scenarios::distinct_tasks_keep_selection_and_repository_issue_uniqueness(
    );
}

#[test]
fn paused_resume_uses_stored_selection_and_launches_one_continuation() {
    coordinator::resume_scenarios::paused_resume_uses_stored_selection_and_launches_one_continuation();
}

#[test]
fn resume_pr_present_or_failed_lookup_holds_without_new_attempt() {
    coordinator::resume_scenarios::resume_pr_present_or_failed_lookup_holds_without_new_attempt();
}

#[test]
fn resume_changed_claim_holds_without_new_attempt() {
    coordinator::resume_scenarios::resume_changed_claim_holds_without_new_attempt();
}

#[test]
fn resume_without_paused_reconciled_state_cannot_create_attempt() {
    coordinator::resume_scenarios::resume_without_paused_reconciled_state_cannot_create_attempt();
}

#[test]
fn failed_resume_dispatch_retains_reservation_and_never_retries() {
    coordinator::resume_scenarios::failed_resume_dispatch_retains_reservation_and_never_retries();
}

#[test]
fn resume_refuses_missing_owner_protocol_rows_without_state_changes() {
    coordinator::resume_scenarios::resume_refuses_missing_owner_protocol_rows_without_state_changes(
    );
}

#[test]
fn resume_refuses_replacement_inode_without_creating_an_attempt() {
    coordinator::resume_scenarios::resume_refuses_replacement_inode_without_creating_an_attempt();
}

#[test]
fn busy_owner_refuses_resume_without_mutation_then_same_task_resumes() {
    coordinator::resume_scenarios::busy_owner_refuses_resume_without_mutation_then_same_task_resumes();
}

#[test]
fn resume_requires_existing_owner_and_refuses_busy_before_remote_reads() {
    coordinator::resume_scenarios::resume_requires_existing_owner_and_refuses_busy_before_remote_reads();
}

#[test]
fn startup_reconcile_holds_missing_receipt_without_relaunching() {
    coordinator::scheduling_scenarios::startup_reconcile_holds_missing_receipt_without_relaunching(
    );
}

#[test]
fn unproven_stopped_attempt_never_reads_pr_or_releases_task() {
    coordinator::scheduling_scenarios::unproven_stopped_attempt_never_reads_pr_or_releases_task();
}

#[test]
fn scheduler_dispatches_other_task_after_verified_pause_without_resuming_paused_task() {
    coordinator::scheduling_scenarios::scheduler_dispatches_other_task_after_verified_pause_without_resuming_paused_task();
}

#[test]
fn scheduler_uses_precomputed_startup_without_reconciling_again() {
    coordinator::scheduling_scenarios::scheduler_uses_precomputed_startup_without_reconciling_again(
    );
}

#[test]
fn scheduler_with_precomputed_blocked_startup_does_not_select_candidates() {
    coordinator::scheduling_scenarios::scheduler_with_precomputed_blocked_startup_does_not_select_candidates();
}

#[test]
fn occupied_owner_prevents_dispatch_before_persisting_or_claiming_task() {
    let mut f = Fixture::new(1);
    let c = f.candidate.clone();
    let owner = luthor::WorktreeOwner::acquire(f.store.root(), "task-a").unwrap();
    let mut github = FakeGithub::new(&c);
    let mut writer = FakeWriter::default();
    let mut launcher = FakeLauncher::default();

    assert!(matches!(
        f.run("task-a", &c, &mut github, &mut writer, &mut launcher),
        Err(DispatchError::Supervisor(SupervisorError::Conflict))
    ));

    let connection = rusqlite::Connection::open(f.config.state_root.join("state.sqlite3")).unwrap();
    for table in ["tasks", "attempts", "reservations", "evidence", "intents"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "unexpected rows in {table}");
    }
    assert_eq!(writer.calls, 0);
    assert_eq!(github.reads, 0);
    assert_eq!(github.prs.lookups, 0);
    assert!(launcher.plans.is_empty());
    drop(owner);
}
