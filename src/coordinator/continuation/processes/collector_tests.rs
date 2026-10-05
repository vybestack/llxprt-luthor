use super::{Error, Scope, assess_scope, parse_listing};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn inspect_listing(
    listing: &str,
    collector_pid: u32,
    mut cwd: impl FnMut(u32) -> Result<PathBuf, Error>,
) -> Result<(), Error> {
    let scope = Scope {
        task: "task-selected",
        attempt: "attempt-selected",
        worktree: Path::new("/private/selected"),
        worker: "selected-worker",
    };
    for row in parse_listing(listing, collector_pid)? {
        assess_scope(&scope, 501, row, &mut cwd)?;
    }
    Ok(())
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn completed_collector_row_needs_no_cwd_observation() {
    let child = Command::new("/bin/sh")
        .args([
            "-c",
            "printf '%s 501 S /bin/ps -ww -axo pid=,uid=,stat=,args=\n' \"$$\"",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let collector_pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let listing = std::str::from_utf8(&output.stdout).unwrap();
    assert_eq!(super::parse_row(listing.trim()).unwrap().pid, collector_pid);
    assert_eq!(
        inspect_listing(listing, collector_pid, |_| Err(Error::Unavailable)),
        Ok(())
    );
}

#[test]
fn only_exact_collector_pid_is_excluded_not_other_ps_processes() {
    let listing = "999998 501 S /bin/ps -ww\n999999 501 S /bin/ps -ww\n";
    let mut observed = Vec::new();
    assert_eq!(
        inspect_listing(listing, 999998, |pid| {
            observed.push(pid);
            Err(Error::Unavailable)
        }),
        Err(Error::Unavailable)
    );
    assert_eq!(observed, [999999]);
}

#[test]
fn unrelated_inaccessible_same_uid_process_remains_unavailable() {
    let listing = "999998 501 S /bin/ps -ww\n999999 501 S unrelated-program\n";
    let mut observed = Vec::new();
    assert_eq!(
        inspect_listing(listing, 999998, |pid| {
            observed.push(pid);
            Err(Error::Unavailable)
        }),
        Err(Error::Unavailable)
    );
    assert_eq!(observed, [999999]);
}

#[test]
fn collector_exclusion_does_not_hide_empty_or_malformed_listings() {
    for listing in [
        "",
        "999998 501 S \n",
        "999998 invalid S /bin/ps\n",
        "999998 501 S /bin/ps\nmalformed\n",
        "malformed\n999998 501 S /bin/ps\n",
    ] {
        assert_eq!(
            inspect_listing(listing, 999998, |_| panic!("parse before assessment")),
            Err(Error::Unavailable),
            "{listing}"
        );
    }
}

#[test]
fn other_rows_keep_namespace_conflicts_and_worker_identity_uncertainty() {
    for (args, expected) in [
        ("custom --session task-selected", Error::Conflict),
        ("luthor __supervise attempt-selected", Error::Conflict),
        ("custom /private/selected", Error::Conflict),
        ("selected-worker", Error::Unavailable),
    ] {
        let listing = format!("999998 501 S /bin/ps\n999999 501 S {args}\n");
        assert_eq!(
            inspect_listing(&listing, 999998, |_| panic!("argv decides refusal")),
            Err(expected)
        );
    }
}

#[test]
fn other_rows_keep_worktree_conflicts_and_uncertain_cwd_refusals() {
    let listing = "999998 501 S /bin/ps\n999999 501 S unrelated-program\n";
    for (path, expected) in [
        ("/private/selected", Err(Error::Conflict)),
        ("/private/selected/subdir", Err(Error::Conflict)),
        ("relative", Err(Error::Unavailable)),
        ("/private/selected (deleted)", Err(Error::Unavailable)),
        ("/other", Ok(())),
    ] {
        assert_eq!(
            inspect_listing(listing, 999998, |pid| {
                assert_eq!(pid, 999999);
                Ok(PathBuf::from(path))
            }),
            expected
        );
    }
}
