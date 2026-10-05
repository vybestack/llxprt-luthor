use luthor::state::StateStore;

mod state {
    use luthor::{
        config::Marker,
        state::{StateError, StateStore},
    };
    use rusqlite::Connection;
    use support::{candidate, config};

    pub(crate) mod migrations;
    pub(crate) mod persistence;
    pub(crate) mod recovery;
    pub(crate) mod selection;
    mod support;
}

#[test]
fn coordinator_lock_is_exclusive_across_processes_and_scoped_to_root() {
    use std::process::Command;
    let held = tempfile::tempdir().unwrap();
    let independent = tempfile::tempdir().unwrap();
    let store = StateStore::open(held.path(), 1).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("coordinator_child_cannot_open_held_root")
        .arg("--nocapture")
        .env("LUTHOR_LOCK_CHILD_ROOT", held.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(StateStore::open(independent.path(), 1).is_ok());
    drop(store);
}

#[test]
fn coordinator_child_cannot_open_held_root() {
    if let Ok(root) = std::env::var("LUTHOR_LOCK_CHILD_ROOT") {
        assert!(StateStore::open(root, 1).is_err());
    }
}

#[test]
fn serializes_coordinator_for_state_directory() {
    let dir = tempfile::tempdir().unwrap();
    let first = StateStore::open(dir.path(), 2).unwrap();
    assert!(StateStore::open(dir.path(), 2).is_err());
    drop(first);
    assert!(StateStore::open(dir.path(), 2).is_ok());
}

#[test]
fn evidence_payloads_are_ordered_task_scoped_and_durable() {
    state::persistence::evidence_payloads_are_ordered_task_scoped_and_durable();
}

#[test]
fn migrates_v2_released_reservation_and_preserves_attempt_history() {
    state::migrations::migrates_v2_released_reservation_and_preserves_attempt_history();
}

#[test]
fn migrates_v1_database_atomically_and_preserves_records_and_capacity() {
    state::migrations::migrates_v1_database_atomically_and_preserves_records_and_capacity();
}

#[test]
fn failed_v1_migration_rolls_back_schema_and_version() {
    state::migrations::failed_v1_migration_rolls_back_schema_and_version();
}

#[test]
fn existing_target_matches_only_one_persisted_target_and_rejects_bad_input() {
    state::selection::existing_target_matches_only_one_persisted_target_and_rejects_bad_input();
}

#[test]
fn existing_target_rejects_ambiguous_persisted_tasks() {
    state::selection::existing_target_rejects_ambiguous_persisted_tasks();
}

#[test]
fn persisted_capacity_is_authoritative_and_schema_version_is_checked() {
    state::migrations::persisted_capacity_is_authoritative_and_schema_version_is_checked();
}

#[test]
fn invalid_config_does_not_create_task_or_selection_evidence() {
    state::selection::invalid_config_does_not_create_task_or_selection_evidence();
}

#[test]
fn failed_attempt_insert_rolls_back_its_reservation() {
    state::persistence::failed_attempt_insert_rolls_back_its_reservation();
}

#[test]
fn persists_identity_evidence_and_reservations_transactionally() {
    state::persistence::persists_identity_evidence_and_reservations_transactionally();
}

#[test]
fn duplicate_identity_does_not_leave_partial_task() {
    state::selection::duplicate_identity_does_not_leave_partial_task();
}

#[test]
fn reservation_history_allows_a_new_attempt_after_reopen() {
    state::persistence::reservation_history_allows_a_new_attempt_after_reopen();
}

#[test]
fn invalid_selections_leave_no_task_or_evidence_after_reopen() {
    state::selection::invalid_selections_leave_no_task_or_evidence_after_reopen();
}

#[test]
fn optional_source_persists_actual_issue_milestone_after_reopen() {
    state::selection::optional_source_persists_actual_issue_milestone_after_reopen();
}

#[test]
fn configured_milestone_selection_is_persisted() {
    state::selection::configured_milestone_selection_is_persisted();
}

#[test]
fn verified_open_pr_requires_a_terminal_attempt() {
    state::persistence::verified_open_pr_requires_a_terminal_attempt();
}

#[test]
fn audited_telemetry_lost_pr_completion_is_terminal_after_reopen() {
    state::recovery::audited_telemetry_lost_pr_completion_is_terminal_after_reopen();
}

#[test]
fn stopped_prior_attempt_without_absent_pause_lookup_is_not_terminal() {
    state::recovery::stopped_prior_attempt_without_absent_pause_lookup_is_not_terminal();
}

#[test]
fn audited_pr_completion_missing_any_recovery_evidence_is_not_terminal() {
    state::recovery::audited_pr_completion_missing_any_recovery_evidence_is_not_terminal();
}

#[test]
fn exit_lookup_capacity_uses_latest_evidence_only() {
    state::recovery::exit_lookup_capacity_uses_latest_evidence_only();
}
