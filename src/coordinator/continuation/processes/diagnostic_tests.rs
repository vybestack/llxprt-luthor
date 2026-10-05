use super::{Error, Probe, Scope, assess_scope_observed, parse_listing_observed};
use std::path::Path;

fn scope() -> Scope<'static> {
    Scope {
        task: "task-selected",
        attempt: "attempt-selected",
        worktree: Path::new("/private/selected"),
        worker: "selected-worker",
    }
}

fn capture(run: impl FnOnce(&Probe)) -> Option<String> {
    let probe = Probe::new(true);
    run(&probe);
    probe.take().map(|record| record.render())
}

#[test]
fn refused_same_uid_process_records_only_observation_metadata() {
    let record = capture(|probe| {
        let row = super::parse_row("999999 501 Sl unrelated secret-token=/sensitive").unwrap();
        assert_eq!(
            assess_scope_observed(
                &scope(),
                501,
                row,
                |pid| {
                    probe.failed("proc_cwd", Some(pid), Some(libc::EACCES));
                    Err(Error::Unavailable)
                },
                probe
            ),
            Err(Error::Unavailable)
        );
    })
    .unwrap();
    assert_eq!(
        record,
        format!(
            "process_probe stage=proc_cwd pid=999999 uid=501 state=S errno={} code=-\n",
            libc::EACCES
        )
    );
    assert!(!record.contains("secret"));
    assert!(!record.contains("/sensitive"));
}

#[test]
fn exact_collector_is_skipped_without_a_failure_record() {
    assert!(
        capture(|probe| {
            let rows = parse_listing_observed("999998 501 S /bin/ps secret-token\n", 999998, probe)
                .unwrap();
            assert!(rows.is_empty());
        })
        .is_none()
    );
}

#[test]
fn parser_failure_keeps_numeric_fields_but_never_external_text() {
    for (line, expected) in [
        ("123 invalid S secret-token", "pid=123 uid=- state=S"),
        ("123 501 S ", "pid=123 uid=501 state=S"),
        ("malformed secret-token /sensitive", "pid=- uid=- state=-"),
        ("0 501 S /sensitive", "pid=0 uid=501 state=S"),
    ] {
        let record =
            capture(|probe| assert!(parse_listing_observed(line, 999998, probe).is_err())).unwrap();
        assert!(record.contains("stage=row_parser"), "{record}");
        assert!(record.contains(expected), "{record}");
        assert!(!record.contains("secret"));
        assert!(!record.contains("/sensitive"));
        assert!(record.len() < 160);
        assert_eq!(record.lines().count(), 1);
    }
}

#[test]
fn argv_refusal_and_cwd_validation_do_not_disclose_sensitive_values() {
    for (line, stage) in [
        ("999999 501 S selected-worker secret-token", "argv_match"),
        ("999999 501 S task-selected secret-token", "argv_match"),
        ("999999 501 S unrelated secret-token", "proc_cwd"),
    ] {
        let record = capture(|probe| {
            let row = super::parse_row(line).unwrap();
            assert!(
                assess_scope_observed(
                    &scope(),
                    501,
                    row,
                    |_| Ok("/secret (deleted)".into()),
                    probe
                )
                .is_err()
            );
        })
        .unwrap();
        assert!(record.contains(&format!("stage={stage}")), "{record}");
        assert!(!record.contains("secret"));
        assert!(!record.contains("selected"));
    }
}

#[test]
fn first_failure_wins_and_disabled_probe_collects_nothing() {
    let record = capture(|probe| {
        probe.failed("ps_spawn", None, Some(libc::ENOENT));
        probe.failed("ps_wait", Some(123), Some(libc::EIO));
    })
    .unwrap();
    assert!(record.contains("stage=ps_spawn"));
    assert!(!record.contains("ps_wait"));
    let probe = Probe::new(false);
    probe.failed("ps_spawn", None, Some(libc::ENOENT));
    assert!(probe.take().is_none());
}

#[test]
fn cwd_os_error_records_numeric_errno_without_path() {
    let record = capture(|probe| {
        assert_eq!(
            super::current_directory_observed(u32::MAX, probe),
            Err(Error::Unavailable)
        );
    })
    .unwrap();
    assert!(record.contains("stage=proc_cwd pid=4294967295"));
    assert!(!record.contains('/'));
}

#[test]
fn status_and_utf8_failures_are_bounded_without_collector_output() {
    let status = capture(|probe| {
        probe.status(Some(17));
        probe.row(Some(123), None, None);
    })
    .unwrap();
    assert_eq!(
        status,
        "process_probe stage=ps_status pid=123 uid=- state=- errno=- code=17\n"
    );
    let utf8 = capture(|probe| probe.failed("ps_utf8", Some(123), None)).unwrap();
    assert_eq!(
        utf8,
        "process_probe stage=ps_utf8 pid=123 uid=- state=- errno=- code=-\n"
    );
}
