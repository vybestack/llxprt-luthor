//! Pending retries report their original deadline even after it has passed.
use super::{priority::schedule, *};

#[test]
fn capacity_wait_reports_deadline_and_retry_wins_when_slot_is_released() {
    let mut f = Fixture::new("model-rate-limit", 5);
    f.elapsed("attempt-real", TransientFailure::RateLimit);
    let other = priority::candidate(&f);
    luthor::state::task_records::create_task(&mut f.store, "other", &other, "rev", &f.config)
        .unwrap();
    luthor::state::scheduling::reserve(&mut f.store, "other", "other-attempt").unwrap();
    let report = schedule(&mut f);
    let retry = &report.automatic_retries[0];
    assert_eq!(retry.decision, RetryDecision::CapacityFull);
    assert_eq!(retry.backoff_until_unix_secs, Some(61));
    let json = serde_json::to_value(retry).unwrap();
    assert_eq!(json["decision"], "capacity_full");
    assert_eq!(json["task_id"], "task");
    assert_eq!(json["backoff_until_unix_secs"], 61);
    assert!(report.launched.is_empty());
    assert_eq!(f.ids.0, 0);
    assert_eq!(reservation_count(&f.store).unwrap(), 1);
    assert_eq!(
        luthor::state::scheduling::release_reservation(&mut f.store, "other-attempt").unwrap(),
        1
    );
    luthor::state::task_records::set_task_phase(&mut f.store, "other", "completed").unwrap();
    let report = schedule(&mut f);
    assert_eq!(
        report.automatic_retries[0].decision,
        RetryDecision::Launched
    );
    assert!(
        report.automatic_retries[0]
            .backoff_until_unix_secs
            .is_none()
    );
    assert_eq!(report.launched.len(), 1);
    assert_eq!(report.launched[0].task_id, "task");
    assert_eq!(reservation_count(&f.store).unwrap(), 1);
}

#[test]
fn busy_retry_keeps_priority_without_reserving_capacity_or_consuming_attempts() {
    let mut f = Fixture::new("model-connectivity", 5);
    f.elapsed("attempt-real", TransientFailure::Connectivity);
    let owner = luthor::WorktreeOwner::acquire_existing(f.store.root(), "task").unwrap();
    let report = schedule(&mut f);
    let retry = &report.automatic_retries[0];
    assert_eq!(retry.decision, RetryDecision::WorktreeBusy);
    assert_eq!(retry.backoff_until_unix_secs, Some(61));
    assert!(report.launched.is_empty());
    assert_eq!(f.ids.0, 0);
    assert_eq!(
        luthor::state::task_records::task_count(&f.store).unwrap(),
        1
    );
    assert_eq!(reservation_count(&f.store).unwrap(), 0);
    drop(owner);
    let report = schedule(&mut f);
    assert_eq!(
        report.automatic_retries[0].decision,
        RetryDecision::Launched
    );
    assert_eq!(report.launched.len(), 1);
    assert_eq!(report.launched[0].task_id, "task");
    assert_eq!(reservation_count(&f.store).unwrap(), 1);
}
