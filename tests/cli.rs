mod cli {
    pub(crate) mod arguments;
    #[cfg(unix)]
    pub(crate) mod dispatch;
    #[cfg(unix)]
    mod harness;
    pub(crate) mod logs;
    pub(crate) mod observations;
    pub(crate) mod output;
    #[cfg(unix)]
    pub(crate) mod process;
    #[cfg(unix)]
    pub(crate) mod resume;
    mod support;
    pub(crate) mod views;
}

#[cfg(unix)]
#[test]
fn recover_requires_execute_before_config_store_or_github_access() {
    cli::arguments::recover_requires_execute_before_config_store_or_github_access();
}

#[test]
fn recover_accepts_documented_syntax_then_fails_before_state_or_github_access() {
    cli::arguments::recover_accepts_documented_syntax_then_fails_before_state_or_github_access();
}

#[test]
fn recover_rejects_extra_argument_before_config_access() {
    cli::arguments::recover_rejects_extra_argument_before_config_access();
}

#[test]
fn show_reports_events_issue_mapping_session_and_receipt_without_secrets() {
    cli::views::show_reports_events_issue_mapping_session_and_receipt_without_secrets();
}

#[test]
fn show_reports_verified_pr_as_cached_and_rejects_malformed_or_duplicate_proof() {
    cli::views::show_reports_verified_pr_as_cached_and_rejects_malformed_or_duplicate_proof();
}

#[test]
fn status_reports_cached_pr_only_for_completed_task_without_gating_on_checks() {
    cli::views::status_reports_cached_pr_only_for_completed_task_without_gating_on_checks();
}

#[test]
fn status_and_show_report_missing_or_unsafe_active_logs_as_unavailable() {
    cli::output::status_and_show_report_missing_or_unsafe_active_logs_as_unavailable();
}

#[test]
fn status_and_show_work_while_coordinator_owns_lock() {
    cli::views::status_and_show_work_while_coordinator_owns_lock();
}

#[test]
fn operator_observations_are_typed_current_and_redacted_in_status_and_show() {
    cli::observations::operator_observations_are_typed_current_and_redacted_in_status_and_show();
}

#[test]
fn malformed_observations_fail_closed_without_exposing_payload() {
    cli::observations::malformed_observations_fail_closed_without_exposing_payload();
}

#[test]
fn status_reports_output_age_and_silence_without_exposing_log_contents() {
    cli::output::status_reports_output_age_and_silence_without_exposing_log_contents();
}

#[test]
fn status_warns_on_durable_attempt_age_with_empty_logs_without_requesting_stop() {
    cli::output::status_warns_on_durable_attempt_age_with_empty_logs_without_requesting_stop();
}

#[test]
fn status_does_not_warn_without_a_valid_reserved_attempt_start() {
    cli::output::status_does_not_warn_without_a_valid_reserved_attempt_start();
}

#[test]
fn logs_reject_foreign_attempt_traversal_and_world_readable_files() {
    cli::logs::logs_reject_foreign_attempt_traversal_and_world_readable_files();
}

#[cfg(unix)]
#[test]
fn logs_reject_symlinks_and_receipt_path_mismatch() {
    cli::logs::logs_reject_symlinks_and_receipt_path_mismatch();
}

#[test]
fn pause_and_reconcile_reject_bad_arguments_and_unknown_tasks() {
    cli::arguments::pause_and_reconcile_reject_bad_arguments_and_unknown_tasks();
}

#[cfg(unix)]
mod resume_cli {
    #[test]
    fn dispatch_execute_holds_unresolved_source_before_project_selection_with_spare_capacity() {
        super::cli::dispatch::dispatch_execute_holds_unresolved_source_before_project_selection_with_spare_capacity();
    }

    #[test]
    fn dispatch_execute_holds_uncertain_attempt_with_spare_capacity() {
        super::cli::dispatch::dispatch_execute_holds_uncertain_attempt_with_spare_capacity();
    }

    #[test]
    fn local_controls_verify_live_child_and_expose_stop_intent_without_secrets() {
        super::cli::process::local_controls_verify_live_child_and_expose_stop_intent_without_secrets();
    }

    #[test]
    fn live_worker_reports_stream_bytes_before_exit() {
        super::cli::process::live_worker_reports_stream_bytes_before_exit();
    }

    #[test]
    fn local_controls_hold_live_child_when_gate_proof_is_missing() {
        super::cli::process::local_controls_hold_live_child_when_gate_proof_is_missing();
    }

    #[test]
    fn local_controls_reject_forged_child_identity_with_gate_evidence() {
        super::cli::process::local_controls_reject_forged_child_identity_with_gate_evidence();
    }

    #[test]
    fn dispatch_execute_admits_second_issue_beside_verified_live_worker() {
        super::cli::dispatch::dispatch_execute_admits_second_issue_beside_verified_live_worker();
    }

    #[test]
    fn dispatch_execute_holds_live_child_without_gate_send_proof() {
        super::cli::dispatch::dispatch_execute_holds_live_child_without_gate_send_proof();
    }

    #[test]
    fn dispatch_execute_clean_store_reaches_eligible_project_candidate() {
        super::cli::dispatch::dispatch_execute_clean_store_reaches_eligible_project_candidate();
    }

    #[test]
    fn dispatch_without_execute_does_not_open_state_or_call_github() {
        super::cli::dispatch::dispatch_without_execute_does_not_open_state_or_call_github();
    }

    #[test]
    fn resume_without_execute_is_held_without_github_or_worker_invocations() {
        super::cli::resume::resume_without_execute_is_held_without_github_or_worker_invocations();
    }

    #[test]
    fn resume_rejects_malformed_and_duplicate_execute_arguments_without_invocation() {
        super::cli::resume::resume_rejects_malformed_and_duplicate_execute_arguments_without_invocation();
    }

    #[test]
    fn resume_unknown_task_fails_before_github_invocation() {
        super::cli::resume::resume_unknown_task_fails_before_github_invocation();
    }

    #[test]
    fn resume_rejects_non_resumable_held_task_without_github_or_worker_invocation() {
        super::cli::resume::resume_rejects_non_resumable_held_task_without_github_or_worker_invocation();
    }

    #[test]
    fn reconcile_prelaunch_claim_uses_only_read_only_gh_and_pause_still_needs_attempt() {
        super::cli::process::reconcile_prelaunch_claim_uses_only_read_only_gh_and_pause_still_needs_attempt();
    }
}
