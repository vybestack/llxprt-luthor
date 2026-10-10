mod claim {
    pub(crate) mod adapters;
    pub(crate) mod assignment;
    pub(crate) mod lookup;
    pub(crate) mod reconciliation;
    mod selectors;
    mod support;
}

#[test]
fn repository_identity_reads_immutable_id_and_rejects_malformed_responses() {
    claim::adapters::repository_identity_reads_immutable_id_and_rejects_malformed_responses();
}

#[test]
fn gh_check_collection_preserves_results_and_failures_for_linked_draft_prs() {
    claim::adapters::gh_check_collection_preserves_results_and_failures_for_linked_draft_prs();
}

#[test]
fn legacy_status_failures_are_advisory_for_linked_open_prs() {
    claim::adapters::legacy_status_failures_are_advisory_for_linked_open_prs();
}

#[test]
fn gh_check_collection_reads_full_page_and_bounded_second_page() {
    claim::adapters::gh_check_collection_reads_full_page_and_bounded_second_page();
}

#[test]
fn gh_detail_preserves_exact_body_for_independent_verification() {
    claim::adapters::gh_detail_preserves_exact_body_for_independent_verification();
}

#[test]
fn exhausts_two_pages_before_returning_absent() {
    claim::lookup::exhausts_two_pages_before_returning_absent();
}

#[test]
fn returns_linked_open_pr_even_when_draft_or_checks_fail() {
    claim::lookup::returns_linked_open_pr_even_when_draft_or_checks_fail();
}

#[test]
fn reports_multiple_exact_links_as_ambiguous() {
    claim::lookup::reports_multiple_exact_links_as_ambiguous();
}

#[test]
fn requires_exact_issue_url_and_whole_line() {
    claim::lookup::requires_exact_issue_url_and_whole_line();
}

#[test]
fn malformed_details_and_page_failures_never_become_absent() {
    claim::lookup::malformed_details_and_page_failures_never_become_absent();
}

#[test]
fn failed_later_page_is_not_reported_as_absent() {
    claim::lookup::failed_later_page_is_not_reported_as_absent();
}

#[test]
fn verified_claim_assigns_once() {
    claim::assignment::verified_claim_assigns_once();
}

#[test]
fn optional_source_claims_persisted_issue_milestone_after_reopen() {
    claim::assignment::optional_source_claims_persisted_issue_milestone_after_reopen();
}

#[test]
fn changed_optional_issue_milestone_refuses_claim_before_assignment() {
    claim::assignment::changed_optional_issue_milestone_refuses_claim_before_assignment();
}

#[test]
fn later_project_page_failure_prevents_assignment_and_verification() {
    claim::assignment::later_project_page_failure_prevents_assignment_and_verification();
}

#[test]
fn duplicate_issue_on_later_project_page_prevents_assignment_and_verification() {
    claim::assignment::duplicate_issue_on_later_project_page_prevents_assignment_and_verification();
}

#[test]
fn held_on_ambiguous_assignment_never_reissues_write() {
    claim::assignment::held_on_ambiguous_assignment_never_reissues_write();
}

#[test]
fn preexisting_linked_pr_prevents_claim() {
    claim::assignment::preexisting_linked_pr_prevents_claim();
}

#[test]
fn postwrite_project_error_holds_intent() {
    claim::assignment::postwrite_project_error_holds_intent();
}

#[test]
fn optional_source_reconcile_compares_persisted_milestone_identity() {
    claim::reconciliation::optional_source_reconcile_compares_persisted_milestone_identity();
}

#[test]
fn source_reconcile_observes_claim_intent_without_attempt_or_assignment() {
    claim::reconciliation::source_reconcile_observes_claim_intent_without_attempt_or_assignment();
}

#[test]
fn source_reconcile_partial_worktree_and_source_errors_stay_held() {
    claim::reconciliation::source_reconcile_partial_worktree_and_source_errors_stay_held();
}

#[test]
fn fully_recorded_source_without_attempt_still_blocks_new_selection() {
    claim::reconciliation::fully_recorded_source_without_attempt_still_blocks_new_selection();
}

#[test]
fn assignment_observes_durable_claim_intent_before_external_write() {
    claim::assignment::assignment_observes_durable_claim_intent_before_external_write();
}
