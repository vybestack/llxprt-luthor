use super::linux::{collect, read_leaf};
use std::{cell::Cell, io, os::unix::fs::symlink};

fn stat(pid: u32, start: &str) -> Vec<u8> {
    format!(
        "{pid} (odd ) name) S {} {start} 0\n",
        vec!["0"; 18].join(" ")
    )
    .into_bytes()
}

fn metadata(comm: &[u8], status: &[u8], exe: &[u8]) -> String {
    collect(123, |leaf| {
        Ok(match leaf {
            "stat" => stat(123, "123456"),
            "comm" => comm.to_vec(),
            "status" => status.to_vec(),
            "exe" => exe.to_vec(),
            _ => panic!("unexpected proc leaf"),
        })
    })
}

#[test]
fn only_bounded_names_and_allowlisted_numeric_status_survive() {
    let record = metadata(
        b"Runner.Worker\n",
        b"Name:\tsecret-token\nUid:\t1001 1001 1001 1001\nGid:\t1 2 3 4\nTracerPid:\t0\nNoNewPrivs:\t1\nSeccomp:\t2\nSecret:\t/sensitive\n",
        b"Runner.Worker",
    );
    for expected in [
        "comm=Runner.Worker",
        "starttime=123456",
        "Uid=1001,1001,1001,1001",
        "Gid=1,2,3,4",
        "TracerPid=0",
        "NoNewPrivs=1",
        "Seccomp=2",
        "exe=Runner.Worker",
    ] {
        assert!(record.contains(expected), "{record}");
    }
    assert!(!record.contains("secret"));
    assert!(!record.contains("/sensitive"));
    assert_eq!(record.lines().count(), 1);
    assert!(record.len() < 800);
}

#[test]
fn untrusted_names_are_redacted_whole_without_secret_fragments() {
    for name in [
        b"worker secret-token\n".as_slice(),
        b"/secret/token",
        b"token=secret",
        b"worker\nsecret",
        b"worker\x1bsecret",
        b"worker\xffsecret",
        b"..",
        b"long-secret-token-name-that-must-not-survive-and-exceeds-even-the-exe-bound",
    ] {
        let record = metadata(name, b"Uid: 1 1 1 1\n", name);
        assert!(record.contains("comm=redacted"), "{record}");
        assert!(!record.contains("secret"));
        assert!(!record.contains('/'));
        assert_eq!(record.lines().count(), 1);
    }
    let record = metadata(b"0123456789012345\n", b"", &[b'a'; 65]);
    assert!(record.contains("comm=redacted"));
    assert!(record.contains("exe=redacted"));
}

#[test]
fn missing_proc_files_report_only_numeric_errors() {
    let record = collect(123, |_| Err(io::Error::from_raw_os_error(libc::EACCES)));
    for key in [
        "stat_errno",
        "comm_errno",
        "status_errno",
        "exe_errno",
        "stat_after_errno",
    ] {
        assert!(
            record.contains(&format!("{key}={}", libc::EACCES)),
            "{record}"
        );
    }
    assert!(record.contains("comm=- starttime=-"));
    assert!(!record.contains("Permission"));
}

#[test]
fn malformed_stat_or_reused_pid_cannot_attribute_other_process_metadata() {
    for malformed in [
        b"secret-token".to_vec(),
        stat(124, "123456"),
        stat(123, "secret-token"),
        stat(123, "18446744073709551616"),
        b"123 (short) S 1".to_vec(),
    ] {
        let record = collect(123, |leaf| {
            Ok(if leaf == "stat" {
                malformed.clone()
            } else {
                b"worker".to_vec()
            })
        });
        assert!(
            record.contains(&format!("stat_errno={}", libc::EINVAL)),
            "{record}"
        );
        assert!(!record.contains("comm=worker"));
        assert!(!record.contains("secret"));
    }
    let calls = Cell::new(0);
    let record = collect(123, |leaf| {
        Ok(if leaf == "stat" {
            calls.set(calls.get() + 1);
            stat(123, if calls.get() == 1 { "1" } else { "2" })
        } else {
            b"worker".to_vec()
        })
    });
    assert!(
        record.contains(&format!("identity_errno={}", libc::ESTALE)),
        "{record}"
    );
    assert!(record.contains("comm=- starttime=-"));
    assert!(!record.contains("exe=worker"));
}

#[test]
fn malformed_numeric_status_never_transports_external_text() {
    for status in [
        b"Uid: 1 2 secret-token 4\n".as_slice(),
        b"Gid: 1 2 3\n",
        b"Uid: 1 2 3 4\nUid: 5 6 7 8\n",
        b"Seccomp: 2 secret-token\n",
        b"TracerPid: 4294967296\n",
        b"NoNewPrivs: -1\n",
        b"Name: \xffsecret\n",
    ] {
        let record = metadata(b"worker\n", status, b"worker");
        assert!(record.contains("status_errno="));
        assert!(!record.contains("secret"));
        assert!(
            record.contains("Uid=- Gid=- TracerPid=- NoNewPrivs=- Seccomp=-"),
            "{record}"
        );
    }
}

#[test]
fn bounded_reads_and_exe_basename_never_expose_parent_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("comm"), vec![b'x'; 65]).unwrap();
    assert_eq!(
        read_leaf(dir.path(), "comm").unwrap_err().raw_os_error(),
        Some(libc::EFBIG)
    );
    symlink("/private/secret-token/worker", dir.path().join("exe")).unwrap();
    assert_eq!(read_leaf(dir.path(), "exe").unwrap(), b"worker");
    assert_eq!(
        read_leaf(dir.path(), "status").unwrap_err().raw_os_error(),
        Some(libc::ENOENT)
    );
}

#[test]
fn opt_out_and_non_cwd_failures_do_not_read_metadata_and_first_cwd_wins() {
    use super::Probe;
    let disabled = Probe::new(false);
    disabled.cwd_failure_with(123, Some(libc::EACCES), || panic!("opt-out read"));
    assert!(disabled.take().is_none());
    let other = Probe::new(true);
    other.failed("ps_spawn", None, Some(libc::EIO));
    other.cwd_failure_with(123, Some(libc::EACCES), || panic!("later failure read"));
    assert!(!other.take().unwrap().render().contains("comm="));
    let first = Probe::new(true);
    first.cwd_failure_with(123, Some(libc::EACCES), || {
        metadata(b"worker", b"", b"worker")
    });
    first.row(Some(123), Some(1001), Some('S'));
    first.cwd_failure_with(124, Some(libc::ENOENT), || panic!("second process read"));
    first.row(Some(124), Some(1002), Some('R'));
    first.cwd_failure_with(123, Some(libc::ENOENT), || panic!("duplicate read"));
    let record = first.take().unwrap().render();
    assert!(record.contains("pid=123 uid=1001 state=S"));
    assert!(record.contains(&format!("errno={}", libc::EACCES)));
    assert!(record.contains("comm=worker starttime=123456"));
    assert!(record.len() < 1024);
}

#[test]
fn complete_maximum_record_fits_fixture_transport_without_truncation() {
    let status = b"Uid: 4294967295 4294967295 4294967295 4294967295\nGid: 4294967295 4294967295 4294967295 4294967295\nTracerPid: 4294967295\nNoNewPrivs: 4294967295\nSeccomp: 4294967295\n";
    let probe = super::Probe::new(true);
    probe.cwd_failure_with(u32::MAX, Some(i32::MAX), || {
        collect(u32::MAX, |leaf| {
            Ok(match leaf {
                "stat" => stat(u32::MAX, "18446744073709551615"),
                "comm" => vec![b'a'; 15],
                "status" => status.to_vec(),
                "exe" => vec![b'b'; 64],
                _ => panic!("unexpected proc leaf"),
            })
        })
    });
    probe.row(Some(u32::MAX), Some(u32::MAX), Some('S'));
    let record = probe.take().unwrap().render();
    assert!(record.len() < 1024, "{}", record.len());
    assert!(record.ends_with("identity_errno=0\n"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_missing_cwd_keeps_refusal_and_collects_metadata_only_when_enabled() {
    let enabled = super::Probe::new(true);
    assert_eq!(
        super::super::current_directory_observed(u32::MAX, &enabled),
        Err(super::super::Error::Unavailable)
    );
    let record = enabled.take().unwrap().render();
    assert!(record.contains(&format!("errno={}", libc::ENOENT)));
    assert!(record.contains("comm=- starttime=-"));
    assert!(record.contains(&format!("exe_errno={}", libc::ENOENT)));
    let disabled = super::Probe::new(false);
    assert_eq!(
        super::super::current_directory_observed(u32::MAX, &disabled),
        Err(super::super::Error::Unavailable)
    );
    assert!(disabled.take().is_none());
}
