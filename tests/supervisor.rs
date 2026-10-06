use luthor::{
    claim::{AssignmentError, AssignmentWriter},
    config::{CommandTemplate, Config, Mapping, Marker, Source},
    coordinator::{
        IdCreator, ScheduleDependencies, SupervisorLauncher, operator_recover_missing_receipt,
        schedule_candidates,
    },
    eligibility::Candidate,
    github::{
        project::{Issue, Page, ProjectItem, ProjectReadError, ProjectReader},
        pull_request::{ErrorCategory, LookupError, PullRequestReader},
    },
    pr_evidence::{ExpectedPrError, expected_for_task},
    state::{
        ExitPrEvidence, PausePrEvidence, PausePrStatus, StateError, StateStore, WorktreeIdentity,
    },
    supervisor::{
        Reconciliation, RecoveryInspection, SupervisorError, ensure_distinct_resume_prompt,
        execute_with_binary, inspect_recovery_quiescence, prepare_initial, prepare_resume,
        reconcile_attempt, request_stop, run_gated_child_with_binary,
        run_gated_child_with_log_writers,
    },
    worktree::ensure_worktree,
};
use std::{fs, io::Cursor, path::Path};
#[cfg(unix)]
use std::{
    io::BufRead,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

mod supervisor {
    use super::*;
    pub(super) mod active_pr;
    pub(super) mod completed_pr;
    pub(super) mod configuration;
    pub(super) mod crashed_supervisor;
    pub(super) mod descendants;
    pub(super) mod dispatch;
    pub(super) mod execution;
    pub(super) mod exit_errors;
    pub(super) mod gate;
    pub(super) mod initial;
    pub(super) mod log_failure;
    pub(super) mod log_writers;
    pub(super) mod operator_absent;
    pub(super) mod operator_matching;
    pub(super) mod operator_refusals;
    pub(super) mod operator_rollback;
    pub(super) mod pr_identity;
    pub(super) mod pr_readers;
    pub(super) mod process_observation;
    pub(super) mod quiescence;
    pub(super) mod receipt_integrity;
    pub(super) mod release_gate;
    pub(super) mod resume_committed;
    pub(super) mod resume_environment;
    pub(super) mod resume_fixture;
    pub(super) mod resume_prompts;
    pub(super) mod scheduler_ports;
    pub(super) mod scheduling;
    pub(super) mod stale_claim;
    pub(super) mod stop;
    pub(super) mod stop_identity;
    pub(super) mod stop_race;
    pub(super) mod stop_races;
}
use configuration::*;
use execution::*;
use log_writers::*;
use pr_readers::*;
use process_observation::*;
use resume_fixture::*;
use scheduler_ports::*;
use stop_race::*;
use supervisor::*;

#[test]
fn initial_prompt_distinguishes_issue_assignee_from_author() {
    initial::initial_prompt_distinguishes_issue_assignee_from_author();
}

#[test]
fn intent_and_slot_survive_restart_and_failed_dispatch_stays_held() {
    initial::intent_and_slot_survive_restart_and_failed_dispatch_stays_held();
}

#[test]
fn unverified_worktree_or_missing_session_cannot_reserve_or_launch() {
    initial::unverified_worktree_or_missing_session_cannot_reserve_or_launch();
}

#[test]
fn branch_switch_before_initial_does_not_reserve_or_launch() {
    initial::branch_switch_before_initial_does_not_reserve_or_launch();
}

#[test]
fn changed_worktree_identity_blocks_before_reservation() {
    initial::changed_worktree_identity_blocks_before_reservation();
}

#[cfg(unix)]
#[test]
fn partial_ready_handshake_times_out_and_keeps_reserved_slot() {
    initial::partial_ready_handshake_times_out_and_keeps_reserved_slot();
}

#[cfg(unix)]
#[test]
fn gate_eof_never_spawns_and_does_not_write_receipt() {
    gate::gate_eof_never_spawns_and_does_not_write_receipt();
}

#[cfg(unix)]
#[test]
fn released_gate_captures_durable_logs_and_receipts_real_exit() {
    gate::released_gate_captures_durable_logs_and_receipts_real_exit();
}

#[cfg(unix)]
#[test]
fn live_worker_log_write_failure_stops_and_holds_both_streams() {
    log_failure::live_worker_log_write_failure_stops_and_holds_both_streams();
}

#[cfg(unix)]
#[test]
fn log_writer_and_evidence_failure_still_stops_registered_child_group() {
    log_failure::log_writer_and_evidence_failure_still_stops_registered_child_group();
}

#[cfg(unix)]
#[test]
fn detached_same_binary_dispatch_records_gate_and_worker_receipt() {
    dispatch::detached_same_binary_dispatch_records_gate_and_worker_receipt();
}

#[test]
fn expected_for_task_uses_selection_mapping_and_verified_worktree() {
    pr_identity::expected_for_task_uses_selection_mapping_and_verified_worktree();
}

#[test]
fn expected_for_task_rejects_changed_login_and_repository_id_lookup_errors() {
    pr_identity::expected_for_task_rejects_changed_login_and_repository_id_lookup_errors();
}

#[cfg(unix)]
#[test]
fn live_running_lost_claim_requests_stop_and_holds_reservation_until_exit() {
    active_pr::live_running_lost_claim_requests_stop_and_holds_reservation_until_exit();
}

#[cfg(unix)]
#[test]
fn live_matching_pr_requests_stop_but_waits_for_exit_and_independent_rechecks() {
    active_pr::live_matching_pr_requests_stop_but_waits_for_exit_and_independent_rechecks();
}

#[cfg(unix)]
#[test]
fn natural_exit_matching_pr_is_proved_and_persisted() {
    completed_pr::natural_exit_matching_pr_is_proved_and_persisted();
}

#[cfg(unix)]
#[test]
fn missing_receipt_cannot_be_overridden_by_matching_open_pr() {
    completed_pr::missing_receipt_cannot_be_overridden_by_matching_open_pr();
}

#[cfg(unix)]
#[test]
fn stopped_exit_matching_pr_is_proved_and_persisted() {
    completed_pr::stopped_exit_matching_pr_is_proved_and_persisted();
}

#[cfg(unix)]
#[test]
fn natural_exit_stale_assignee_is_held_despite_matching_open_pr() {
    stale_claim::natural_exit_stale_assignee_is_held_despite_matching_open_pr();
}

#[cfg(unix)]
#[test]
fn stopped_exit_stale_assignee_is_held_despite_matching_open_pr() {
    stale_claim::stopped_exit_stale_assignee_is_held_despite_matching_open_pr();
}

#[cfg(unix)]
#[test]
fn natural_exit_seven_attends_and_scheduler_dispatches_only_other_issue() {
    scheduling::natural_exit_seven_attends_and_scheduler_dispatches_only_other_issue();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn pause_after_natural_exit_preserves_natural_attention_path() {
    stop_races::pause_after_natural_exit_preserves_natural_attention_path();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn pause_intent_before_natural_exit_before_signal_preserves_attention_path() {
    stop_races::pause_intent_before_natural_exit_before_signal_preserves_attention_path();
}

#[cfg(unix)]
#[test]
fn natural_exit_pr_error_keeps_held_slot_and_evidence() {
    exit_errors::natural_exit_pr_error_keeps_held_slot_and_evidence();
}

#[cfg(unix)]
#[test]
fn uncertain_child_group_never_reads_pr_or_frees_capacity() {
    exit_errors::uncertain_child_group_never_reads_pr_or_frees_capacity();
}

#[cfg(unix)]
#[test]
fn verified_exit_seven_releases_once_and_survives_restart() {
    exit_errors::verified_exit_seven_releases_once_and_survives_restart();
}

#[cfg(unix)]
#[test]
fn live_tracked_descendant_holds_valid_receipt_reconciliation() {
    descendants::live_tracked_descendant_holds_valid_receipt_reconciliation();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn escaped_tracked_descendant_holds_valid_receipt_until_reaped() {
    descendants::escaped_tracked_descendant_holds_valid_receipt_until_reaped();
}

#[cfg(unix)]
#[test]
fn malformed_tracked_identity_holds_valid_receipt_reconciliation() {
    descendants::malformed_tracked_identity_holds_valid_receipt_reconciliation();
}

#[test]
fn malformed_tracked_descendant_evidence_holds_missing_receipt_attempt() {
    descendants::malformed_tracked_descendant_evidence_holds_missing_receipt_attempt();
}

#[cfg(unix)]
#[test]
fn live_tracked_descendant_prevents_missing_receipt_absence_reconciliation() {
    descendants::live_tracked_descendant_prevents_missing_receipt_absence_reconciliation();
}

#[cfg(unix)]
#[test]
fn reaped_supervisor_with_removed_receipt_proves_recovery_quiescence() {
    quiescence::reaped_supervisor_with_removed_receipt_proves_recovery_quiescence();
}

#[cfg(unix)]
#[test]
fn resumed_missing_receipt_accepts_attempt_snapshot_descending_from_original() {
    quiescence::resumed_missing_receipt_accepts_attempt_snapshot_descending_from_original();
}

#[cfg(unix)]
#[test]
fn operator_recovery_missing_owner_proof_holds_before_github() {
    operator_absent::operator_recovery_missing_owner_proof_holds_before_github();
}

#[cfg(unix)]
#[test]
fn operator_recovery_pr_lookup_failure_keeps_slot_reserved() {
    operator_absent::operator_recovery_pr_lookup_failure_keeps_slot_reserved();
}

#[cfg(unix)]
#[test]
fn operator_recovery_absent_pr_records_telemetry_loss_and_releases_slot() {
    operator_absent::operator_recovery_absent_pr_records_telemetry_loss_and_releases_slot();
}

#[cfg(unix)]
#[test]
fn operator_recovery_matching_pr_completes_task_and_persists_proof() {
    operator_matching::operator_recovery_matching_pr_completes_task_and_persists_proof();
}

#[cfg(unix)]
#[test]
fn operator_recovery_release_failure_rolls_back_for_absent_and_matching_pr() {
    operator_rollback::operator_recovery_release_failure_rolls_back_for_absent_and_matching_pr();
}

#[cfg(unix)]
#[test]
fn operator_recovery_does_not_count_nonmatching_pr() {
    operator_refusals::operator_recovery_does_not_count_nonmatching_pr();
}

#[cfg(unix)]
#[test]
fn operator_recovery_rejects_wrong_pr_head_and_author() {
    operator_refusals::operator_recovery_rejects_wrong_pr_head_and_author();
}

#[cfg(unix)]
#[test]
fn operator_recovery_stale_claim_keeps_slot_reserved_without_pr_lookup() {
    operator_refusals::operator_recovery_stale_claim_keeps_slot_reserved_without_pr_lookup();
}

#[cfg(unix)]
#[test]
fn absent_and_corrupt_receipt_keep_reservation() {
    receipt_integrity::absent_and_corrupt_receipt_keep_reservation();
}

#[cfg(unix)]
#[test]
fn plan_without_persisted_launch_intent_keeps_slot() {
    receipt_integrity::plan_without_persisted_launch_intent_keeps_slot();
}

#[cfg(unix)]
#[test]
fn missing_receipt_after_dispatch_before_worker_start_keeps_reservation() {
    receipt_integrity::missing_receipt_after_dispatch_before_worker_start_keeps_reservation();
}

#[cfg(unix)]
#[test]
fn incomplete_or_nonregular_logs_keep_reservation() {
    receipt_integrity::incomplete_or_nonregular_logs_keep_reservation();
}

#[cfg(unix)]
#[test]
fn contradictory_identity_and_malformed_receipts_keep_reservation() {
    receipt_integrity::contradictory_identity_and_malformed_receipts_keep_reservation();
}

#[cfg(unix)]
#[test]
fn reused_child_pid_with_contradictory_start_identity_keeps_slot() {
    receipt_integrity::reused_child_pid_with_contradictory_start_identity_keeps_slot();
}

#[cfg(unix)]
#[test]
fn gate_release_decision_reconciles_without_gate_sent_evidence() {
    receipt_integrity::gate_release_decision_reconciles_without_gate_sent_evidence();
}

#[cfg(unix)]
#[test]
fn missing_child_registration_holds_even_with_valid_receipt() {
    receipt_integrity::missing_child_registration_holds_even_with_valid_receipt();
}

#[cfg(unix)]
#[test]
fn live_child_group_is_not_released() {
    receipt_integrity::live_child_group_is_not_released();
}

#[cfg(unix)]
#[test]
fn branch_switch_after_ready_blocks_release_and_holds_slot() {
    release_gate::branch_switch_after_ready_blocks_release_and_holds_slot();
}

#[cfg(unix)]
#[test]
fn same_binary_ready_without_release_does_not_launch_worker() {
    release_gate::same_binary_ready_without_release_does_not_launch_worker();
}

#[cfg(unix)]
#[test]
fn registered_shim_stays_gated_and_survives_supervisor_crash_after_release() {
    crashed_supervisor::registered_shim_stays_gated_and_survives_supervisor_crash_after_release();
}

#[cfg(unix)]
#[test]
fn spoofed_child_identity_does_not_signal_live_group() {
    stop_identity::spoofed_child_identity_does_not_signal_live_group();
}

#[cfg(unix)]
#[test]
fn matching_supervisor_without_socket_does_not_fallback_to_group_signal() {
    stop_identity::matching_supervisor_without_socket_does_not_fallback_to_group_signal();
}

#[cfg(unix)]
#[test]
fn stop_term_has_durable_intent_and_keeps_slot_until_reconcile() {
    stop::stop_term_has_durable_intent_and_keeps_slot_until_reconcile();
}

#[cfg(unix)]
#[test]
fn stop_escalates_only_on_live_matching_child() {
    stop::stop_escalates_only_on_live_matching_child();
}

#[cfg(unix)]
#[test]
fn absent_supervisor_and_wrong_identity_never_signal_unrelated_process() {
    stop::absent_supervisor_and_wrong_identity_never_signal_unrelated_process();
}

#[cfg(unix)]
#[test]
#[ignore]
fn resume_environment_mismatch_child() {
    resume_environment::resume_environment_mismatch_child();
}

#[cfg(unix)]
#[test]
fn resume_rejects_different_root_environment_before_reservation_in_child_process() {
    resume_environment::resume_rejects_different_root_environment_before_reservation_in_child_process();
}

#[cfg(unix)]
#[test]
fn branch_switch_before_resume_does_not_reserve_or_launch() {
    resume_environment::branch_switch_before_resume_does_not_reserve_or_launch();
}

#[cfg(unix)]
#[test]
fn committed_worker_resumes_same_session_and_worktree_after_verified_stop() {
    resume_committed::committed_worker_resumes_same_session_and_worktree_after_verified_stop();
}

#[cfg(unix)]
#[test]
fn paused_attempt_prepares_distinct_continuation_after_reopen() {
    resume_prompts::paused_attempt_prepares_distinct_continuation_after_reopen();
}

#[cfg(unix)]
#[test]
fn minimal_resume_template_gets_interrupted_worktree_inspection_guidance() {
    resume_prompts::minimal_resume_template_gets_interrupted_worktree_inspection_guidance();
}

#[cfg(unix)]
#[test]
fn resume_rejects_same_final_prompt_but_allows_distinct_rendered_attempt() {
    resume_prompts::resume_rejects_same_final_prompt_but_allows_distinct_rendered_attempt();
}

#[cfg(unix)]
#[test]
fn resume_requires_paused_verified_exit_and_unused_attempt_id() {
    resume_prompts::resume_requires_paused_verified_exit_and_unused_attempt_id();
}

#[cfg(unix)]
#[test]
fn resume_rejects_same_prompt_and_tampered_session_or_inode_before_reservation() {
    resume_prompts::resume_rejects_same_prompt_and_tampered_session_or_inode_before_reservation();
}

#[test]
fn initial_rejects_conflicting_session_and_cwd_arguments_before_reservation() {
    resume_prompts::initial_rejects_conflicting_session_and_cwd_arguments_before_reservation();
}

#[cfg(unix)]
#[test]
fn retry_natural_exit_audits_current_argv_preserves_history_and_reconciles_new_revision() {
    retry_cases::ordinary::retry_natural_exit_audits_current_argv_preserves_history_and_reconciles_new_revision();
}

#[cfg(unix)]
#[test]
fn retry_refusals_never_reserve_or_launch_or_rewrite_selection() {
    retry_cases::ordinary::retry_refusals_never_reserve_or_launch_or_rewrite_selection();
}

#[cfg(unix)]
#[test]
fn retry_unsupported_saved_budget_uses_corrected_template_without_rewriting_old_argv() {
    retry_cases::ordinary::retry_unsupported_saved_budget_uses_corrected_template_without_rewriting_old_argv();
}

#[cfg(unix)]
#[test]
fn historical_startup_exit_revalidation_launches_once_preserving_all_old_rows_and_files() {
    retry_cases::historical::historical_startup_exit_revalidation_launches_once_preserving_all_old_rows_and_files();
}

#[cfg(unix)]
#[test]
fn historical_revalidation_refuses_unknown_runtime_and_live_or_reused_registered_processes() {
    retry_cases::historical::historical_revalidation_refuses_unknown_runtime_and_live_or_reused_registered_processes();
}

#[cfg(unix)]
#[test]
fn historical_exit_handoff_refuses_surviving_groups_even_with_reaped_leaders() {
    retry_cases::historical::historical_exit_handoff_refuses_surviving_groups_even_with_reaped_leaders();
}

#[cfg(unix)]
#[test]
fn historical_handoff_requires_exhaustive_pr_absence_and_rechecks_transaction_state() {
    retry_cases::historical::historical_handoff_requires_exhaustive_pr_absence_and_rechecks_transaction_state();
}

#[cfg(unix)]
mod supervisor_support;
#[cfg(unix)]
use supervisor_support::{
    retry_cases,
    stop_views::{assert_crashed_supervisor_reservation, assert_natural_stop_views_and_restart},
};
