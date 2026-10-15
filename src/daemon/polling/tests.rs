use super::*;
use std::io;

#[test]
fn continuous_cycle_survives_transient_errors_and_resets_bounded_backoff() {
    let mut cycles = 0;
    let mut waits = Vec::new();
    let result = run(
        false,
        || {
            cycles += 1;
            match cycles {
                1..=6 => Err(StateError::CoordinatorBusy.into()),
                7 => Ok(()),
                8 => Err(io::Error::from(io::ErrorKind::TimedOut).into()),
                _ => Err("fatal fixture integrity error".into()),
            }
        },
        |delay| waits.push(delay.as_secs()),
    );
    assert_eq!(
        result.unwrap_err().to_string(),
        "fatal fixture integrity error"
    );
    assert_eq!(cycles, 9);
    assert_eq!(waits, [30, 60, 120, 240, 300, 300, 30, 30]);
}

#[test]
fn once_reports_transient_failure_without_sleeping_or_retrying() {
    let result = run(
        true,
        || Err(StateError::CoordinatorBusy.into()),
        |_| panic!("once slept"),
    );
    assert!(matches!(
        result.unwrap_err().downcast_ref::<StateError>(),
        Some(StateError::CoordinatorBusy)
    ));
}

#[test]
fn unknown_storage_and_malformed_upstream_errors_are_not_retried() {
    for error in [
        Box::new(StateError::InvalidConfig) as Error,
        Box::new(io::Error::from(io::ErrorKind::PermissionDenied)),
        Box::new(LookupError {
            category: ErrorCategory::Malformed,
            code: "bad-fixture",
            status: None,
        }),
    ] {
        assert!(!transient(error.as_ref()));
    }
    assert!(transient(
        &crate::github::identity::IdentityError::Transient
    ));
    assert!(!transient(&crate::github::identity::IdentityError::Output));
    assert_eq!(
        retry_reason(&StateError::CoordinatorBusy),
        "coordinator busy"
    );
    assert!(transient(&ReconciliationBlocked(
        "startup reconciliation blocked scheduling"
    )));
    assert!(transient(&LookupError {
        category: ErrorCategory::RateLimit,
        code: "rate-limit",
        status: Some(429)
    }));
}
