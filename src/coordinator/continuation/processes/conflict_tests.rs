use super::{Error, Scope, assess_scope, parse_row};
use std::path::{Path, PathBuf};

fn scope() -> Scope<'static> {
    Scope {
        task: "task-selected",
        attempt: "attempt-selected",
        worktree: Path::new("/private/selected"),
        worker: "selected-worker",
    }
}

#[test]
fn active_out_of_band_namespace_or_worktree_conflicts_without_registered_ids() {
    for args in [
        "custom --session task-selected",
        "luthor __supervise root attempt-selected",
        "custom /private/selected",
    ] {
        let line = format!("999999 501 S {args}");
        let row = parse_row(&line).unwrap();
        assert_eq!(
            assess_scope(&scope(), 501, row, |_| panic!("argv conflict needs no cwd")),
            Err(Error::Conflict)
        );
    }
    for cwd in ["/private/selected", "/private/selected/subdir"] {
        let row = parse_row("999999 501 S unrelated-program").unwrap();
        assert_eq!(
            assess_scope(&scope(), 501, row, |_| Ok(PathBuf::from(cwd))),
            Err(Error::Conflict)
        );
    }
}

#[test]
fn unassessable_worker_or_same_user_process_fails_closed() {
    let row = parse_row("999999 502 S selected-worker").unwrap();
    assert_eq!(
        assess_scope(&scope(), 501, row, |_| panic!(
            "worker is already uncertain"
        )),
        Err(Error::Unavailable)
    );
    let row = parse_row("999999 501 S unrelated-program").unwrap();
    assert_eq!(
        assess_scope(&scope(), 501, row, |_| Err(Error::Unavailable)),
        Err(Error::Unavailable)
    );
    let row = parse_row("999999 501 S unrelated-program").unwrap();
    assert_eq!(
        assess_scope(&scope(), 501, row, |_| Ok(PathBuf::from("/other"))),
        Ok(())
    );
}

#[test]
fn zombie_is_not_active_conflict_and_self_is_not_registered_as_worker() {
    let row = parse_row("999999 501 Z task-selected").unwrap();
    assert_eq!(
        assess_scope(&scope(), 501, row, |_| panic!("zombie is inert")),
        Ok(())
    );
    let line = format!("{} 501 S task-selected", std::process::id());
    let row = parse_row(&line).unwrap();
    assert_eq!(
        assess_scope(&scope(), 501, row, |_| panic!("coordinator is exempt")),
        Ok(())
    );
}

#[test]
fn relative_or_deleted_process_cwd_is_not_absence_proof() {
    for path in ["relative", "/private/selected (deleted)"] {
        let row = parse_row("999999 501 S unrelated-program").unwrap();
        assert_eq!(
            assess_scope(&scope(), 501, row, |_| Ok(PathBuf::from(path))),
            Err(Error::Unavailable)
        );
    }
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn os_cwd_adapter_reads_coordinator_and_refuses_unavailable_pid() {
    let expected = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    assert_eq!(
        super::current_directory(std::process::id()).unwrap(),
        expected
    );
    assert_eq!(super::current_directory(u32::MAX), Err(Error::Unavailable));
}
