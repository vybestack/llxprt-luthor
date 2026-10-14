use super::*;
use std::{collections::VecDeque, os::unix::process::ExitStatusExt, process::ExitStatus};

fn output(status: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(status << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn transient_reads_retry_three_times_and_preserve_last_error() {
    for failure in [
        output(1, "", "gh: HTTP 500"),
        output(1, "", "gh: HTTP 429"),
        output(1, "", "read: connection reset by peer"),
    ] {
        let mut calls = 0;
        let mut waits = Vec::new();
        let result = read_with(
            || {
                calls += 1;
                Ok(failure.clone())
            },
            |wait| waits.push(wait),
        )
        .unwrap();
        assert!(!result.status.success());
        assert_eq!(calls, 3);
        assert_eq!(
            waits,
            [Duration::from_millis(100), Duration::from_millis(200)]
        );
    }
}

#[test]
fn retry_after_is_honored_and_long_wait_defers_to_next_cycle() {
    for seconds in [2, 60] {
        let mut replies = VecDeque::from([
            output(
                1,
                &format!("HTTP/2.0 429 Too Many Requests\r\nRetry-After: {seconds}\r\n\r\n{{}}"),
                "gh: HTTP 429",
            ),
            output(0, "HTTP/2.0 200 OK\r\n\r\n[]", ""),
        ]);
        let mut waits = Vec::new();
        let result =
            read_with(|| Ok(replies.pop_front().unwrap()), |wait| waits.push(wait)).unwrap();
        if seconds == 2 {
            assert!(result.status.success());
            assert_eq!(result.stdout, b"[]");
            assert_eq!(waits, [Duration::from_secs(2)]);
            assert!(replies.is_empty());
        } else {
            assert!(!result.status.success());
            assert!(waits.is_empty());
            assert_eq!(replies.len(), 1);
        }
    }
    assert_eq!(
        retry_delay("Wed, 21 Oct 2015 07:28:00 GMT"),
        Some(Duration::ZERO)
    );
}

#[test]
fn permanent_errors_and_local_spawn_errors_are_not_retried() {
    for status in [401, 403, 404, 422] {
        let mut calls = 0;
        let result = read_with(
            || {
                calls += 1;
                Ok(output(1, "", &format!("HTTP {status}")))
            },
            |_| panic!("no backoff"),
        )
        .unwrap();
        assert!(!result.status.success());
        assert_eq!(calls, 1);
    }
    let mut calls = 0;
    let error = read_with(
        || {
            calls += 1;
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        },
        |_| panic!("no backoff"),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(calls, 1);
}
